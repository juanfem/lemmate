//! Sign-in through an OpenID Connect provider (SPEC §11.1): Authelia, Keycloak, Google, or
//! anything else that speaks the authorization-code flow.
//!
//! `GET /api/v1/auth/oidc/start` sends the browser to the provider with a fresh `state`, a
//! `nonce` and a PKCE challenge; the provider sends it back to `/api/v1/auth/oidc/callback`,
//! where the code is exchanged for an ID token and the browser gets an ordinary session cookie.
//! From then on nothing distinguishes the session from a password one.
//!
//! The ID token's signature is not checked. It comes straight from the token endpoint over TLS,
//! in answer to a request this server made, and OIDC Core §3.1.3.7 (6) allows TLS server
//! validation in place of the signature in exactly that case. Everything else is checked:
//! issuer, audience, expiry and nonce. For the same reason the issuer must be `https` — plain
//! `http` is accepted only on a loopback address, which is what the tests use.
//!
//! Who gets in: an identity already tied to an account signs into it; a new identity whose
//! *verified* email matches an account is tied to that account (which is how an admin-made or
//! password account moves over); otherwise an account is created when an uninvited visitor
//! could register — the server is empty, or registration is open — or when the sign-in began on
//! an invite link.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::{Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::{Router, routing::get};
use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use lemmate_core::store::{UserRow, now_ms};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tracing::{info, warn};

use crate::app::AppState;
use crate::auth::{self, AuthMode};

/// How long a sign-in may take between leaving for the provider and coming back.
const PENDING_TTL: Duration = Duration::from_secs(10 * 60);
/// Unfinished sign-ins kept at most. `/start` is unauthenticated, so this bounds what a flood
/// of it can hold in memory; the oldest go first.
const MAX_PENDING: usize = 10_000;
/// Clock skew tolerated on `exp`.
const SKEW_MS: i64 = 60_000;

#[derive(Debug, Clone)]
pub struct OidcConfig {
    /// The issuer URL, exactly as the provider's discovery document states it.
    pub issuer: String,
    pub client_id: String,
    /// Sent with HTTP Basic (`client_secret_basic`); none for a public client, which PKCE covers.
    pub client_secret: Option<String>,
    /// This server's callback as the provider knows it: `<public url>/api/v1/auth/oidc/callback`.
    pub redirect_url: String,
    /// What the sign-in button calls the provider.
    pub display_name: String,
    /// Space-separated; must include `openid`.
    pub scopes: String,
}

impl OidcConfig {
    /// Refuse a configuration that could not work, or could not be trusted, at startup rather
    /// than at the first sign-in.
    pub fn validate(&self) -> Result<(), String> {
        let issuer =
            url::Url::parse(&self.issuer).map_err(|e| format!("OIDC issuer {:?}: {e}", self.issuer))?;
        let loopback = matches!(issuer.host(), Some(url::Host::Ipv4(a)) if a.is_loopback())
            || matches!(issuer.host(), Some(url::Host::Ipv6(a)) if a.is_loopback())
            || issuer.host_str() == Some("localhost");
        if issuer.scheme() != "https" && !(issuer.scheme() == "http" && loopback) {
            return Err(format!("OIDC issuer must be https (got {})", self.issuer));
        }
        let redirect = url::Url::parse(&self.redirect_url)
            .map_err(|e| format!("OIDC redirect URL {:?}: {e}", self.redirect_url))?;
        if !matches!(redirect.scheme(), "http" | "https") {
            return Err(format!("OIDC redirect URL must be http(s) (got {})", self.redirect_url));
        }
        if self.client_id.trim().is_empty() {
            return Err("OIDC client id is empty".into());
        }
        if !self.scopes.split_whitespace().any(|s| s == "openid") {
            return Err("OIDC scopes must include openid".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
struct Discovery {
    issuer: String,
    authorization_endpoint: String,
    token_endpoint: String,
    #[serde(default)]
    userinfo_endpoint: Option<String>,
}

struct Pending {
    nonce: String,
    verifier: String,
    invite: Option<String>,
    /// Where to send the browser once signed in (`local_path`).
    next: Option<String>,
    created: Instant,
}

/// The OIDC client: configuration, the provider's discovery document once fetched, and the
/// sign-ins currently away at the provider.
pub struct Oidc {
    config: OidcConfig,
    agent: ureq::Agent,
    discovery: tokio::sync::Mutex<Option<Arc<Discovery>>>,
    pending: std::sync::Mutex<HashMap<String, Pending>>,
}

impl Oidc {
    pub fn new(config: OidcConfig) -> Self {
        let agent = ureq::Agent::new_with_config(
            ureq::Agent::config_builder()
                .http_status_as_error(false)
                .timeout_global(Some(Duration::from_secs(15)))
                .build(),
        );
        Self { config, agent, discovery: tokio::sync::Mutex::new(None), pending: Default::default() }
    }

    pub fn display_name(&self) -> &str {
        &self.config.display_name
    }

    /// The provider's discovery document, fetched on first use and kept. Lazily, so that a
    /// server starting before its provider (one `docker compose up`) still starts.
    async fn discovery(&self) -> Result<Arc<Discovery>, String> {
        let mut cached = self.discovery.lock().await;
        if let Some(d) = cached.as_ref() {
            return Ok(d.clone());
        }
        let url = format!("{}/.well-known/openid-configuration", self.config.issuer.trim_end_matches('/'));
        let agent = self.agent.clone();
        let doc: Discovery = tokio::task::spawn_blocking(move || get_json(&agent, &url, None))
            .await
            .map_err(|e| e.to_string())??;
        if doc.issuer.trim_end_matches('/') != self.config.issuer.trim_end_matches('/') {
            return Err(format!("the provider calls itself {}, not {}", doc.issuer, self.config.issuer));
        }
        let doc = Arc::new(doc);
        *cached = Some(doc.clone());
        Ok(doc)
    }

    fn remember(&self, state: String, pending: Pending) {
        let mut map = self.pending.lock().expect("pending sign-ins");
        map.retain(|_, p| p.created.elapsed() < PENDING_TTL);
        if map.len() >= MAX_PENDING
            && let Some(oldest) = map.iter().min_by_key(|(_, p)| p.created).map(|(k, _)| k.clone())
        {
            map.remove(&oldest);
        }
        map.insert(state, pending);
    }

    /// Take a pending sign-in back; each `state` works once.
    fn redeem(&self, state: &str) -> Option<Pending> {
        let p = self.pending.lock().expect("pending sign-ins").remove(state)?;
        (p.created.elapsed() < PENDING_TTL).then_some(p)
    }
}

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/v1/auth/oidc/start", get(start))
        .route("/api/v1/auth/oidc/callback", get(callback))
}

#[derive(Deserialize)]
struct StartParams {
    /// A registration invite (the link's token, or the whole link) to spend if this identity
    /// needs an account.
    #[serde(default)]
    invite: Option<String>,
    /// A page of this site to come back to after signing in, e.g. a render opened without a
    /// session. Anything but a local path is ignored.
    #[serde(default)]
    next: Option<String>,
}

/// `next` if it is a path on this site — `/…`, never `//host/…` or anything a browser could
/// read as another origin — so the parameter cannot turn sign-in into an open redirect.
fn local_path(next: &str) -> Option<String> {
    let ok = next.starts_with('/')
        && !next.starts_with("//")
        && !next.contains('\\')
        && !next.chars().any(|c| c.is_control() || c.is_whitespace());
    ok.then(|| next.to_owned())
}

async fn start(State(state): State<Arc<AppState>>, Query(p): Query<StartParams>) -> Response {
    let Some(oidc) = state.oidc.as_ref() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let discovery = match oidc.discovery().await {
        Ok(d) => d,
        Err(e) => {
            warn!(%e, "OIDC discovery");
            return (StatusCode::BAD_GATEWAY, format!("the identity provider is not reachable: {e}"))
                .into_response();
        }
    };
    let Ok(mut url) = url::Url::parse(&discovery.authorization_endpoint) else {
        return (StatusCode::BAD_GATEWAY, "the identity provider's authorization endpoint is not a URL")
            .into_response();
    };
    let csrf = auth::new_token();
    let nonce = auth::new_token();
    let verifier = auth::new_token(); // 64 hex digits: within PKCE's 43–128 unreserved characters
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", &oidc.config.client_id)
        .append_pair("redirect_uri", &oidc.config.redirect_url)
        .append_pair("scope", &oidc.config.scopes)
        .append_pair("state", &csrf)
        .append_pair("nonce", &nonce)
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256");
    let invite = p.invite.map(|i| lemmate_core::credentials::invite_token(&i)).filter(|t| !t.is_empty());
    let next = p.next.as_deref().and_then(local_path);
    oidc.remember(csrf, Pending { nonce, verifier, invite, next, created: Instant::now() });
    Redirect::to(url.as_str()).into_response()
}

#[derive(Deserialize)]
struct CallbackParams {
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    error_description: Option<String>,
}

/// Back to the app with the reason a sign-in failed, for the sign-in page to show.
fn fail(msg: &str) -> Response {
    warn!(reason = msg, "OIDC sign-in refused");
    let mut to = url::Url::parse("http://x/").expect("static");
    to.query_pairs_mut().append_pair("signin_error", msg);
    Redirect::to(&format!("/?{}", to.query().unwrap_or_default())).into_response()
}

async fn callback(State(state): State<Arc<AppState>>, Query(p): Query<CallbackParams>) -> Response {
    let Some(oidc) = state.oidc.as_ref() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if let Some(e) = p.error {
        return fail(&format!("the identity provider said: {}", p.error_description.unwrap_or(e)));
    }
    let (Some(code), Some(csrf)) = (p.code, p.state) else {
        return fail("the identity provider sent no code");
    };
    let Some(pending) = oidc.redeem(&csrf) else {
        return fail("that sign-in expired or was already used; try again");
    };
    let discovery = match oidc.discovery().await {
        Ok(d) => d,
        Err(e) => return fail(&format!("the identity provider is not reachable: {e}")),
    };
    let claims = {
        let agent = oidc.agent.clone();
        let config = oidc.config.clone();
        let discovery = discovery.clone();
        let verifier = pending.verifier.clone();
        match tokio::task::spawn_blocking(move || exchange(&agent, &config, &discovery, &code, &verifier))
            .await
        {
            Ok(Ok(c)) => c,
            Ok(Err(e)) => return fail(&e),
            Err(e) => return fail(&e.to_string()),
        }
    };
    if let Err(e) = check_claims(&claims, &discovery.issuer, &oidc.config.client_id, &pending.nonce, now_ms())
    {
        return fail(&e);
    }
    let user = match resolve_user(&state, &discovery.issuer, &claims, pending.invite.as_deref()).await {
        Ok(u) => u,
        Err(e) => return fail(&e),
    };
    let mut store = state.store.lock().await;
    let token = match auth::issue_session(&mut store, &user, Some("browser (OIDC)")).await {
        Ok(t) => t,
        Err(_) => return fail("could not start a session"),
    };
    drop(store);
    info!(user = %user.email, "signed in through OIDC");
    let mut resp = Redirect::to(pending.next.as_deref().unwrap_or("/")).into_response();
    if let Ok(v) = HeaderValue::from_str(&auth::session_cookie(&state, &token)) {
        resp.headers_mut().insert(header::SET_COOKIE, v);
    }
    resp
}

/// The claims this server uses, from the ID token and, where it is silent, the userinfo
/// endpoint (Authelia, for one, keeps email out of the ID token by default).
#[derive(Debug, Clone, Default, Deserialize)]
struct Claims {
    #[serde(default)]
    iss: String,
    #[serde(default)]
    sub: String,
    #[serde(default)]
    aud: Audience,
    #[serde(default)]
    azp: Option<String>,
    #[serde(default)]
    exp: i64,
    #[serde(default)]
    nonce: Option<String>,
    #[serde(default)]
    email: Option<String>,
    #[serde(default, deserialize_with = "boolish")]
    email_verified: bool,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    preferred_username: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(untagged)]
enum Audience {
    One(String),
    Many(Vec<String>),
    #[default]
    None,
}

impl Audience {
    fn contains(&self, id: &str) -> bool {
        match self {
            Self::One(a) => a == id,
            Self::Many(v) => v.iter().any(|a| a == id),
            Self::None => false,
        }
    }
    fn len(&self) -> usize {
        match self {
            Self::One(_) => 1,
            Self::Many(v) => v.len(),
            Self::None => 0,
        }
    }
}

/// Some providers (older Cognito, a few others) send `email_verified` as the string "true".
fn boolish<'de, D: serde::Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
    Ok(match serde_json::Value::deserialize(d)? {
        serde_json::Value::Bool(b) => b,
        serde_json::Value::String(s) => s.eq_ignore_ascii_case("true"),
        _ => false,
    })
}

fn get_json<T: serde::de::DeserializeOwned>(
    agent: &ureq::Agent,
    url: &str,
    bearer: Option<&str>,
) -> Result<T, String> {
    let mut req = agent.get(url).header("accept", "application/json");
    if let Some(t) = bearer {
        req = req.header("authorization", &format!("Bearer {t}"));
    }
    let mut resp = req.call().map_err(|e| format!("{url}: {e}"))?;
    let status = resp.status();
    let body = resp.body_mut().read_to_string().map_err(|e| format!("{url}: {e}"))?;
    if !status.is_success() {
        return Err(format!("{url}: {status}"));
    }
    serde_json::from_str(&body).map_err(|e| format!("{url}: {e}"))
}

/// Trade the code for tokens, and read the claims out of the ID token (topped up from userinfo).
fn exchange(
    agent: &ureq::Agent,
    config: &OidcConfig,
    discovery: &Discovery,
    code: &str,
    verifier: &str,
) -> Result<Claims, String> {
    let mut form = vec![
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", config.redirect_url.as_str()),
        ("code_verifier", verifier),
    ];
    let mut req = agent.post(&discovery.token_endpoint).header("accept", "application/json");
    match &config.client_secret {
        Some(secret) => {
            // RFC 6749 §2.3.1: both halves form-encoded before they are joined.
            let enc = |s: &str| url::form_urlencoded::byte_serialize(s.as_bytes()).collect::<String>();
            let basic = STANDARD.encode(format!("{}:{}", enc(&config.client_id), enc(secret)));
            req = req.header("authorization", &format!("Basic {basic}"));
        }
        None => form.push(("client_id", config.client_id.as_str())),
    }
    let mut resp = req.send_form(form).map_err(|e| format!("token request: {e}"))?;
    let status = resp.status();
    let body = resp.body_mut().read_to_string().map_err(|e| format!("token request: {e}"))?;
    #[derive(Deserialize)]
    struct TokenResponse {
        id_token: Option<String>,
        access_token: Option<String>,
        error: Option<String>,
        error_description: Option<String>,
    }
    let tokens: TokenResponse =
        serde_json::from_str(&body).map_err(|_| format!("the token endpoint answered {status}"))?;
    if let Some(e) = tokens.error {
        return Err(format!(
            "the identity provider refused the code: {}",
            tokens.error_description.unwrap_or(e)
        ));
    }
    let id_token = tokens.id_token.ok_or("the identity provider sent no ID token")?;
    let mut claims = decode_id_token(&id_token)?;
    if (claims.email.is_none() || !claims.email_verified)
        && let (Some(endpoint), Some(access)) = (&discovery.userinfo_endpoint, &tokens.access_token)
    {
        // Best effort: without it the ID token's claims decide, and say what is missing.
        let info: Claims = get_json(agent, endpoint, Some(access)).unwrap_or_else(|e| {
            warn!(%e, "OIDC userinfo");
            Claims::default()
        });
        // OIDC Core §5.3.2: userinfo about somebody else is to be ignored.
        if !info.sub.is_empty() && info.sub == claims.sub {
            if claims.email.is_none() {
                claims.email = info.email;
                claims.email_verified = info.email_verified;
            } else if claims.email == info.email {
                claims.email_verified |= info.email_verified;
            }
            claims.name = claims.name.or(info.name);
            claims.preferred_username = claims.preferred_username.or(info.preferred_username);
        }
    }
    Ok(claims)
}

/// The payload of a compact JWS, unverified (see the module comment for why that is enough).
fn decode_id_token(jwt: &str) -> Result<Claims, String> {
    let payload = jwt.split('.').nth(1).ok_or("the ID token is not a JWT")?;
    let bytes = URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .map_err(|_| "the ID token is not base64url".to_owned())?;
    serde_json::from_slice(&bytes).map_err(|e| format!("the ID token's claims do not parse: {e}"))
}

fn check_claims(c: &Claims, issuer: &str, client_id: &str, nonce: &str, now: i64) -> Result<(), String> {
    if c.iss.trim_end_matches('/') != issuer.trim_end_matches('/') {
        return Err(format!("the ID token was issued by {}, not {issuer}", c.iss));
    }
    if !c.aud.contains(client_id) || (c.aud.len() > 1 && c.azp.as_deref().is_some_and(|a| a != client_id)) {
        return Err("the ID token is for another client".into());
    }
    if c.exp * 1000 + SKEW_MS < now {
        return Err("the ID token has expired".into());
    }
    if c.nonce.as_deref() != Some(nonce) {
        return Err("the ID token does not answer this sign-in (nonce)".into());
    }
    if c.sub.is_empty() {
        return Err("the ID token names no subject".into());
    }
    Ok(())
}

/// The account an identity signs into, tying or creating it as the module comment describes.
async fn resolve_user(
    state: &AppState,
    issuer: &str,
    c: &Claims,
    invite: Option<&str>,
) -> Result<UserRow, String> {
    let AuthMode::Enabled { allow_registration, .. } = state.options.auth else {
        return Err("this server has no accounts".into());
    };
    // Qualified by the issuer: a `sub` is only unique within its provider.
    let subject = format!("{}#{}", issuer.trim_end_matches('/'), c.sub);
    let internal = |e: lemmate_core::Error| e.to_string();
    let mut store = state.store.lock().await;
    if let Some(u) = store.user_by_oidc_subject(&subject).map_err(internal)? {
        return Ok(u);
    }
    let email = c.email.as_deref().map(|e| e.trim().to_lowercase()).filter(|e| e.contains('@'));
    let Some(email) = email else {
        return Err("the identity provider did not share an email address (ask for the email scope)".into());
    };
    if let Some(existing) = store.user_by_email(&email).map_err(internal)? {
        if !c.email_verified {
            return Err(format!(
                "an account for {email} exists, but the identity provider has not verified that address"
            ));
        }
        if store.oidc_subject_of(&existing.id).map_err(internal)?.is_some() {
            return Err(format!("the account for {email} signs in with a different identity"));
        }
        store.set_oidc_subject(&existing.id, &subject).map_err(internal)?;
        info!(user = %email, "tied an existing account to its OIDC identity");
        return Ok(existing);
    }
    let first = store.user_count().map_err(internal)? == 0;
    let id = lemmate_core::NoteId::new().to_string();
    let name = c
        .name
        .clone()
        .or_else(|| c.preferred_username.clone())
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| email.split('@').next().unwrap_or("user").to_owned());
    if first || allow_registration {
        store.create_user(&id, &email, &name, None, first).map_err(internal)?;
    } else if let Some(invite) = invite {
        if !store
            .create_user_with_invite(&auth::token_hash(invite), &id, &email, &name, None)
            .map_err(internal)?
        {
            return Err("this invite has already been used, expired, or was revoked".into());
        }
    } else {
        return Err(format!("there is no account for {email} here; ask the admin for an invite"));
    }
    store.set_oidc_subject(&id, &subject).map_err(internal)?;
    store.user_by_id(&id).map_err(internal)?.ok_or_else(|| "the new account vanished".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claims(json: serde_json::Value) -> Claims {
        serde_json::from_value(json).unwrap()
    }

    #[test]
    fn configuration_is_checked_up_front() {
        let ok = OidcConfig {
            issuer: "https://auth.example.org".into(),
            client_id: "lemmate".into(),
            client_secret: None,
            redirect_url: "https://notes.example.org/api/v1/auth/oidc/callback".into(),
            display_name: "Authelia".into(),
            scopes: "openid email profile".into(),
        };
        assert!(ok.validate().is_ok());
        assert!(OidcConfig { issuer: "http://127.0.0.1:9000".into(), ..ok.clone() }.validate().is_ok());
        assert!(OidcConfig { issuer: "http://auth.example.org".into(), ..ok.clone() }.validate().is_err());
        assert!(OidcConfig { scopes: "email".into(), ..ok.clone() }.validate().is_err());
        assert!(OidcConfig { client_id: " ".into(), ..ok.clone() }.validate().is_err());
    }

    #[test]
    fn id_token_claims_are_checked() {
        let now = 1_800_000_000_000;
        let good = serde_json::json!({
            "iss": "https://idp", "sub": "42", "aud": "app", "exp": now / 1000 + 300, "nonce": "n1",
        });
        assert!(check_claims(&claims(good.clone()), "https://idp/", "app", "n1", now).is_ok());
        let with = |k: &str, v: serde_json::Value| {
            let mut c = good.clone();
            c[k] = v;
            claims(c)
        };
        assert!(check_claims(&with("iss", "https://evil".into()), "https://idp", "app", "n1", now).is_err());
        assert!(check_claims(&with("aud", "other".into()), "https://idp", "app", "n1", now).is_err());
        assert!(
            check_claims(&with("aud", serde_json::json!(["other", "app"])), "https://idp", "app", "n1", now)
                .is_ok()
        );
        let mut azp = good.clone();
        azp["aud"] = serde_json::json!(["other", "app"]);
        azp["azp"] = "other".into();
        assert!(check_claims(&claims(azp), "https://idp", "app", "n1", now).is_err());
        assert!(
            check_claims(&with("exp", (now / 1000 - 3600).into()), "https://idp", "app", "n1", now).is_err()
        );
        assert!(check_claims(&with("nonce", "n2".into()), "https://idp", "app", "n1", now).is_err());
        assert!(check_claims(&with("sub", "".into()), "https://idp", "app", "n1", now).is_err());
    }

    #[test]
    fn only_local_paths_are_followed_after_sign_in() {
        assert_eq!(
            local_path("/api/v1/vaults/X/notes/Y/render?format=html").as_deref(),
            Some("/api/v1/vaults/X/notes/Y/render?format=html")
        );
        assert_eq!(local_path("/"), Some("/".into()));
        for bad in
            ["//evil.example/", "https://evil.example/", "/\\evil.example", "evil", "/a b", "/a\nb", ""]
        {
            assert_eq!(local_path(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn id_tokens_decode_with_or_without_padding() {
        let payload = URL_SAFE_NO_PAD.encode(br#"{"sub":"7","email_verified":"true","aud":["a"]}"#);
        let c = decode_id_token(&format!("eyJhbGciOiJSUzI1NiJ9.{payload}.sig")).unwrap();
        assert_eq!(c.sub, "7");
        assert!(c.email_verified, "the string form counts");
        assert!(decode_id_token("nope").is_err());
    }
}

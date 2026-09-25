//! OIDC sign-in end to end (SPEC §11.1), against a small in-process provider that does what
//! Authelia or Keycloak would: discovery, a token endpoint that checks the client secret and
//! the PKCE verifier, and a userinfo endpoint.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::extract::{Form, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use lemmate_core::Store;
use lemmate_server::oidc::OidcConfig;
use lemmate_server::{AppState, AuthMode, ServerOptions, build_state, router};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// What the provider will say about whoever logs in next, and what it was asked.
#[derive(Default)]
struct Provider {
    addr: Option<SocketAddr>,
    /// code → (claims to put in the ID token, PKCE challenge it must answer)
    codes: HashMap<String, (Value, String)>,
    /// Claims only the userinfo endpoint gives out.
    userinfo: Value,
}

type Shared = Arc<Mutex<Provider>>;

async fn discovery(State(p): State<Shared>) -> Json<Value> {
    let base = format!("http://{}", p.lock().unwrap().addr.unwrap());
    Json(json!({
        "issuer": base,
        "authorization_endpoint": format!("{base}/authorize"),
        "token_endpoint": format!("{base}/token"),
        "userinfo_endpoint": format!("{base}/userinfo"),
    }))
}

async fn token(
    State(p): State<Shared>,
    headers: HeaderMap,
    Form(f): Form<HashMap<String, String>>,
) -> (StatusCode, Json<Value>) {
    let expected = format!("Basic {}", STANDARD.encode("lemmate:s3cret"));
    if headers.get("authorization").and_then(|v| v.to_str().ok()) != Some(expected.as_str()) {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error": "invalid_client"})));
    }
    let Some((claims, challenge)) = p.lock().unwrap().codes.remove(&f["code"]) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "invalid_grant", "error_description": "unknown code"})),
        );
    };
    if URL_SAFE_NO_PAD.encode(Sha256::digest(f["code_verifier"].as_bytes())) != challenge {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "invalid_grant", "error_description": "PKCE"})),
        );
    }
    let jwt = format!(
        "{}.{}.not-checked",
        URL_SAFE_NO_PAD.encode(br#"{"alg":"RS256"}"#),
        URL_SAFE_NO_PAD.encode(claims.to_string())
    );
    (StatusCode::OK, Json(json!({"id_token": jwt, "access_token": "at", "token_type": "Bearer"})))
}

async fn userinfo(State(p): State<Shared>, headers: HeaderMap) -> (StatusCode, Json<Value>) {
    if headers.get("authorization").and_then(|v| v.to_str().ok()) != Some("Bearer at") {
        return (StatusCode::UNAUTHORIZED, Json(Value::Null));
    }
    (StatusCode::OK, Json(p.lock().unwrap().userinfo.clone()))
}

struct World {
    lemmate: SocketAddr,
    provider: Shared,
    state: Arc<AppState>,
}

async fn world(allow_registration: bool, password_login: bool) -> World {
    let provider: Shared = Arc::default();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let paddr = listener.local_addr().unwrap();
    provider.lock().unwrap().addr = Some(paddr);
    let app = Router::new()
        .route("/.well-known/openid-configuration", get(discovery))
        .route("/token", post(token))
        .route("/userinfo", get(userinfo))
        .with_state(provider.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let lemmate = listener.local_addr().unwrap();
    let options = ServerOptions {
        auth: AuthMode::Enabled { allow_registration, secure_cookies: false },
        password_login,
        oidc: Some(OidcConfig {
            issuer: format!("http://{paddr}"),
            client_id: "lemmate".into(),
            client_secret: Some("s3cret".into()),
            redirect_url: format!("http://{lemmate}/api/v1/auth/oidc/callback"),
            display_name: "Test IdP".into(),
            scopes: "openid email profile".into(),
        }),
        ..ServerOptions::default()
    };
    let state = build_state(Store::open_in_memory().unwrap(), options);
    let app = router(state.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    World { lemmate, provider, state }
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder().http_status_as_error(false).max_redirects(0).build().into()
}

/// GET without following redirects: (status, Location, Set-Cookie).
async fn fetch(url: String, cookie: Option<String>) -> (u16, String, Option<String>, String) {
    tokio::task::spawn_blocking(move || {
        let mut req = agent().get(&url);
        if let Some(c) = cookie {
            req = req.header("cookie", &c);
        }
        let mut r = req.call().unwrap();
        let h = |k: &str| r.headers().get(k).and_then(|v| v.to_str().ok()).map(str::to_owned);
        let (location, cookie) = (h("location").unwrap_or_default(), h("set-cookie"));
        (r.status().as_u16(), location, cookie, r.body_mut().read_to_string().unwrap_or_default())
    })
    .await
    .unwrap()
}

fn query(url: &str) -> HashMap<String, String> {
    url::Url::parse(url).unwrap().query_pairs().into_owned().collect()
}

impl World {
    /// Run one sign-in as the identity `claims` describes (iss/aud/nonce filled in), optionally
    /// through an invite. Returns the callback's redirect target and the session cookie it set.
    async fn sign_in(&self, claims: Value, invite: Option<&str>) -> (String, Option<String>) {
        let start = match invite {
            Some(i) => format!("http://{}/api/v1/auth/oidc/start?invite={i}", self.lemmate),
            None => format!("http://{}/api/v1/auth/oidc/start", self.lemmate),
        };
        let (status, to_provider, _, _) = fetch(start, None).await;
        assert!((300..400).contains(&status), "{status}");
        let q = query(&to_provider);
        assert_eq!(q["client_id"], "lemmate");
        assert_eq!(q["code_challenge_method"], "S256");
        assert_eq!(q["redirect_uri"], format!("http://{}/api/v1/auth/oidc/callback", self.lemmate));
        // The provider authenticates the person and sends them back with a code.
        let mut claims = claims;
        let paddr = self.provider.lock().unwrap().addr.unwrap();
        claims["iss"] = format!("http://{paddr}").into();
        claims["aud"] = "lemmate".into();
        claims["exp"] = (lemmate_core::store::now_ms() / 1000 + 300).into();
        if claims.get("nonce").is_none() {
            claims["nonce"] = q["nonce"].clone().into();
        }
        let code = lemmate_core::NoteId::new().to_string();
        self.provider.lock().unwrap().codes.insert(code.clone(), (claims, q["code_challenge"].clone()));
        let back =
            format!("http://{}/api/v1/auth/oidc/callback?code={code}&state={}", self.lemmate, q["state"]);
        let (status, location, cookie, _) = fetch(back.clone(), None).await;
        assert!((300..400).contains(&status), "{status}");
        // A state works once: replaying the same callback is refused.
        let (_, replay, replay_cookie, _) = fetch(back, None).await;
        assert!(replay.contains("signin_error"), "{replay}");
        assert!(replay_cookie.is_none());
        (location, cookie.map(|c| c.split(';').next().unwrap().to_owned()))
    }

    async fn me(&self, cookie: &str) -> Value {
        let (status, _, _, body) =
            fetch(format!("http://{}/api/v1/auth/me", self.lemmate), Some(cookie.to_owned())).await;
        assert_eq!(status, 200, "{body}");
        serde_json::from_str(&body).unwrap()
    }
}

#[tokio::test]
async fn the_first_identity_becomes_the_admin_and_signs_back_into_the_same_account() {
    let w = world(false, true).await;
    let (s, _, _, body) = fetch(format!("http://{}/api/v1/auth/config", w.lemmate), None).await;
    assert_eq!(s, 200);
    let config: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(config["oidc"], "Test IdP");

    let ann = json!({"sub": "ann-1", "email": "Ann@Example.org", "email_verified": true, "name": "Ann A."});
    let (to, cookie) = w.sign_in(ann.clone(), None).await;
    assert_eq!(to, "/");
    let me = w.me(&cookie.unwrap()).await;
    assert_eq!(me["email"], "ann@example.org");
    assert_eq!(me["display_name"], "Ann A.");
    assert_eq!(me["is_admin"], true);

    // The same subject again is the same account, even if the email moved on.
    let moved = json!({"sub": "ann-1", "email": "ann@new.example", "email_verified": true});
    let (_, cookie) = w.sign_in(moved, None).await;
    assert_eq!(w.me(&cookie.unwrap()).await["id"], me["id"]);
    assert_eq!(w.state.store.lock().await.user_count().unwrap(), 1);
}

#[tokio::test]
async fn a_stranger_needs_open_registration_or_an_invite() {
    let w = world(false, true).await;
    let (_, admin) =
        w.sign_in(json!({"sub": "a", "email": "admin@x.org", "email_verified": true}), None).await;
    let admin = admin.unwrap();

    let bob = json!({"sub": "bob-7", "email": "bob@x.org", "email_verified": true});
    let (to, cookie) = w.sign_in(bob.clone(), None).await;
    assert!(to.starts_with("/?signin_error="), "{to}");
    assert!(query(&format!("http://x{to}"))["signin_error"].contains("invite"), "{to}");
    assert!(cookie.is_none());

    // The admin mints an invite; signing in through it creates Bob, once.
    let invite: Value = tokio::task::spawn_blocking({
        let url = format!("http://{}/api/v1/invites", w.lemmate);
        move || {
            let mut r = agent()
                .post(&url)
                .header("cookie", &admin)
                .header("content-type", "application/json")
                .send("{}")
                .unwrap();
            serde_json::from_str(&r.body_mut().read_to_string().unwrap()).unwrap()
        }
    })
    .await
    .unwrap();
    let link = invite["link"].as_str().unwrap().to_owned(); // "/#/invite/<token>"
    let token = link.rsplit('/').next().unwrap().to_owned();
    let (to, cookie) = w.sign_in(bob, Some(&token)).await;
    assert_eq!(to, "/");
    let me = w.me(&cookie.unwrap()).await;
    assert_eq!(me["email"], "bob@x.org");
    assert_eq!(me["is_admin"], false);
    let carol = json!({"sub": "carol", "email": "carol@x.org", "email_verified": true});
    let (to, _) = w.sign_in(carol, Some(&token)).await;
    assert!(to.contains("signin_error"), "a spent invite: {to}");
}

#[tokio::test]
async fn a_verified_email_ties_an_existing_password_account_and_an_unverified_one_does_not() {
    let w = world(false, true).await;
    // Ann registered with a password before OIDC was set up.
    let (status, body) = tokio::task::spawn_blocking({
        let url = format!("http://{}/api/v1/auth/register", w.lemmate);
        move || {
            let mut r = agent()
                .post(&url)
                .header("content-type", "application/json")
                .send(json!({"email": "ann@x.org", "password": "long enough"}).to_string().as_bytes())
                .unwrap();
            (r.status().as_u16(), r.body_mut().read_to_string().unwrap())
        }
    })
    .await
    .unwrap();
    assert_eq!(status, 200, "{body}");
    let id = serde_json::from_str::<Value>(&body).unwrap()["user"]["id"].clone();

    let (to, _) = w.sign_in(json!({"sub": "s1", "email": "ann@x.org", "email_verified": false}), None).await;
    assert!(to.contains("signin_error") && to.contains("verified"), "{to}");

    // Email and its verification only from userinfo, as Authelia does by default.
    w.provider.lock().unwrap().userinfo = json!({"sub": "s1", "email": "ann@x.org", "email_verified": true});
    let (to, cookie) = w.sign_in(json!({"sub": "s1"}), None).await;
    assert_eq!(to, "/");
    assert_eq!(w.me(&cookie.unwrap()).await["id"], id);

    // Userinfo about someone else is ignored.
    w.provider.lock().unwrap().userinfo =
        json!({"sub": "other", "email": "ann@x.org", "email_verified": true});
    let (to, _) = w.sign_in(json!({"sub": "s2"}), None).await;
    assert!(to.contains("signin_error"), "{to}");
}

#[tokio::test]
async fn a_token_for_another_sign_in_is_refused() {
    let w = world(true, false).await;
    let (to, cookie) =
        w.sign_in(json!({"sub": "x", "email": "x@x.org", "nonce": "not-this-one"}), None).await;
    assert!(to.contains("signin_error") && to.contains("nonce"), "{to}");
    assert!(cookie.is_none());
    // Open registration: anyone the provider vouches for gets in, even with password login off.
    let (to, cookie) = w.sign_in(json!({"sub": "x", "email": "x@x.org"}), None).await;
    assert_eq!(to, "/");
    assert!(cookie.is_some());
}

//! Signing a native app in through the browser (RFC 8252: loopback redirect plus PKCE).
//!
//! The desktop app and `lemmate login --browser` open this server's web client at
//! `/?authorize=app&redirect_uri=…&state=…&code_challenge=…&device=…`. The page signs in however
//! the server signs in — through the identity provider, or with a password — and then asks the
//! person to allow the app. Allowing it is `POST /api/v1/auth/app/approve`, which files a
//! single-use code and answers with the redirect to the app's loopback address; the app trades
//! the code and the verifier behind its challenge at `POST /api/v1/auth/app/token` for a
//! personal access token named after the device, which then lists and revokes like any other.
//!
//! The redirect only ever goes to `http://` on a loopback address, so a code can only reach a
//! program on the machine the browser runs on; the verifier means only the program that started
//! the sign-in can spend it; and it has to be spent within [`CODE_TTL`].

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::State;
use axum::http::StatusCode;
use axum::{Json, Router, routing::post};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::app::AppState;
use crate::auth::{self, AuthMode, AuthUser, TOKEN_PREFIX};

/// How long an approved code may wait for the app to collect it.
pub const CODE_TTL: Duration = Duration::from_secs(120);
/// Codes waiting at once, at most; approving needs a session, so this is only a backstop.
const MAX_CODES: usize = 1_000;

/// One approval, waiting for the app to spend its code.
pub struct Grant {
    user_id: String,
    challenge: String,
    redirect_uri: String,
    device: String,
    created: Instant,
}

#[derive(Default)]
pub struct Grants(std::sync::Mutex<HashMap<String, Grant>>);

impl Grants {
    fn insert(&self, code: String, grant: Grant) {
        let mut map = self.0.lock().expect("grants");
        map.retain(|_, g| g.created.elapsed() < CODE_TTL);
        if map.len() >= MAX_CODES
            && let Some(oldest) = map.iter().min_by_key(|(_, g)| g.created).map(|(k, _)| k.clone())
        {
            map.remove(&oldest);
        }
        map.insert(code, grant);
    }

    /// Take a code back; each works once, spent or not.
    fn take(&self, code: &str) -> Option<Grant> {
        let g = self.0.lock().expect("grants").remove(code)?;
        (g.created.elapsed() < CODE_TTL).then_some(g)
    }
}

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/v1/auth/app/approve", post(approve))
        .route("/api/v1/auth/app/token", post(token))
}

/// `http://` on a loopback address, any port, any path — the only place a code may be sent.
pub fn loopback_redirect(uri: &str) -> bool {
    let Ok(u) = url::Url::parse(uri) else { return false };
    let host_ok = match u.host() {
        Some(url::Host::Ipv4(a)) => a.is_loopback(),
        Some(url::Host::Ipv6(a)) => a.is_loopback(),
        Some(url::Host::Domain(d)) => d == "localhost",
        None => false,
    };
    u.scheme() == "http"
        && host_ok
        && u.username().is_empty()
        && u.password().is_none()
        && u.fragment().is_none()
}

/// The PKCE S256 challenge for a verifier.
pub fn challenge_of(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

#[derive(Deserialize)]
pub struct ApproveIn {
    pub redirect_uri: String,
    pub state: String,
    pub code_challenge: String,
    /// What the app calls itself and its machine, e.g. "Lemmate desktop on juan-pc".
    pub device: String,
}

#[derive(Serialize)]
pub struct ApproveOut {
    /// Where to send the browser: the app's address with `code` and `state` on it.
    pub redirect: String,
}

async fn approve(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Json(body): Json<ApproveIn>,
) -> Result<Json<ApproveOut>, StatusCode> {
    if matches!(state.options.auth, AuthMode::Disabled) {
        return Err(StatusCode::NOT_FOUND);
    }
    // A token handing out tokens is what `/tokens` refuses too.
    if user.token.is_some() {
        return Err(StatusCode::FORBIDDEN);
    }
    let device = body.device.trim();
    let challenge_ok = (43..=128).contains(&body.code_challenge.len())
        && body.code_challenge.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if !loopback_redirect(&body.redirect_uri)
        || !challenge_ok
        || device.is_empty()
        || device.len() > 100
        || body.state.len() > 256
    {
        return Err(StatusCode::BAD_REQUEST);
    }
    let code = auth::new_token();
    let mut redirect = url::Url::parse(&body.redirect_uri).map_err(|_| StatusCode::BAD_REQUEST)?;
    redirect.query_pairs_mut().append_pair("code", &code).append_pair("state", &body.state);
    state.app_grants.insert(
        code,
        Grant {
            user_id: user.id,
            challenge: body.code_challenge,
            redirect_uri: body.redirect_uri,
            device: device.to_owned(),
            created: Instant::now(),
        },
    );
    Ok(Json(ApproveOut { redirect: redirect.into() }))
}

#[derive(Deserialize)]
pub struct TokenIn {
    pub code: String,
    pub code_verifier: String,
    pub redirect_uri: String,
}

#[derive(Serialize)]
pub struct TokenOut {
    pub token: String,
    pub email: String,
}

/// Spend a code: the verifier must answer its challenge and the redirect must be the one it was
/// approved for. Anything wrong is the same 400, and the code is gone either way.
async fn token(
    State(state): State<Arc<AppState>>,
    Json(body): Json<TokenIn>,
) -> Result<Json<TokenOut>, StatusCode> {
    let grant = state.app_grants.take(&body.code).ok_or(StatusCode::BAD_REQUEST)?;
    if challenge_of(&body.code_verifier) != grant.challenge || body.redirect_uri != grant.redirect_uri {
        return Err(StatusCode::BAD_REQUEST);
    }
    let token = format!("{TOKEN_PREFIX}{}", auth::new_token());
    let mut store = state.store.lock().await;
    let user = store
        .user_by_id(&grant.user_id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::BAD_REQUEST)?;
    store
        .create_access_token(&auth::token_hash(&token), &user.id, &grant.device, None, false, None)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(TokenOut { token, email: user.email }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_loopback_http_is_a_redirect() {
        for ok in [
            "http://127.0.0.1:53124/cb",
            "http://127.0.0.1/lemmate-callback",
            "http://[::1]:9/x",
            "http://localhost:8/",
        ] {
            assert!(loopback_redirect(ok), "{ok}");
        }
        for bad in [
            "https://127.0.0.1/cb",
            "http://evil.example/cb",
            "http://127.0.0.1.evil.example/",
            "http://user@127.0.0.1/",
            "http://127.0.0.1/#frag",
            "lemmate://cb",
            "",
        ] {
            assert!(!loopback_redirect(bad), "{bad}");
        }
    }

    #[test]
    fn the_challenge_is_rfc_7636s() {
        // RFC 7636 appendix B.
        assert_eq!(
            challenge_of("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }
}

//! Accounts, sessions, and vault roles (SPEC §11.1, §11.2).
//!
//! Sessions are opaque random tokens; the store keeps only their BLAKE3 hash. Browsers get the
//! token in an HttpOnly cookie (so the WebSocket upgrade carries it for free); native clients
//! and the CLI send `Authorization: Bearer <token>`.

use std::sync::Arc;

use argon2::Argon2;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use axum::extract::{FromRequestParts, Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header, request::Parts};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router, routing::get};
use lemmate_core::attachments::hash_bytes;
use lemmate_core::store::{Role, UserRow};
use lemmate_core::{NoteId, Store, VaultId};
use serde::{Deserialize, Serialize};

use crate::app::AppState;

pub const COOKIE: &str = "lemmate_session";
pub const SESSION_TTL_MS: i64 = 30 * 24 * 60 * 60 * 1000;

#[derive(Debug, Clone)]
pub enum AuthMode {
    /// No accounts: every request acts as a local owner. For development and the desktop
    /// relay only — never on a network-facing server.
    Disabled,
    Enabled {
        /// Let anyone create an account. The very first account is always allowed (and is
        /// the admin); afterwards admins can always create users.
        allow_registration: bool,
        /// Mark the session cookie `Secure` (set when serving behind HTTPS).
        secure_cookies: bool,
    },
}

/// The requester. With auth disabled this is a synthetic local owner.
#[derive(Debug, Clone, Serialize)]
pub struct AuthUser {
    pub id: String,
    pub email: String,
    pub display_name: String,
    pub is_admin: bool,
}

impl AuthUser {
    pub fn local() -> Self {
        Self {
            id: "local".into(),
            email: "local@localhost".into(),
            display_name: "local".into(),
            is_admin: true,
        }
    }
    fn from_row(u: UserRow) -> Self {
        Self { id: u.id, email: u.email, display_name: u.display_name, is_admin: u.is_admin }
    }
}

pub fn hash_password(password: &str) -> Result<String, StatusCode> {
    let raw: [u8; 16] = rand::random();
    let salt = SaltString::encode_b64(&raw).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

pub fn verify_password(password: &str, hash: &str) -> bool {
    PasswordHash::new(hash)
        .map(|h| Argon2::default().verify_password(password.as_bytes(), &h).is_ok())
        .unwrap_or(false)
}

pub fn new_token() -> String {
    let bytes: [u8; 32] = rand::random();
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn token_hash(token: &str) -> String {
    hash_bytes(token.as_bytes())
}

/// The bearer token or session cookie carried by a request, if any.
pub fn token_from_headers(headers: &HeaderMap) -> Option<String> {
    if let Some(v) = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok())
        && let Some(t) = v.strip_prefix("Bearer ")
    {
        return Some(t.trim().to_owned());
    }
    let cookies = headers.get(header::COOKIE)?.to_str().ok()?;
    cookies.split(';').map(str::trim).find_map(|c| c.strip_prefix(&format!("{COOKIE}=")).map(str::to_owned))
}

/// Resolve the requester; `None` when auth is on and the token is missing/invalid.
pub async fn authenticate(state: &AppState, headers: &HeaderMap) -> Option<AuthUser> {
    match state.options.auth {
        AuthMode::Disabled => Some(AuthUser::local()),
        AuthMode::Enabled { .. } => {
            let token = token_from_headers(headers)?;
            let store = state.store.lock().await;
            store.session_user(&token_hash(&token)).ok().flatten().map(AuthUser::from_row)
        }
    }
}

impl FromRequestParts<Arc<AppState>> for AuthUser {
    type Rejection = StatusCode;
    async fn from_request_parts(parts: &mut Parts, state: &Arc<AppState>) -> Result<Self, Self::Rejection> {
        authenticate(state, &parts.headers).await.ok_or(StatusCode::UNAUTHORIZED)
    }
}

/// The requester's role on a vault. A vault nobody owns yet is claimed by whoever touches it
/// first (that is how vaults are created: the client just starts syncing a new id).
pub async fn role_or_claim(state: &AppState, user: &AuthUser, vault: VaultId, claim: bool) -> Option<Role> {
    if matches!(state.options.auth, AuthMode::Disabled) {
        return Some(Role::Owner);
    }
    let mut store = state.store.lock().await;
    if let Ok(Some(r)) = store.membership(vault, &user.id) {
        return Some(r);
    }
    if claim && store.member_count(vault).ok()? == 0 {
        store.set_membership(vault, &user.id, Role::Owner).ok()?;
        return Some(Role::Owner);
    }
    None
}

/// The role a user holds on one note: the vault role, or a direct share (SPEC §11.2 "overrides
/// upward only"), whichever is higher.
pub async fn note_role(state: &AppState, user: &AuthUser, vault: VaultId, note: NoteId) -> Option<Role> {
    if matches!(state.options.auth, AuthMode::Disabled) {
        return Some(Role::Owner);
    }
    let store = state.store.lock().await;
    let vault_role = store.membership(vault, &user.id).ok().flatten();
    let share_role = store.note_share_role(note, &user.id).ok().flatten();
    match (vault_role, share_role) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    }
}

/// 404 for non-members (no existence leak), 403 for an insufficient role.
pub async fn require(
    state: &AppState,
    user: &AuthUser,
    vault: VaultId,
    min: Role,
) -> Result<Role, StatusCode> {
    match role_or_claim(state, user, vault, false).await {
        None => Err(StatusCode::NOT_FOUND),
        Some(r) if r >= min => Ok(r),
        Some(_) => Err(StatusCode::FORBIDDEN),
    }
}

// ---- Routes ---------------------------------------------------------------------------------

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/v1/auth/register", axum::routing::post(register))
        .route("/api/v1/auth/login", axum::routing::post(login))
        .route("/api/v1/auth/logout", axum::routing::post(logout))
        .route("/api/v1/auth/me", get(me))
        .route("/api/v1/vaults/{vault}/members", get(list_members).put(put_member))
        .route("/api/v1/vaults/{vault}/members/{user}", axum::routing::delete(delete_member))
        .route(
            "/api/v1/vaults/{vault}/notes/{id}/shares",
            get(list_shares).put(put_share).delete(delete_share),
        )
        .route("/api/v1/shared-with-me", get(shared_with_me))
        .route("/api/v1/shared/{token}", get(shared_note))
}

#[derive(Serialize)]
pub struct ShareOut {
    pub kind: String,
    pub user_id: Option<String>,
    pub email: Option<String>,
    pub role: String,
    pub expires_ms: Option<i64>,
    /// Only returned once, when a link is created.
    pub link: Option<String>,
}

fn share_out(s: lemmate_core::store::NoteShareRow) -> ShareOut {
    ShareOut {
        kind: if s.token_hash.is_some() { "link".into() } else { "user".into() },
        user_id: s.user_id,
        email: s.email,
        role: s.role.as_str().to_owned(),
        expires_ms: s.expires_ms,
        link: None,
    }
}

async fn note_in_vault(state: &AppState, vault: VaultId, id: NoteId) -> Result<(), StatusCode> {
    match state.store.lock().await.note_by_id(id) {
        Ok(Some(row)) if row.vault_id == vault => Ok(()),
        _ => Err(StatusCode::NOT_FOUND),
    }
}

async fn list_shares(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Path((vault, id)): Path<(String, String)>,
) -> Result<Json<Vec<ShareOut>>, StatusCode> {
    let vault: VaultId = vault.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    let id: lemmate_core::NoteId = id.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    require(&state, &user, vault, Role::Editor).await?;
    note_in_vault(&state, vault, id).await?;
    let rows = state.store.lock().await.note_shares(id).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(rows.into_iter().map(share_out).collect()))
}

#[derive(Deserialize)]
pub struct ShareIn {
    /// `user` (needs `email`) or `link`.
    pub kind: String,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default = "default_viewer")]
    pub role: String,
    /// For links: lifetime in days (default: no expiry).
    #[serde(default)]
    pub expires_days: Option<u32>,
}

fn default_viewer() -> String {
    "viewer".into()
}

/// Share a note with a user (editor/viewer) or mint a public read-only link. Vault editors may
/// share; links are always read-only.
async fn put_share(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Path((vault, id)): Path<(String, String)>,
    Json(body): Json<ShareIn>,
) -> Result<Json<ShareOut>, StatusCode> {
    let vault: VaultId = vault.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    let id: lemmate_core::NoteId = id.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    require(&state, &user, vault, Role::Editor).await?;
    note_in_vault(&state, vault, id).await?;
    let role = Role::parse(&body.role).filter(|r| *r < Role::Owner).ok_or(StatusCode::BAD_REQUEST)?;
    let mut store = state.store.lock().await;
    match body.kind.as_str() {
        "user" => {
            let email = body.email.as_deref().ok_or(StatusCode::BAD_REQUEST)?;
            let target = store
                .user_by_email(email)
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
                .ok_or(StatusCode::NOT_FOUND)?;
            store
                .share_note_with_user(id, &target.id, role, &user.id)
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            Ok(Json(ShareOut {
                kind: "user".into(),
                user_id: Some(target.id),
                email: Some(target.email),
                role: role.as_str().into(),
                expires_ms: None,
                link: None,
            }))
        }
        "link" => {
            let token = new_token();
            let expires =
                body.expires_days.map(|d| lemmate_core::store::now_ms() + i64::from(d) * 24 * 3600 * 1000);
            store
                .share_note_by_link(id, &token_hash(&token), Role::Viewer, &user.id, expires)
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            Ok(Json(ShareOut {
                kind: "link".into(),
                user_id: None,
                email: None,
                role: "viewer".into(),
                expires_ms: expires,
                link: Some(format!("/#/s/{token}")),
            }))
        }
        _ => Err(StatusCode::BAD_REQUEST),
    }
}

#[derive(Deserialize)]
pub struct Unshare {
    #[serde(default)]
    pub user_id: Option<String>,
    /// Remove every link share of the note.
    #[serde(default)]
    pub links: bool,
}

async fn delete_share(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Path((vault, id)): Path<(String, String)>,
    Json(body): Json<Unshare>,
) -> Result<StatusCode, StatusCode> {
    let vault: VaultId = vault.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    let id: lemmate_core::NoteId = id.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    require(&state, &user, vault, Role::Editor).await?;
    let mut store = state.store.lock().await;
    if let Some(u) = body.user_id {
        store.unshare_note_user(id, &u).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    }
    if body.links {
        for s in store.note_shares(id).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)? {
            if let Some(h) = s.token_hash {
                store.unshare_note_link(id, &h).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            }
        }
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Serialize)]
pub struct SharedNoteOut {
    pub id: String,
    pub vault_id: String,
    pub path: String,
    pub title: Option<String>,
    pub role: String,
}

async fn shared_with_me(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
) -> Result<Json<Vec<SharedNoteOut>>, StatusCode> {
    let rows = state
        .store
        .lock()
        .await
        .notes_shared_with(&user.id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(
        rows.into_iter()
            .map(|(n, r)| SharedNoteOut {
                id: n.id.to_string(),
                vault_id: n.vault_id.to_string(),
                path: n.path,
                title: n.title,
                role: r.as_str().into(),
            })
            .collect(),
    ))
}

#[derive(Serialize)]
pub struct PublicNote {
    pub id: String,
    pub path: String,
    pub title: Option<String>,
    pub content: String,
}

/// Anonymous read of a note through a link token (SPEC §11.2 public links).
async fn shared_note(
    State(state): State<Arc<AppState>>,
    Path(token): Path<String>,
) -> Result<Json<PublicNote>, StatusCode> {
    let (note, _) = state
        .store
        .lock()
        .await
        .note_for_link(&token_hash(&token))
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    let row = state
        .store
        .lock()
        .await
        .note_by_id(note)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    let content = state
        .store
        .lock()
        .await
        .load_doc(lemmate_core::DocId::Note(note))
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .text();
    Ok(Json(PublicNote { id: note.to_string(), path: row.path, title: row.title, content }))
}

#[derive(Deserialize)]
pub struct Credentials {
    pub email: String,
    pub password: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub device: Option<String>,
}

#[derive(Serialize)]
pub struct SessionOut {
    pub token: String,
    pub user: AuthUser,
}

fn session_response(state: &AppState, token: String, user: AuthUser) -> Response {
    let secure = matches!(state.options.auth, AuthMode::Enabled { secure_cookies: true, .. });
    let cookie = format!(
        "{COOKIE}={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age={}{}",
        SESSION_TTL_MS / 1000,
        if secure { "; Secure" } else { "" }
    );
    let mut resp = Json(SessionOut { token, user }).into_response();
    if let Ok(v) = HeaderValue::from_str(&cookie) {
        resp.headers_mut().insert(header::SET_COOKIE, v);
    }
    resp
}

async fn issue_session(
    store: &mut Store,
    user: &UserRow,
    device: Option<&str>,
) -> Result<String, StatusCode> {
    let token = new_token();
    store
        .create_session(&token_hash(&token), &user.id, device, SESSION_TTL_MS)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(token)
}

async fn register(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(c): Json<Credentials>,
) -> Result<Response, StatusCode> {
    let AuthMode::Enabled { allow_registration, .. } = state.options.auth else {
        return Err(StatusCode::NOT_FOUND);
    };
    let email = c.email.trim().to_lowercase();
    if !email.contains('@') || c.password.len() < 8 {
        return Err(StatusCode::BAD_REQUEST);
    }
    let requester = authenticate(&state, &headers).await;
    let mut store = state.store.lock().await;
    let first = store.user_count().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)? == 0;
    let allowed = first || allow_registration || requester.as_ref().is_some_and(|u| u.is_admin);
    if !allowed {
        return Err(StatusCode::FORBIDDEN);
    }
    if store.user_by_email(&email).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?.is_some() {
        return Err(StatusCode::CONFLICT);
    }
    let id = lemmate_core::NoteId::new().to_string();
    let hash = hash_password(&c.password)?;
    let name = c.display_name.unwrap_or_else(|| email.split('@').next().unwrap_or("user").to_owned());
    store
        .create_user(&id, &email, &name, Some(&hash), first)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let user = store
        .user_by_id(&id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;
    // An admin creating someone else's account does not get logged in as them.
    if requester.as_ref().is_some_and(|u| u.is_admin && !first) {
        return Ok(Json(AuthUser::from_row(user)).into_response());
    }
    let token = issue_session(&mut store, &user, c.device.as_deref()).await?;
    Ok(session_response(&state, token, AuthUser::from_row(user)))
}

async fn login(
    State(state): State<Arc<AppState>>,
    Json(c): Json<Credentials>,
) -> Result<Response, StatusCode> {
    if matches!(state.options.auth, AuthMode::Disabled) {
        return Err(StatusCode::NOT_FOUND);
    }
    let mut store = state.store.lock().await;
    let user = store.user_by_email(c.email.trim()).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let ok = user
        .as_ref()
        .and_then(|u| u.password_hash.as_deref())
        .is_some_and(|h| verify_password(&c.password, h));
    if !ok {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let user = user.expect("checked");
    let token = issue_session(&mut store, &user, c.device.as_deref()).await?;
    Ok(session_response(&state, token, AuthUser::from_row(user)))
}

async fn logout(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if let Some(t) = token_from_headers(&headers) {
        let _ = state.store.lock().await.delete_session(&token_hash(&t));
    }
    let mut resp = StatusCode::NO_CONTENT.into_response();
    resp.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_static("lemmate_session=; Path=/; HttpOnly; Max-Age=0"),
    );
    resp
}

async fn me(user: AuthUser) -> Json<AuthUser> {
    Json(user)
}

#[derive(Serialize)]
pub struct MemberOut {
    pub user_id: String,
    pub email: String,
    pub display_name: String,
    pub role: String,
}

async fn list_members(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Path(vault): Path<String>,
) -> Result<Json<Vec<MemberOut>>, StatusCode> {
    let vault: VaultId = vault.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    require(&state, &user, vault, Role::Viewer).await?;
    let rows = state.store.lock().await.members(vault).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(
        rows.into_iter()
            .map(|(u, r)| MemberOut {
                user_id: u.id,
                email: u.email,
                display_name: u.display_name,
                role: r.as_str().to_owned(),
            })
            .collect(),
    ))
}

#[derive(Deserialize)]
pub struct MemberIn {
    pub email: String,
    pub role: String,
}

async fn put_member(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Path(vault): Path<String>,
    Json(m): Json<MemberIn>,
) -> Result<StatusCode, StatusCode> {
    let vault: VaultId = vault.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    require(&state, &user, vault, Role::Owner).await?;
    let role = Role::parse(&m.role).ok_or(StatusCode::BAD_REQUEST)?;
    let mut store = state.store.lock().await;
    let target = store
        .user_by_email(&m.email)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    store.set_membership(vault, &target.id, role).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn delete_member(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Path((vault, target)): Path<(String, String)>,
) -> Result<StatusCode, StatusCode> {
    let vault: VaultId = vault.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    let role = require(&state, &user, vault, Role::Viewer).await?;
    if target != user.id && role < Role::Owner {
        return Err(StatusCode::FORBIDDEN); // members may leave; only owners remove others
    }
    let mut store = state.store.lock().await;
    if role == Role::Owner && target == user.id {
        let owners = store
            .members(vault)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
            .iter()
            .filter(|(_, r)| *r == Role::Owner)
            .count();
        if owners <= 1 {
            return Err(StatusCode::CONFLICT); // the last owner cannot leave
        }
    }
    store.remove_membership(vault, &target).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(StatusCode::NO_CONTENT)
}

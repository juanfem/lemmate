//! Router, shared state, WebSocket relay, and REST handlers (SPEC §7, §13.1).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crate::auth::{self, AuthMode, AuthUser};
use axum::body::Bytes;
use axum::extract::ws::{CloseFrame, Message as WsMessage, WebSocket, WebSocketUpgrade};
use axum::extract::{DefaultBodyLimit, Multipart, Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use lemmate_core::attachments::{
    AttachmentStore, MAX_ATTACHMENT_BYTES, hash_bytes, is_valid_hash, mime_for_path,
};
use lemmate_core::import::{self, Upload, UploadReport};
use lemmate_core::store::{AttachmentRow, Role, Saved, now_ms};
use lemmate_core::sync::{Frame, Message, SyncMessage};
use lemmate_core::{DocId, NoteDoc, NoteId, RetentionPolicy, Store, VaultDoc, VaultId, history, markdown};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, Semaphore, broadcast, watch};
use tower_http::trace::TraceLayer;
use tracing::{info, warn};
use yrs::StateVector;

#[derive(Debug, Clone)]
pub struct ServerOptions {
    pub policy: RetentionPolicy,
    /// Root of the content-addressed blob store.
    pub attachments_dir: std::path::PathBuf,
    /// How long an unreferenced blob is kept before it is purged (SPEC §9: trash window).
    pub attachment_grace: std::time::Duration,
    /// Built web client (`ui/dist`) to serve at `/`; none → API and sync only.
    pub web_dir: Option<std::path::PathBuf>,
    pub auth: AuthMode,
    /// `pandoc` binary for exports (default: on PATH); exports answer 501 when it is missing.
    pub pandoc: Option<std::path::PathBuf>,
    /// `quarto` binary for renders (default: `$LEMMATE_QUARTO`, then on PATH).
    pub quarto: Option<std::path::PathBuf>,
    /// Whether notes may be rendered through Quarto at all. A render honours the note's front
    /// matter, which can run Lua filters and pull files into the output — on this host, at the
    /// say-so of anyone who can edit a note. Off, renders answer 501 as if quarto were missing.
    pub quarto_enabled: bool,
    /// Email + password sign-in (SPEC §11.1). Off, accounts come only from `oidc`.
    pub password_login: bool,
    /// An OpenID Connect provider to sign in with.
    pub oidc: Option<crate::oidc::OidcConfig>,
}

impl Default for ServerOptions {
    fn default() -> Self {
        Self {
            policy: RetentionPolicy::default(),
            attachments_dir: std::env::temp_dir().join("notes-attachments"),
            attachment_grace: std::time::Duration::from_secs(30 * 24 * 60 * 60),
            web_dir: None,
            auth: AuthMode::Disabled,
            pandoc: None,
            quarto: None,
            quarto_enabled: true,
            password_login: true,
            oidc: None,
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PurgeReport {
    pub vaults: usize,
    pub purged_notes: usize,
    pub newly_orphaned: usize,
    pub rescued: usize,
    pub purged: usize,
}

/// Orphan sweep (SPEC §9): a blob no live vault-doc entry references is marked at first sight
/// and deleted once it has been unreferenced for `grace`; a blob referenced again is rescued.
pub async fn purge_orphans(
    state: &AppState,
    now_ms: i64,
    grace: std::time::Duration,
) -> lemmate_core::Result<PurgeReport> {
    let mut report = PurgeReport::default();
    let mut store = state.store.lock().await;
    // Notes trashed longer than the grace period go for good (SPEC §9).
    let grace_days = (grace.as_secs() / 86_400) as u32;
    report.purged_notes = store.purge_trash(grace_days)?;
    if report.purged_notes > 0 {
        // A purged note's room would still answer with its text, for an id that is now free.
        drop(store);
        state.evict_idle_rooms(Duration::ZERO).await;
        store = state.store.lock().await;
    }
    let vaults: Vec<VaultId> = store
        .doc_ids()?
        .into_iter()
        .filter_map(|d| match d {
            DocId::Vault(v) => Some(v),
            DocId::Note(_) => None,
        })
        .collect();
    let grace_ms = grace.as_millis() as i64;
    for vault in vaults {
        report.vaults += 1;
        let live: std::collections::HashSet<String> =
            store.load_vault_doc(vault)?.attachment_entries().into_iter().map(|(_, h)| h).collect();
        for (hash, orphaned) in store.attachment_hashes(vault)? {
            if live.contains(&hash) {
                if orphaned.is_some() {
                    store.set_attachment_orphaned(vault, &hash, None)?;
                    report.rescued += 1;
                }
                continue;
            }
            let since = orphaned.unwrap_or(now_ms);
            if now_ms - since >= grace_ms {
                state.attachments.remove(vault, &hash)?;
                store.delete_attachment(vault, &hash)?;
                report.purged += 1;
            } else if orphaned.is_none() {
                store.set_attachment_orphaned(vault, &hash, Some(now_ms))?;
                report.newly_orphaned += 1;
            }
        }
    }
    Ok(report)
}

/// Body limit for everything but uploads: JSON, mostly, and a note's whole text at the most.
pub const JSON_BODY_LIMIT: usize = 4 * 1024 * 1024;
/// The largest WebSocket message (and frame: a browser sends a message as one frame) accepted.
/// The biggest legitimate one is a first sync of a large vault doc or note — text and paths,
/// never attachment bytes, which go over HTTP.
pub const WS_MAX_MESSAGE: usize = 32 * 1024 * 1024;
/// A room nobody has touched for this long is dropped; its doc is in the store already.
pub const ROOM_IDLE: Duration = Duration::from_secs(10 * 60);
/// How often `get_room` looks for idle rooms.
const ROOM_SWEEP_EVERY: Duration = Duration::from_secs(60);
/// How long a provisional claim on a new note (`vault_of_note`) holds, and how many are kept.
const CLAIM_TTL: Duration = Duration::from_secs(60 * 60);
const MAX_CLAIMS: usize = 100_000;
/// pandoc / Quarto runs at once; the rest wait their turn, up to `RENDER_WAIT`.
const RENDER_SLOTS: usize = 2;
const RENDER_WAIT: Duration = Duration::from_secs(120);
/// How often an open socket checks its credential and subscriptions even when nothing says
/// they changed — a session that simply expired.
const WS_RECHECK: Duration = Duration::from_secs(5 * 60);

pub struct AppState {
    pub store: Mutex<Store>,
    pub options: ServerOptions,
    pub attachments: AttachmentStore,
    /// Note docs seen before their vault entry exists, bound to the vault the creating
    /// connection was working in (a UI writes the note text before the vault map entry), and
    /// when. Only a note with no history can be claimed; a claim lapses after `CLAIM_TTL`, and
    /// goes as soon as a vault doc names the note.
    note_vault_claims: Mutex<HashMap<NoteId, (VaultId, Instant)>>,
    /// Bumped whenever who may read what changes — members, shares, tokens, sessions, a note
    /// moving vaults — so every open socket checks its credential and subscriptions again.
    auth_epoch: watch::Sender<u64>,
    /// Held across "is this path free?" and creating the note there, so two requests for the
    /// same day's note do not make two.
    create_lock: Mutex<()>,
    /// pandoc and Quarto runs (`RENDER_SLOTS`).
    render_slots: Semaphore,
    /// Failed password sign-ins (`auth::login`).
    pub(crate) login_throttle: auth::LoginThrottle,
    last_room_sweep: std::sync::Mutex<Instant>,
    /// Per vault, the files no note used when they were last looked for among the notes'
    /// references (`claim_waiting_files`). Each is looked for once per process: a note that
    /// names one later is indexed then anyway.
    waiting_files: std::sync::Mutex<HashMap<VaultId, HashSet<String>>>,
    /// Renders made for viewing, to be opened again without rendering again.
    renders: lemmate_core::quarto::RenderCache,
    rooms: Mutex<HashMap<String, Arc<Room>>>,
    bus: broadcast::Sender<Outbound>,
    next_conn: AtomicU64,
    /// The OIDC client, when `options.oidc` is set.
    pub oidc: Option<crate::oidc::Oidc>,
    /// Native-app sign-ins approved and waiting to be collected (`apps.rs`).
    pub app_grants: crate::apps::Grants,
}

struct Room {
    id: DocId,
    doc: Mutex<RoomDoc>,
    /// `now_ms()` when it was last handed out (`get_room`).
    last_used: AtomicI64,
}

/// A note doc or the vault doc, behind one CRDT interface.
enum RoomDoc {
    Note(NoteDoc),
    Vault(VaultDoc),
}

impl RoomDoc {
    fn state_vector(&self) -> StateVector {
        match self {
            RoomDoc::Note(d) => d.state_vector(),
            RoomDoc::Vault(d) => d.state_vector(),
        }
    }
    fn diff_since(&self, sv: &StateVector) -> Vec<u8> {
        match self {
            RoomDoc::Note(d) => d.diff_since(sv),
            RoomDoc::Vault(d) => d.diff_since(sv),
        }
    }
    fn apply_update(&self, u: &[u8]) -> lemmate_core::Result<bool> {
        match self {
            RoomDoc::Note(d) => d.apply_update(u),
            RoomDoc::Vault(d) => d.apply_update(u),
        }
    }
    fn encode_full(&self) -> Vec<u8> {
        match self {
            RoomDoc::Note(d) => d.encode_full(),
            RoomDoc::Vault(d) => d.encode_full(),
        }
    }
}

/// A frame to fan out to every other connection subscribed to `doc_id`.
#[derive(Clone)]
struct Outbound {
    from: u64,
    doc_id: Arc<str>,
    bytes: Arc<Vec<u8>>,
}

pub fn build_state(store: Store, options: ServerOptions) -> Arc<AppState> {
    let (bus, _) = broadcast::channel(1024);
    let attachments = AttachmentStore::new(&options.attachments_dir);
    let oidc = options.oidc.clone().map(crate::oidc::Oidc::new);
    Arc::new(AppState {
        store: Mutex::new(store),
        options,
        attachments,
        note_vault_claims: Mutex::new(HashMap::new()),
        auth_epoch: watch::channel(0).0,
        create_lock: Mutex::new(()),
        render_slots: Semaphore::new(RENDER_SLOTS),
        login_throttle: Default::default(),
        last_room_sweep: std::sync::Mutex::new(Instant::now()),
        waiting_files: std::sync::Mutex::new(HashMap::new()),
        renders: lemmate_core::quarto::RenderCache::new(),
        rooms: Mutex::new(HashMap::new()),
        bus,
        next_conn: AtomicU64::new(1),
        oidc,
        app_grants: Default::default(),
    })
}

impl AppState {
    /// Tell every open socket that access may have changed (see `auth_epoch`).
    pub fn auth_changed(&self) {
        self.auth_epoch.send_modify(|e| *e = e.wrapping_add(1));
    }

    /// Rooms held in memory right now.
    pub async fn room_count(&self) -> usize {
        self.rooms.lock().await.len()
    }

    /// Drop every room unused for `idle` that nobody is holding. Safe at any time: an update is
    /// in the store before it is fanned out, so a dropped room loads again exactly as it was,
    /// and subscriptions are by doc id, not by room. Returns how many went.
    pub async fn evict_idle_rooms(&self, idle: Duration) -> usize {
        let cutoff = now_ms() - idle.as_millis() as i64;
        let mut rooms = self.rooms.lock().await;
        let before = rooms.len();
        // Under the map's lock a count of one means no handler holds it, and none can take it.
        rooms.retain(|_, r| Arc::strong_count(r) > 1 || r.last_used.load(Ordering::Relaxed) > cutoff);
        before - rooms.len()
    }
}

pub fn router(state: Arc<AppState>) -> Router {
    let web_dir = state.options.web_dir.clone();
    let router = Router::new()
        .merge(auth::router())
        .merge(crate::oidc::router())
        .merge(crate::apps::router())
        .route("/healthz", get(|| async { "ok" }))
        .route("/ws", get(ws_upgrade))
        .route("/api/v1/vaults", get(list_vaults))
        .route("/api/v1/vaults/{vault}", axum::routing::delete(delete_vault))
        .route("/api/v1/vaults/{vault}/notes", get(list_notes).post(create_note))
        .route(
            "/api/v1/vaults/{vault}/import",
            axum::routing::post(import_vault).layer(DefaultBodyLimit::max(MAX_ATTACHMENT_BYTES as usize)),
        )
        .route(
            "/api/v1/vaults/{vault}/notes/{id}",
            get(get_note).put(put_note).patch(patch_note).delete(delete_note),
        )
        .route("/api/v1/vaults/{vault}/daily/{date}", get(daily_note))
        .route("/api/v1/vaults/{vault}/trash", get(list_trash))
        .route("/api/v1/vaults/{vault}/notes/{id}/restore", axum::routing::post(restore_note))
        .route("/api/v1/vaults/{vault}/notes/{id}/backlinks", get(backlinks))
        .route("/api/v1/vaults/{vault}/notes/{id}/export", axum::routing::post(export_note))
        .route("/api/v1/vaults/{vault}/notes/{id}/render", get(render_page).post(render_note))
        .route("/api/v1/vaults/{vault}/notes/{id}/render/{render}", get(kept_render))
        .route("/api/v1/vaults/{vault}/notes/{id}/versions", get(list_versions).post(save_version))
        .route("/api/v1/vaults/{vault}/notes/{id}/versions/{seq}", get(get_version).patch(label_version))
        .route("/api/v1/vaults/{vault}/tags", get(tags))
        .route("/api/v1/vaults/{vault}/tagged", get(tagged))
        .route("/api/v1/vaults/{vault}/search", get(search_vault))
        .route("/api/v1/search", get(search))
        // Uploads may be as large as an attachment; everything else is small (`JSON_BODY_LIMIT`).
        .route(
            "/api/v1/vaults/{vault}/attachments/{hash}",
            get(get_attachment)
                .put(put_attachment)
                .layer(DefaultBodyLimit::max(MAX_ATTACHMENT_BYTES as usize)),
        )
        .route(
            "/api/v1/vaults/{vault}/files",
            get(list_files)
                .put(put_file)
                .delete(delete_file)
                .layer(DefaultBodyLimit::max(MAX_ATTACHMENT_BYTES as usize)),
        )
        .route("/api/v1/vaults/{vault}/files/move", axum::routing::post(move_file))
        .layer(DefaultBodyLimit::max(JSON_BODY_LIMIT))
        // Nothing this server sends is to be sniffed into another type than it says.
        .layer(axum::middleware::map_response(|mut r: axum::response::Response| async move {
            r.headers_mut()
                .insert(header::X_CONTENT_TYPE_OPTIONS, header::HeaderValue::from_static("nosniff"));
            r
        }));
    let router = match web_dir {
        // Single-page app: unknown paths fall back to index.html so `#/v/<id>` links work.
        Some(dir) => router.fallback_service(lemmate_core::web::client(&dir)),
        None => router,
    };
    router.layer(TraceLayer::new_for_http()).with_state(state)
}

// ---- Sync over WebSocket --------------------------------------------------------------------

async fn ws_upgrade(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    user: AuthUser,
) -> impl IntoResponse {
    // Kept so the socket can ask later whether its credential still stands (`recheck_socket`).
    let token = auth::token_from_headers(&headers).unwrap_or_default();
    ws.max_message_size(WS_MAX_MESSAGE)
        .max_frame_size(WS_MAX_MESSAGE)
        .on_upgrade(move |socket| handle_socket(socket, state, user, token))
}

/// Per-connection authorization state.
struct Conn {
    id: u64,
    user: AuthUser,
    /// The session or access token the socket was opened with.
    token: String,
    /// The vault this connection last gained access to; new note docs are bound to it.
    vault: Option<VaultId>,
}

async fn handle_socket(mut socket: WebSocket, state: Arc<AppState>, user: AuthUser, token: String) {
    let conn_id = state.next_conn.fetch_add(1, Ordering::Relaxed);
    let mut conn = Conn { id: conn_id, user, token, vault: None };
    let mut rx = state.bus.subscribe();
    let mut auth_rx = state.auth_epoch.subscribe();
    let mut recheck = tokio::time::interval_at(tokio::time::Instant::now() + WS_RECHECK, WS_RECHECK);
    let mut subscribed: HashSet<String> = HashSet::new();
    info!(conn_id, user = %conn.user.email, "ws connected");

    loop {
        tokio::select! {
            incoming = socket.recv() => match incoming {
                Some(Ok(WsMessage::Binary(bytes))) => {
                    let replies = handle_frame(&state, &mut conn, &bytes, &mut subscribed).await;
                    for reply in replies {
                        if socket.send(WsMessage::Binary(reply.into())).await.is_err() {
                            break;
                        }
                    }
                }
                Some(Ok(WsMessage::Close(_))) | None | Some(Err(_)) => break,
                Some(Ok(_)) => {}
            },
            out = rx.recv() => match out {
                Ok(o) => {
                    if o.from != conn_id && subscribed.contains(&*o.doc_id)
                        && socket.send(WsMessage::Binary(o.bytes.as_slice().to_vec().into())).await.is_err()
                    {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    // Updates this client needed are gone, so its docs may be behind for good.
                    // Closing makes it reconnect, and a reconnect handshakes every doc again.
                    warn!(conn_id, n, "ws client lagged; closing so it resyncs");
                    let _ = socket.send(close(axum::extract::ws::close_code::NORMAL, "lagged; resync")).await;
                    break;
                }
                Err(broadcast::error::RecvError::Closed) => break,
            },
            Ok(()) = auth_rx.changed() => {
                if !recheck_socket(&state, &mut socket, &mut conn, &mut subscribed).await {
                    break;
                }
            }
            _ = recheck.tick() => {
                if !recheck_socket(&state, &mut socket, &mut conn, &mut subscribed).await {
                    break;
                }
            }
        }
    }
    info!(conn_id, "ws disconnected");
}

fn close(code: u16, reason: &'static str) -> WsMessage {
    WsMessage::Close(Some(CloseFrame { code, reason: reason.into() }))
}

/// The frame that tells a client it may not have a doc (the client shows it, and stops).
fn denied(doc_id: &str, reason: &str) -> Vec<u8> {
    Frame::new(doc_id, &Message::Auth(Some(reason.into()))).encode()
}

/// Ask again whether this socket's credential stands and which of its docs it may still read:
/// a revoked token, a session signed out or expired, a member removed, a share taken back. A
/// doc it lost is dropped from its subscriptions and the client told so; a credential that is
/// gone closes the socket (returns false).
async fn recheck_socket(
    state: &AppState,
    socket: &mut WebSocket,
    conn: &mut Conn,
    subscribed: &mut HashSet<String>,
) -> bool {
    if matches!(state.options.auth, AuthMode::Disabled) {
        return true;
    }
    let Some(user) = auth::authenticate_token(state, &conn.token).await else {
        info!(conn_id = conn.id, "ws credential no longer valid; closing");
        let _ = socket.send(close(axum::extract::ws::close_code::POLICY, "signed out")).await;
        return false;
    };
    conn.user = user;
    let mut lost = Vec::new();
    for doc in subscribed.iter() {
        let readable = match doc.parse::<DocId>() {
            Ok(id) => doc_role(state, conn, id, false).await.is_some(),
            Err(_) => false,
        };
        if !readable {
            lost.push(doc.clone());
        }
    }
    for doc in lost {
        subscribed.remove(&doc);
        info!(conn_id = conn.id, %doc, user = %conn.user.email, "access withdrawn");
        if socket.send(WsMessage::Binary(denied(&doc, "permission denied").into())).await.is_err() {
            return false;
        }
    }
    true
}

/// The role this connection holds on a doc. With `claim`, a vault nobody owns, or a note nobody
/// has written yet, is taken by this connection (that is a first sync); without, it only asks.
async fn doc_role(state: &AppState, conn: &Conn, doc_id: DocId, claim: bool) -> Option<Role> {
    if matches!(state.options.auth, AuthMode::Disabled) {
        return Some(Role::Owner);
    }
    match doc_id {
        DocId::Vault(v) => auth::role_or_claim(state, &conn.user, v, claim).await,
        DocId::Note(id) => {
            let v = vault_of_note(state, id, conn.vault, claim).await?;
            auth::note_role(state, &conn.user, v, id).await
        }
    }
}

/// Which vault a note doc belongs to: its row (in the trash or not), a provisional claim, or —
/// for a note with no history at all — the connection's vault, which `claim` records.
///
/// A doc with history but no row and no live claim is not new: a note purged from the trash, one
/// left behind by a deleted vault, a claim that lapsed. Nobody gets to adopt it by syncing it.
async fn vault_of_note(
    state: &AppState,
    id: NoteId,
    conn_vault: Option<VaultId>,
    claim: bool,
) -> Option<VaultId> {
    let history = {
        let store = state.store.lock().await;
        if let Some(home) = store.note_vault_of(id).ok()? {
            return Some(home);
        }
        store.has_history(DocId::Note(id)).ok()?
    };
    let mut claims = state.note_vault_claims.lock().await;
    if let Some((v, at)) = claims.get(&id)
        && at.elapsed() < CLAIM_TTL
    {
        return Some(*v);
    }
    if history {
        return None;
    }
    let v = conn_vault?;
    if claim {
        if claims.len() >= MAX_CLAIMS {
            claims.retain(|_, (_, at)| at.elapsed() < CLAIM_TTL);
            if claims.len() >= MAX_CLAIMS
                && let Some(oldest) = claims.iter().min_by_key(|(_, (_, at))| *at).map(|(k, _)| *k)
            {
                claims.remove(&oldest);
            }
        }
        claims.insert(id, (v, Instant::now()));
    }
    Some(v)
}

/// Process one inbound frame; returns frames to send back to *this* connection.
async fn handle_frame(
    state: &Arc<AppState>,
    conn: &mut Conn,
    bytes: &[u8],
    subscribed: &mut HashSet<String>,
) -> Vec<Vec<u8>> {
    let conn_id = conn.id;
    let frame = match Frame::decode(bytes) {
        Ok(f) => f,
        Err(e) => {
            warn!(conn_id, %e, "bad frame");
            return Vec::new();
        }
    };
    let doc_id: DocId = match frame.doc_id.parse() {
        Ok(id) => id,
        Err(e) => {
            warn!(conn_id, %e, "bad doc id");
            return Vec::new();
        }
    };
    let msg = match frame.message() {
        Ok(m) => m,
        Err(e) => {
            warn!(conn_id, %e, "bad yjs message");
            return Vec::new();
        }
    };
    // Authorization (SPEC §11.2): reads need a viewer, writes an editor; a vault nobody owns
    // is claimed by the first authenticated user who touches it.
    let is_write =
        matches!(msg, Message::Sync(SyncMessage::SyncStep2(_)) | Message::Sync(SyncMessage::Update(_)));
    let role = doc_role(state, conn, doc_id, true).await;
    let allowed = match role {
        Some(r) if is_write => r >= Role::Editor,
        Some(_) => true,
        None => false,
    };
    if !allowed {
        warn!(conn_id, doc = %frame.doc_id, user = %conn.user.email, write = is_write, "denied");
        if role.is_none() {
            subscribed.remove(&frame.doc_id);
        }
        return vec![denied(&frame.doc_id, "permission denied")];
    }
    if let DocId::Vault(v) = doc_id {
        conn.vault = Some(v);
    }

    let room = match get_room(state, doc_id).await {
        Ok(r) => r,
        Err(e) => {
            warn!(conn_id, %e, "loading doc");
            return Vec::new();
        }
    };

    match msg {
        Message::Sync(SyncMessage::SyncStep1(sv)) => {
            subscribed.insert(frame.doc_id.clone());
            let doc = room.doc.lock().await;
            vec![
                Frame::new(&frame.doc_id, &Message::Sync(SyncMessage::SyncStep2(doc.diff_since(&sv))))
                    .encode(),
                Frame::new(&frame.doc_id, &Message::Sync(SyncMessage::SyncStep1(doc.state_vector())))
                    .encode(),
            ]
        }
        Message::Sync(SyncMessage::SyncStep2(update)) | Message::Sync(SyncMessage::Update(update)) => {
            subscribed.insert(frame.doc_id.clone());
            let touched = {
                let doc = room.doc.lock().await;
                // What an update would put into a vault doc is looked at before it goes in.
                let touched = match &*doc {
                    RoomDoc::Vault(v) => match vet_vault_update(v, &update) {
                        Ok(t) => Some(t),
                        Err(bad) => {
                            warn!(conn_id, path = %bad, "refused a vault update with an unsafe path");
                            return vec![denied(&frame.doc_id, &format!("refused: unsafe path {bad:?}"))];
                        }
                    },
                    RoomDoc::Note(_) => None,
                };
                match doc.apply_update(&update) {
                    Err(e) => {
                        warn!(conn_id, %e, "rejected update");
                        return Vec::new();
                    }
                    // Nothing new (e.g. the empty SyncStep2 an in-sync client answers with):
                    // don't persist it and don't wake other subscribers.
                    Ok(false) => return Vec::new(),
                    Ok(true) => {}
                }
                touched
            };
            {
                let doc = room.doc.lock().await;
                let mut store = state.store.lock().await;
                if let Err(e) = store.append_update(room.id, &update, None) {
                    warn!(conn_id, %e, "persisting update");
                }
                match store.maintain(room.id, &state.options.policy, now_ms(), || doc.encode_full()) {
                    Ok(m) if m.snapshotted || m.pruned_updates > 0 => {
                        info!(doc = %room.id, snapshotted = m.snapshotted, pruned = m.pruned_updates, "maintenance")
                    }
                    Ok(_) => {}
                    Err(e) => warn!(conn_id, %e, "maintenance"),
                }
            }
            let author = Author { user: &conn.user, touched: touched.unwrap_or_default() };
            if let Err(e) = derive_metadata(state, &room, Some(&author)).await {
                warn!(conn_id, %e, "indexing");
            }
            let out = Frame::new(&frame.doc_id, &Message::Sync(SyncMessage::Update(update))).encode();
            let _ =
                state.bus.send(Outbound { from: conn_id, doc_id: frame.doc_id.into(), bytes: Arc::new(out) });
            Vec::new()
        }
        Message::Awareness(_) => {
            let _ = state.bus.send(Outbound {
                from: conn_id,
                doc_id: frame.doc_id.into(),
                bytes: Arc::new(bytes.to_vec()),
            });
            Vec::new()
        }
        Message::AwarenessQuery | Message::Auth(_) | Message::Custom(..) => Vec::new(),
    }
}

/// Whether a vault-relative path is one every replica can write inside the vault's folder:
/// relative, no `.`/`..` or other hidden segment (`.lemmate`, `.git`), no empty segment, no
/// backslash, drive letter, NUL or other control character. The REST routes and the vault-doc
/// updates clients send are held to the same rule.
pub fn safe_vault_path(p: &str) -> bool {
    let drive = p.as_bytes().get(1) == Some(&b':') && p.as_bytes()[0].is_ascii_alphabetic();
    !p.is_empty()
        && !p.starts_with('/')
        && !drive
        && !p.contains('\\')
        && !p.chars().any(char::is_control)
        && p.split('/').all(|seg| !seg.is_empty() && !seg.starts_with('.'))
}

/// Look at what an update would do to a vault doc before it is applied. Every path it brings in
/// — a note's, an attachment's, a kept file's — must pass [`safe_vault_path`], or the whole
/// update is refused (`Err` names the first bad path): a client projects these paths onto its
/// disk. Paths the doc already holds are not judged again. `Ok` carries the notes whose entry
/// the update sets or changes.
fn vet_vault_update(current: &VaultDoc, update: &[u8]) -> Result<HashSet<NoteId>, String> {
    let full = current.encode_full();
    // An update that does not decode is refused when it is applied; nothing to judge here.
    let Ok(next) = VaultDoc::from_updates([full.as_slice(), update]) else {
        return Ok(HashSet::new());
    };
    let before: HashMap<NoteId, String> = current.entries().into_iter().collect();
    let mut known: HashSet<String> = before.values().cloned().collect();
    known.extend(current.attachment_entries().into_iter().map(|(p, _)| p));
    known.extend(current.kept_files());
    let mut touched = HashSet::new();
    for (id, path) in next.entries() {
        if before.get(&id) != Some(&path) {
            touched.insert(id);
            if !known.contains(&path) && !safe_vault_path(&path) {
                return Err(path);
            }
        }
    }
    for path in next.attachment_entries().into_iter().map(|(p, _)| p).chain(next.kept_files()) {
        if !known.contains(&path) && !safe_vault_path(&path) {
            return Err(path);
        }
    }
    Ok(touched)
}

async fn get_room(state: &Arc<AppState>, id: DocId) -> lemmate_core::Result<Arc<Room>> {
    let sweep = {
        let mut last = state.last_room_sweep.lock().unwrap_or_else(|e| e.into_inner());
        let due = last.elapsed() >= ROOM_SWEEP_EVERY;
        if due {
            *last = Instant::now();
        }
        due
    };
    if sweep {
        let n = state.evict_idle_rooms(ROOM_IDLE).await;
        if n > 0 {
            tracing::debug!(rooms = n, "dropped idle rooms");
        }
    }
    let key = id.to_string();
    let mut rooms = state.rooms.lock().await;
    if let Some(r) = rooms.get(&key) {
        r.last_used.store(now_ms(), Ordering::Relaxed);
        return Ok(r.clone());
    }
    let store = state.store.lock().await;
    let doc = match id {
        DocId::Note(_) => RoomDoc::Note(store.load_doc(id)?),
        DocId::Vault(v) => RoomDoc::Vault(store.load_vault_doc(v)?),
    };
    drop(store);
    let room = Arc::new(Room { id, doc: Mutex::new(doc), last_used: AtomicI64::new(now_ms()) });
    rooms.insert(key, room.clone());
    Ok(room)
}

/// Who a change synced by a client came from, for [`derive_metadata`]: the user, and the notes
/// whose vault-doc entry the change set (`vet_vault_update`).
struct Author<'a> {
    user: &'a AuthUser,
    touched: HashSet<NoteId>,
}

/// Whether a vault doc may take a note whose row is in another vault, `home` — a merge does
/// exactly that (SPEC §3.2). Only for a note the change itself put there, and only by someone
/// who may edit the vault it leaves: otherwise anyone could pull another vault's note into one
/// of their own by naming its id. A change the server made itself (`author` none) is trusted.
fn may_take(state: &AppState, store: &Store, author: Option<&Author>, id: NoteId, home: VaultId) -> bool {
    if matches!(state.options.auth, AuthMode::Disabled) {
        return true;
    }
    let Some(a) = author else { return true };
    a.touched.contains(&id) && auth::vault_role(store, a.user, home).is_some_and(|r| r >= Role::Editor)
}

/// Keep the relational tables (notes, tags, links, FTS) in step with the CRDT truth so the REST
/// and search endpoints reflect what clients synced (SPEC §4.2: metadata is derived, never a
/// second source of truth).
async fn derive_metadata(
    state: &Arc<AppState>,
    room: &Room,
    author: Option<&Author<'_>>,
) -> lemmate_core::Result<()> {
    let doc = room.doc.lock().await;
    let mut store = state.store.lock().await;
    let mut placed = Vec::new();
    let mut moved = false;
    match (&*doc, room.id) {
        (RoomDoc::Vault(v), DocId::Vault(vault_id)) => {
            let entries = v.entries();
            let live: std::collections::HashSet<NoteId> = entries.iter().map(|(id, _)| *id).collect();
            for row in store.list_notes(vault_id)? {
                if !live.contains(&row.id) {
                    store.trash_note(row.id)?;
                }
            }
            let files: HashMap<String, String> = v.attachment_entries().into_iter().collect();
            let attachment_paths: Vec<String> = files.keys().cloned().collect();
            let read = blob_reader(state, vault_id, &files);
            for (id, path) in entries {
                if let Some(home) = store.note_vault_of(id)?
                    && home != vault_id
                {
                    if !may_take(state, &store, author, id, home) {
                        warn!(note = %id, from = %home, into = %vault_id, "a vault doc names another vault's note; left where it is");
                        continue;
                    }
                    moved = true;
                }
                let existing = store.note_by_id(id)?;
                let title = existing.as_ref().and_then(|r| r.title.clone()).or_else(|| {
                    std::path::Path::new(&path).file_stem().map(|s| s.to_string_lossy().into_owned())
                });
                store.upsert_note(id, vault_id, &path, title.as_deref())?;
                placed.push(id);
                // Content may have arrived before the entry did (a browser creates the text
                // first): index it now that the row exists.
                if existing.is_none() {
                    let text = store.load_doc(DocId::Note(id))?.text();
                    if !text.is_empty() {
                        index_note_text(&mut store, id, &path, &text, &attachment_paths, &read, true)?;
                    }
                }
            }
            claim_waiting_files(state, &mut store, vault_id, &attachment_paths, &read)?;
        }
        (RoomDoc::Note(n), DocId::Note(id)) => {
            if let Some(row) = store.note_by_id(id)? {
                let files: HashMap<String, String> =
                    store.load_vault_doc(row.vault_id)?.attachment_entries().into_iter().collect();
                let attachment_paths: Vec<String> = files.keys().cloned().collect();
                let read = blob_reader(state, row.vault_id, &files);
                index_note_text(&mut store, id, &row.path, &n.text(), &attachment_paths, &read, true)?;
            } else {
                // No vault entry yet: keep the FTS/tags fresh; the title lands when the entry does.
                store.index_note(id, &markdown::index(&n.text())?)?;
            }
        }
        _ => {}
    }
    drop(store);
    drop(doc);
    // A note with a row needs no claim any more.
    if !placed.is_empty() {
        let mut claims = state.note_vault_claims.lock().await;
        for id in placed {
            claims.remove(&id);
        }
    }
    // A note that changed vaults changed who may read it.
    if moved {
        state.auth_changed();
    }
    Ok(())
}

/// Re-derive every note's metadata if the store's was written by an older indexer
/// ([`markdown::INDEX_VERSION`]): a note indexed before the indexer learned to read a construct
/// would otherwise keep its old tags, links and search text until somebody next edits it.
/// Returns how many notes were re-derived, or `None` when the store was already current.
pub fn reindex_if_stale(store: &mut Store) -> lemmate_core::Result<Option<usize>> {
    if store.index_is_current()? {
        return Ok(None);
    }
    let mut count = 0;
    for (vault_id, _) in store.vaults()? {
        let attachment_paths: Vec<String> =
            store.load_vault_doc(vault_id)?.attachment_entries().into_iter().map(|(p, _)| p).collect();
        for row in store.list_notes(vault_id)? {
            let text = store.load_doc(DocId::Note(row.id))?.text();
            // No blob store here: what a stylesheet imports is filled in at the note's next edit.
            index_note_text(store, row.id, &row.path, &text, &attachment_paths, &|_| None, false)?;
            count += 1;
        }
    }
    store.mark_index_current()?;
    Ok(Some(count))
}

/// Files can arrive after the notes that name them — the theme saved once the front matter
/// already says `theme: [default, cern.scss]`, the image copied in after its link — and a note
/// is indexed when *it* changes, so it would never learn it uses them. Here each file no note
/// uses yet, seen for the first time, is looked for in the vault's notes: the ones that mention
/// its name are indexed again, and so, when it is a stylesheet or YAML (which can be imported
/// by one a note uses), are the notes that use one of those.
fn claim_waiting_files(
    state: &AppState,
    store: &mut Store,
    vault: VaultId,
    attachment_paths: &[String],
    read: &dyn Fn(&str) -> Option<Vec<u8>>,
) -> lemmate_core::Result<()> {
    let pairs = store.note_attachment_paths_in(vault)?;
    let used: HashSet<&str> = pairs.iter().map(|(_, p)| p.as_str()).collect();
    let fresh: Vec<&String> = {
        let mut waiting = state.waiting_files.lock().unwrap_or_else(|e| e.into_inner());
        let seen = waiting.entry(vault).or_default();
        attachment_paths.iter().filter(|p| !used.contains(p.as_str()) && seen.insert((*p).clone())).collect()
    };
    if fresh.is_empty() {
        return Ok(());
    }
    let names: Vec<String> = fresh.iter().filter_map(|p| p.rsplit('/').next()).map(str::to_owned).collect();
    let styled: HashSet<NoteId> = if fresh.iter().any(|p| lemmate_core::attachments::names_files(p)) {
        pairs.iter().filter(|(_, p)| lemmate_core::attachments::names_files(p)).map(|(id, _)| *id).collect()
    } else {
        HashSet::new()
    };
    for row in store.list_notes(vault)? {
        let text = store.load_doc(DocId::Note(row.id))?.text();
        let mentions =
            names.iter().any(|n| text.contains(n.as_str()) || text.contains(&n.replace(' ', "%20")));
        if mentions || styled.contains(&row.id) {
            index_note_text(store, row.id, &row.path, &text, attachment_paths, read, false)?;
        }
    }
    Ok(())
}

/// Reads a vault file's bytes from the blob store by path, for following what a stylesheet
/// imports: `files` maps the vault's paths to their hashes.
fn blob_reader<'a>(
    state: &'a AppState,
    vault: VaultId,
    files: &'a HashMap<String, String>,
) -> impl Fn(&str) -> Option<Vec<u8>> + 'a {
    move |p: &str| files.get(p).and_then(|hash| state.attachments.get(vault, hash).ok().flatten())
}

/// Index one note's text: tags, links, FTS, title, and which vault attachments it references.
/// `edited` stamps the note as changed; a re-derivation of unchanged text does not.
fn index_note_text(
    store: &mut Store,
    id: NoteId,
    path: &str,
    text: &str,
    attachment_paths: &[String],
    read: &dyn Fn(&str) -> Option<Vec<u8>>,
    edited: bool,
) -> lemmate_core::Result<()> {
    let ix = markdown::index(text)?;
    if edited {
        store.index_note(id, &ix)?;
    } else {
        store.reindex_note(id, &ix)?;
    }
    let paths = lemmate_core::attachments::referenced(path, text, attachment_paths, read)?;
    store.set_note_attachments(id, &paths)
}

// ---- Writes through the API (SPEC §13.1) ------------------------------------------------------

/// Apply a change produced on the server to a room doc: journal it, run maintenance, derive
/// metadata, and fan it out to every subscriber exactly like a client update.
async fn commit_change(state: &Arc<AppState>, room: &Arc<Room>, update: Vec<u8>) -> Result<(), StatusCode> {
    if update.is_empty() {
        return Ok(());
    }
    {
        let doc = room.doc.lock().await;
        let mut store = state.store.lock().await;
        store.append_update(room.id, &update, Some("api")).map_err(internal)?;
        let _ = store.maintain(room.id, &state.options.policy, now_ms(), || doc.encode_full());
    }
    derive_metadata(state, room, None).await.map_err(internal)?;
    let doc_id = room.id.to_string();
    let frame = Frame::new(&doc_id, &Message::Sync(SyncMessage::Update(update))).encode();
    let _ = state.bus.send(Outbound { from: 0, doc_id: doc_id.into(), bytes: Arc::new(frame) });
    Ok(())
}

async fn note_room(state: &Arc<AppState>, id: NoteId) -> Result<Arc<Room>, StatusCode> {
    get_room(state, DocId::Note(id)).await.map_err(internal)
}

async fn vault_room(state: &Arc<AppState>, vault: VaultId) -> Result<Arc<Room>, StatusCode> {
    get_room(state, DocId::Vault(vault)).await.map_err(internal)
}

fn normalize_path(path: &str) -> Result<String, StatusCode> {
    let p = path.trim().trim_start_matches('/');
    if !safe_vault_path(p) {
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok(if p.ends_with(".md") || p.ends_with(".qmd") { p.to_owned() } else { format!("{p}.md") })
}

#[derive(Deserialize)]
struct NewNote {
    path: String,
    #[serde(default)]
    content: String,
}

async fn create_note(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Path(vault): Path<String>,
    Json(body): Json<NewNote>,
) -> Result<(StatusCode, Json<NoteBody>), StatusCode> {
    let vault: VaultId = vault.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    auth::require(&state, &user, vault, Role::Editor).await?;
    let path = normalize_path(&body.path)?;
    let vroom = vault_room(&state, vault).await?;
    // Held from the check to the entry being written, so two requests cannot both find the
    // path free (two clicks on today's daily note).
    let _creating = state.create_lock.lock().await;
    {
        let doc = vroom.doc.lock().await;
        if let RoomDoc::Vault(v) = &*doc
            && v.entries().iter().any(|(_, p)| *p == path)
        {
            return Err(StatusCode::CONFLICT);
        }
    }
    let (id, text) = create_note_in(&state, vault, &vroom, &path, &body.content).await?;
    let title = state.store.lock().await.note_by_id(id).map_err(internal)?.and_then(|r| r.title);
    Ok((StatusCode::CREATED, Json(NoteBody { id: id.to_string(), path, title, content: text })))
}

/// Create one note in a vault whose role the caller has already checked, and return its id and
/// the text as stored (front matter carries the id, SPEC §6.3). Shared by `create_note` and the
/// Obsidian importer.
async fn create_note_in(
    state: &Arc<AppState>,
    vault: VaultId,
    vroom: &Arc<Room>,
    path: &str,
    content: &str,
) -> Result<(NoteId, String), StatusCode> {
    let id = NoteId::new();
    let text =
        lemmate_core::frontmatter::normalize(content, &id.to_string()).unwrap_or_else(|| content.to_owned());
    // Content first, then the entry, so nobody sees an empty note (same order as the UI).
    let nroom = note_room(state, id).await?;
    state.note_vault_claims.lock().await.insert(id, (vault, Instant::now()));
    let update = match &*nroom.doc.lock().await {
        RoomDoc::Note(d) => d.set_text(&text),
        RoomDoc::Vault(_) => return Err(StatusCode::INTERNAL_SERVER_ERROR),
    };
    commit_change(state, &nroom, update).await?;
    let vupdate = match &*vroom.doc.lock().await {
        RoomDoc::Vault(v) => v.set_path(id, path),
        RoomDoc::Note(_) => return Err(StatusCode::INTERNAL_SERVER_ERROR),
    };
    commit_change(state, vroom, vupdate).await?;
    Ok((id, text))
}

/// Import an Obsidian vault from the browser (SPEC §11.4): a multipart body whose parts are the
/// files, each named by its vault-relative path. Conversion is `lemmate_core::import`, the same
/// code `lemmate import obsidian` runs; the notes it produces are created through the room docs
/// like any other API write. Idempotent: a path the vault already holds is skipped, so a
/// re-uploaded batch does not duplicate notes.
async fn import_vault(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Path(vault): Path<String>,
    mut form: Multipart,
) -> Result<Json<UploadReport>, StatusCode> {
    let vault: VaultId = vault.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    // Importing into a vault nobody owns claims it, exactly as a first sync does.
    match auth::role_or_claim(&state, &user, vault, true).await {
        None => return Err(StatusCode::NOT_FOUND),
        Some(r) if r < Role::Editor => return Err(StatusCode::FORBIDDEN),
        Some(_) => {}
    }
    let vroom = vault_room(&state, vault).await?;
    let mut out = UploadReport::default();
    while let Some(field) = form.next_field().await.map_err(|_| StatusCode::BAD_REQUEST)? {
        let rel = field.file_name().or_else(|| field.name()).unwrap_or_default().to_owned();
        let bytes = field.bytes().await.map_err(|_| StatusCode::BAD_REQUEST)?;
        let Some(upload) = import::import_upload(&rel, bytes.to_vec()) else {
            if import::upload_rejected(&rel) {
                out.skipped += 1;
            }
            continue;
        };
        match upload {
            Upload::Note { path, text, callouts, embeds } => {
                let _creating = state.create_lock.lock().await;
                let taken = match &*vroom.doc.lock().await {
                    RoomDoc::Vault(v) => v.entries().iter().any(|(_, p)| *p == path),
                    RoomDoc::Note(_) => return Err(StatusCode::INTERNAL_SERVER_ERROR),
                };
                if taken {
                    out.skipped += 1;
                    continue;
                }
                create_note_in(&state, vault, &vroom, &path, &text).await?;
                out.notes += 1;
                out.callouts += callouts;
                out.embeds += embeds;
            }
            Upload::Attachment { path, bytes } => {
                if bytes.len() as u64 > MAX_ATTACHMENT_BYTES {
                    out.skipped += 1;
                    continue;
                }
                let (hash, _) = state.attachments.put(vault, &bytes).map_err(internal)?;
                let row = AttachmentRow {
                    hash: hash.clone(),
                    size: bytes.len() as u64,
                    mime: mime_for_path(&path),
                    filename_hint: Some(path.clone()),
                };
                state.store.lock().await.upsert_attachment(vault, &row).map_err(internal)?;
                let update = match &*vroom.doc.lock().await {
                    RoomDoc::Vault(v) => v.set_attachment(&path, &hash),
                    RoomDoc::Note(_) => return Err(StatusCode::INTERNAL_SERVER_ERROR),
                };
                commit_change(&state, &vroom, update).await?;
                out.attachments += 1;
            }
            Upload::Bookmarks(marks) => {
                let (added, update) = match &*vroom.doc.lock().await {
                    RoomDoc::Vault(v) => {
                        let before = v.bookmarks().len();
                        let update = v.add_bookmarks(&marks);
                        (v.bookmarks().len() - before, update)
                    }
                    RoomDoc::Note(_) => return Err(StatusCode::INTERNAL_SERVER_ERROR),
                };
                commit_change(&state, &vroom, update).await?;
                out.bookmarks += added;
            }
            Upload::Daily(settings) => {
                let update = match &*vroom.doc.lock().await {
                    RoomDoc::Vault(v) => v.set_daily(&settings),
                    RoomDoc::Note(_) => return Err(StatusCode::INTERNAL_SERVER_ERROR),
                };
                commit_change(&state, &vroom, update).await?;
                out.daily_notes = true;
            }
        }
    }
    Ok(Json(out))
}

#[derive(Deserialize)]
struct PutNote {
    content: String,
}

/// Replace the text; applied as a diff so it merges with concurrent editors (SPEC §13.1).
async fn put_note(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Path((vault, id)): Path<(String, String)>,
    Json(body): Json<PutNote>,
) -> Result<Json<NoteBody>, StatusCode> {
    let vault: VaultId = vault.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    let id: NoteId = id.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    if auth::note_role(&state, &user, vault, id).await.is_none_or(|r| r < Role::Editor) {
        return Err(StatusCode::NOT_FOUND);
    }
    let row = state
        .store
        .lock()
        .await
        .note_by_id(id)
        .map_err(internal)?
        .filter(|r| r.vault_id == vault)
        .ok_or(StatusCode::NOT_FOUND)?;
    let room = note_room(&state, id).await?;
    let text =
        lemmate_core::frontmatter::normalize(&body.content, &id.to_string()).unwrap_or(body.content.clone());
    let update = match &*room.doc.lock().await {
        RoomDoc::Note(d) => d.set_text(&text),
        RoomDoc::Vault(_) => return Err(StatusCode::INTERNAL_SERVER_ERROR),
    };
    commit_change(&state, &room, update).await?;
    let title = state.store.lock().await.note_by_id(id).map_err(internal)?.and_then(|r| r.title);
    Ok(Json(NoteBody { id: id.to_string(), path: row.path, title, content: text }))
}

#[derive(Deserialize)]
struct PatchNote {
    path: String,
}

async fn patch_note(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Path((vault, id)): Path<(String, String)>,
    Json(body): Json<PatchNote>,
) -> Result<StatusCode, StatusCode> {
    let vault: VaultId = vault.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    let id: NoteId = id.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    auth::require(&state, &user, vault, Role::Editor).await?;
    let path = normalize_path(&body.path)?;
    let vroom = vault_room(&state, vault).await?;
    let update = match &*vroom.doc.lock().await {
        RoomDoc::Vault(v) => {
            if v.path_of(id).is_none() {
                return Err(StatusCode::NOT_FOUND);
            }
            if v.entries().iter().any(|(other, p)| *p == path && *other != id) {
                return Err(StatusCode::CONFLICT);
            }
            v.set_path(id, &path)
        }
        RoomDoc::Note(_) => return Err(StatusCode::INTERNAL_SERVER_ERROR),
    };
    commit_change(&state, &vroom, update).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn delete_note(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Path((vault, id)): Path<(String, String)>,
) -> Result<StatusCode, StatusCode> {
    let vault: VaultId = vault.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    let id: NoteId = id.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    auth::require(&state, &user, vault, Role::Editor).await?;
    let vroom = vault_room(&state, vault).await?;
    let update = match &*vroom.doc.lock().await {
        RoomDoc::Vault(v) => {
            if v.path_of(id).is_none() {
                return Err(StatusCode::NOT_FOUND);
            }
            v.remove(id)
        }
        RoomDoc::Note(_) => return Err(StatusCode::INTERNAL_SERVER_ERROR),
    };
    commit_change(&state, &vroom, update).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// The note for a day — `Daily/YYYY-MM-DD.md` unless the vault says otherwise — created with a
/// heading when missing (SPEC §9, §13.1).
async fn daily_note(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Path((vault, date)): Path<(String, String)>,
) -> Result<Json<NoteBody>, StatusCode> {
    let vault_id: VaultId = vault.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    auth::require(&state, &user, vault_id, Role::Editor).await?;
    let day = lemmate_core::daily::Date::parse(&date).ok_or(StatusCode::BAD_REQUEST)?;
    let vroom = vault_room(&state, vault_id).await?;
    let path = match &*vroom.doc.lock().await {
        RoomDoc::Vault(v) => v.daily().path_for(day),
        RoomDoc::Note(_) => return Err(StatusCode::INTERNAL_SERVER_ERROR),
    };
    if let Some(found) = existing_note_at(&state, vault_id, &path).await? {
        return Ok(found);
    }
    let created = create_note(
        State(state.clone()),
        user,
        Path(vault),
        Json(NewNote { path: path.clone(), content: format!("# {date}\n\n") }),
    )
    .await;
    match created {
        Ok((_, body)) => Ok(body),
        // Somebody else made it in the meantime: theirs is the day's note.
        Err(StatusCode::CONFLICT) => {
            existing_note_at(&state, vault_id, &path).await?.ok_or(StatusCode::CONFLICT)
        }
        Err(e) => Err(e),
    }
}

async fn existing_note_at(
    state: &Arc<AppState>,
    vault: VaultId,
    path: &str,
) -> Result<Option<Json<NoteBody>>, StatusCode> {
    let Some(row) = state.store.lock().await.note_by_path(vault, path).map_err(internal)? else {
        return Ok(None);
    };
    let room = note_room(state, row.id).await?;
    let content = match &*room.doc.lock().await {
        RoomDoc::Note(d) => d.text(),
        RoomDoc::Vault(_) => String::new(),
    };
    Ok(Some(Json(NoteBody { id: row.id.to_string(), path: row.path, title: row.title, content })))
}

#[derive(Serialize)]
struct TrashOut {
    id: String,
    path: String,
    title: Option<String>,
    deleted_at: String,
}

async fn list_trash(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Path(vault): Path<String>,
) -> Result<Json<Vec<TrashOut>>, StatusCode> {
    let vault: VaultId = vault.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    auth::require(&state, &user, vault, Role::Viewer).await?;
    let rows = state.store.lock().await.trashed_notes(vault).map_err(internal)?;
    Ok(Json(
        rows.into_iter()
            .map(|(n, d)| TrashOut { id: n.id.to_string(), path: n.path, title: n.title, deleted_at: d })
            .collect(),
    ))
}

/// Put a trashed note back into the vault doc (SPEC §9); its content never left the journal.
async fn restore_note(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Path((vault, id)): Path<(String, String)>,
) -> Result<Json<NoteSummary>, StatusCode> {
    let vault: VaultId = vault.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    let id: NoteId = id.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    auth::require(&state, &user, vault, Role::Editor).await?;
    let row = {
        let mut store = state.store.lock().await;
        // Whose note it is is asked before anything about it changes.
        if store.note_vault_of(id).map_err(internal)? != Some(vault) {
            return Err(StatusCode::NOT_FOUND);
        }
        store.restore_note(id).map_err(internal)?.ok_or(StatusCode::NOT_FOUND)?
    };
    let vroom = vault_room(&state, vault).await?;
    let update = match &*vroom.doc.lock().await {
        RoomDoc::Vault(v) => v.set_path(id, &row.path),
        RoomDoc::Note(_) => return Err(StatusCode::INTERNAL_SERVER_ERROR),
    };
    commit_change(&state, &vroom, update).await?;
    Ok(Json(NoteSummary {
        id: row.id.to_string(),
        path: row.path,
        title: row.title,
        updated_at: row.updated_at,
    }))
}

// ---- REST -------------------------------------------------------------------------------------

#[derive(Serialize)]
struct VaultSummary {
    id: String,
    notes: u32,
    /// The caller's role as this request may use it (a read-only token reads `viewer`), so a
    /// client can leave out what it could not do anyway.
    role: &'static str,
}

#[derive(Serialize)]
struct NoteSummary {
    id: String,
    path: String,
    title: Option<String>,
    /// Only the note listing carries this — it is what lets a client tell which of its cached
    /// copies have gone stale without fetching them (SPEC §6.4). Backlinks, tagged notes and
    /// search hits come from queries that do not select it, and answer `None`.
    #[serde(skip_serializing_if = "Option::is_none")]
    updated_at: Option<String>,
}

#[derive(Serialize)]
struct NoteBody {
    id: String,
    path: String,
    title: Option<String>,
    content: String,
}

#[derive(Deserialize)]
struct SearchParams {
    q: String,
    #[serde(default = "default_limit")]
    limit: u32,
}

fn default_limit() -> u32 {
    20
}

#[derive(Serialize)]
struct SearchHitOut {
    note_id: String,
    title: Option<String>,
    snippet: String,
}

async fn list_vaults(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
) -> Result<Json<Vec<VaultSummary>>, StatusCode> {
    let store = state.store.lock().await;
    let rows: Vec<(VaultId, Role, u32)> = match state.options.auth {
        AuthMode::Disabled => {
            store.vaults().map_err(internal)?.into_iter().map(|(v, n)| (v, Role::Owner, n)).collect()
        }
        AuthMode::Enabled { .. } => store
            .vaults_of(&user.id)
            .map_err(internal)?
            .into_iter()
            .filter(|(v, _, _)| user.reaches(*v))
            .map(|(v, r, n)| (v, user.cap(r), n))
            .collect(),
    };
    Ok(Json(
        rows.into_iter()
            .map(|(id, role, notes)| VaultSummary { id: id.to_string(), notes, role: role.as_str() })
            .collect(),
    ))
}

/// Erase a vault: its doc, its metadata, its members and its blobs (SPEC §3.2).
///
/// This exists for the end of a **merge**: the notes have already been re-parented into another
/// vault by that vault's doc, and what is left here is an empty shell that would otherwise be
/// pulled back down by every client on the next launch. Note *docs* are not deleted — the ids
/// belong to the destination vault now — so a vault deleted while it still holds notes takes
/// their listing with it, which is why only an owner may ask.
async fn delete_vault(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Path(vault): Path<String>,
) -> Result<StatusCode, StatusCode> {
    let vault: VaultId = vault.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    auth::require(&state, &user, vault, Role::Owner).await?;
    // Drop the room first: a live one would keep answering for a vault that no longer exists,
    // and would write its doc back out on the next update.
    state.rooms.lock().await.remove(&DocId::Vault(vault).to_string());
    let notes = state.store.lock().await.delete_vault(vault).map_err(internal)?;
    // The note docs it leaves behind have history and no row, so nobody can claim them by
    // syncing (`vault_of_note`); a provisional claim to this vault must not outlive it either.
    state.note_vault_claims.lock().await.retain(|_, (v, _)| *v != vault);
    state.auth_changed();
    state.attachments.remove_vault(vault).map_err(internal)?;
    tracing::info!(%vault, user = %user.email, notes, "vault deleted");
    Ok(StatusCode::NO_CONTENT)
}

async fn backlinks(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Path((vault, id)): Path<(String, String)>,
) -> Result<Json<Vec<NoteSummary>>, StatusCode> {
    let vault: VaultId = vault.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    auth::require(&state, &user, vault, Role::Viewer).await?;
    let id: NoteId = id.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    let store = state.store.lock().await;
    let note = store
        .note_by_id(id)
        .map_err(internal)?
        .filter(|n| n.vault_id == vault)
        .ok_or(StatusCode::NOT_FOUND)?;
    let rows = store.backlinks_to(&note).map_err(internal)?;
    Ok(Json(
        rows.into_iter()
            .map(|n| NoteSummary {
                id: n.id.to_string(),
                path: n.path,
                title: n.title,
                updated_at: n.updated_at,
            })
            .collect(),
    ))
}

#[derive(Deserialize)]
struct ExportIn {
    format: String,
    /// A render to look at in the app rather than to save (`quarto::RenderOptions::viewing`).
    #[serde(default)]
    view: bool,
    /// `?print-pdf` on a render opened as a page: open it to be printed (`quarto::for_print`).
    #[serde(default, rename = "print-pdf")]
    print: Option<String>,
}

/// Render a note through pandoc (SPEC §12). The note's bibliography — its own `bibliography:`,
/// else the vault's `export/references.bib` — and citation style are laid out from the blob
/// store for pandoc to read. Other attachments resolving against the blob store is future work;
/// today image links stay relative.
async fn export_note(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Path((vault, id)): Path<(String, String)>,
    Json(body): Json<ExportIn>,
) -> Result<impl IntoResponse, StatusCode> {
    let vault: VaultId = vault.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    let id: NoteId = id.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    if auth::note_role(&state, &user, vault, id).await.is_none() {
        return Err(StatusCode::NOT_FOUND);
    }
    let format = lemmate_core::pandoc::Format::parse(&body.format).ok_or(StatusCode::BAD_REQUEST)?;
    if !lemmate_core::pandoc::pandoc_available(state.options.pandoc.as_deref()) {
        return Err(StatusCode::NOT_IMPLEMENTED);
    }
    let row = note_in(&state, vault, id).await?;
    let room = note_room(&state, id).await?;
    let text = match &*room.doc.lock().await {
        RoomDoc::Note(d) => d.text(),
        RoomDoc::Vault(_) => return Err(StatusCode::NOT_FOUND),
    };
    let entries: HashMap<String, String> = match &*vault_room(&state, vault).await?.doc.lock().await {
        RoomDoc::Vault(v) => v.attachment_entries().into_iter().collect(),
        RoomDoc::Note(_) => return Err(StatusCode::NOT_FOUND),
    };
    let cites = lemmate_core::pandoc::citation_files(&row.path, &text, |p| entries.contains_key(p));
    let blobs = state.attachments.clone();
    let pandoc = state.options.pandoc.clone();
    let _slot = render_slot(&state).await?;
    let (bytes, mime) = tokio::task::spawn_blocking(move || {
        // Each file at its vault path under a scratch directory, removed afterwards.
        let dir = std::env::temp_dir().join(format!("lemmate-export-{}", NoteId::new()));
        let lay_out = |p: &String| -> Option<std::path::PathBuf> {
            let bytes = blobs.get(vault, entries.get(p)?).ok().flatten()?;
            let file = dir.join(p);
            std::fs::create_dir_all(file.parent()?).ok()?;
            std::fs::write(&file, bytes).ok()?;
            Some(file)
        };
        let opts = lemmate_core::pandoc::ExportOptions {
            pandoc,
            bibliography: cites.bibliography.iter().filter_map(lay_out).collect(),
            csl: cites.csl.as_ref().and_then(lay_out),
            ..Default::default()
        };
        let out = lemmate_core::pandoc::render(&text, format, &opts);
        let _ = std::fs::remove_dir_all(&dir);
        out
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .map_err(|e| {
        warn!(%e, "export");
        StatusCode::UNPROCESSABLE_ENTITY
    })?;
    let stem = std::path::Path::new(&row.path)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "note".into());
    let disposition = format!("attachment; filename=\"{}.{}\"", stem.replace('"', ""), format.extension());
    Ok(([(header::CONTENT_TYPE, mime.to_owned()), (header::CONTENT_DISPOSITION, disposition)], bytes))
}

/// Render a note through Quarto (SPEC §5.6): the attachments it references are laid out
/// beside it from the blob store, and so is the vault's `export/references.bib` when there is
/// one. 501 when rendering is switched off or quarto is missing; 422 carries Quarto's message.
async fn render_note(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Path((vault, id)): Path<(String, String)>,
    Json(body): Json<ExportIn>,
) -> Result<axum::response::Response, StatusCode> {
    let vault: VaultId = vault.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    let id: NoteId = id.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    if auth::note_role(&state, &user, vault, id).await.is_none() {
        return Err(StatusCode::NOT_FOUND);
    }
    // `"auto"`: what the note declares; `"preview"`: the page it declares (`quarto.rs`).
    let preview = body.format == "preview" || body.format == "auto";
    let format = if preview {
        lemmate_core::quarto::Format::Html
    } else {
        lemmate_core::quarto::Format::parse(&body.format).ok_or(StatusCode::BAD_REQUEST)?
    };
    if !state.options.quarto_enabled {
        return Err(StatusCode::NOT_IMPLEMENTED);
    }
    let row = note_in(&state, vault, id).await?;
    let text = match &*note_room(&state, id).await?.doc.lock().await {
        RoomDoc::Note(d) => d.text(),
        RoomDoc::Vault(_) => return Err(StatusCode::NOT_FOUND),
    };
    let format = match body.format.as_str() {
        "auto" => lemmate_core::quarto::declared_format(&text),
        "preview" => lemmate_core::quarto::preview_format(&text),
        _ => format,
    };
    let entries: HashMap<String, String> = match &*vault_room(&state, vault).await?.doc.lock().await {
        RoomDoc::Vault(v) => v.attachment_entries().into_iter().collect(),
        RoomDoc::Note(_) => return Err(StatusCode::NOT_FOUND),
    };
    let blobs = state.attachments.clone();
    let bin = state.options.quarto.clone();
    let path = row.path.clone();
    let view = body.view;
    let _slot = render_slot(&state).await?;
    let rendered = tokio::task::spawn_blocking(move || {
        if !lemmate_core::quarto::quarto_available(bin.as_deref()) {
            return Ok(None);
        }
        let paths: Vec<String> = entries.keys().cloned().collect();
        let read = |p: &str| entries.get(p).and_then(|hash| blobs.get(vault, hash).ok().flatten());
        let opts =
            lemmate_core::quarto::RenderOptions { quarto: bin.clone(), viewing: view, ..Default::default() };
        lemmate_core::quarto::render(&path, &text, format, &paths, read, &opts).map(Some)
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    match rendered {
        Ok(Some((bytes, mime))) => {
            let disposition = lemmate_core::quarto::disposition(&row.path, format);
            // A page made to be looked at is kept, so opening it again elsewhere is instant; a
            // deck's speaker view shows its previews from that kept page, so it names it.
            let kept = view.then(lemmate_core::quarto::RenderCache::new_id);
            let bytes = match (&kept, format) {
                (Some(k), lemmate_core::quarto::Format::RevealJs) => {
                    lemmate_core::quarto::with_speaker(bytes, &vault.to_string(), &id.to_string(), k)
                }
                _ => bytes,
            };
            if let Some(k) = &kept {
                state.renders.put_as(k, &id.to_string(), &bytes, mime, &disposition);
            }
            let mut response = (
                [(header::CONTENT_TYPE, mime.to_owned()), (header::CONTENT_DISPOSITION, disposition)],
                bytes,
            )
                .into_response();
            if let Some(kept) = kept.and_then(|k| header::HeaderValue::from_str(&k).ok()) {
                response.headers_mut().insert("x-render-id", kept);
            }
            Ok(response)
        }
        Ok(None) => Err(StatusCode::NOT_IMPLEMENTED),
        Err(e) => {
            warn!(%e, "quarto render");
            // Quarto's own words, not wrapped in ours: the pane shows them as they are.
            let msg = match e {
                lemmate_core::Error::Export(m) => m,
                other => other.to_string(),
            };
            Ok((StatusCode::UNPROCESSABLE_ENTITY, without_temp_paths(&msg)).into_response())
        }
    }
}

/// The live note `id`, provided it is in `vault`.
async fn note_in(
    state: &AppState,
    vault: VaultId,
    id: NoteId,
) -> Result<lemmate_core::store::NoteRow, StatusCode> {
    state
        .store
        .lock()
        .await
        .note_by_id(id)
        .map_err(internal)?
        .filter(|r| r.vault_id == vault)
        .ok_or(StatusCode::NOT_FOUND)
}

/// A turn at pandoc or Quarto (`RENDER_SLOTS` at once): each run is a process tree that can take
/// a CPU and a good deal of memory for many seconds. 503 when none frees up in `RENDER_WAIT`.
async fn render_slot(state: &AppState) -> Result<tokio::sync::SemaphorePermit<'_>, StatusCode> {
    match tokio::time::timeout(RENDER_WAIT, state.render_slots.acquire()).await {
        Ok(Ok(permit)) => Ok(permit),
        _ => Err(StatusCode::SERVICE_UNAVAILABLE),
    }
}

/// A render's error message without this server's scratch paths: Quarto names the files it was
/// working on by their full path, which says where (and as whom) the server runs. Each path into
/// the temp directory is cut back to what follows its working folder — the note's own path.
fn without_temp_paths(msg: &str) -> String {
    let tmp = std::env::temp_dir();
    let mut prefixes = vec![tmp.to_string_lossy().trim_end_matches('/').to_owned()];
    // macOS hands out /var/… and reports /private/var/…; either can appear.
    if let Ok(real) = tmp.canonicalize() {
        prefixes.push(real.to_string_lossy().trim_end_matches('/').to_owned());
    }
    prefixes.retain(|p| !p.is_empty());
    let mut out = msg.to_owned();
    for prefix in prefixes {
        let needle = format!("{prefix}/");
        while let Some(at) = out.find(&needle) {
            let rest = &out[at + needle.len()..];
            // Skip the working folder (`notes-export-…`) as well, when there is one.
            let end = rest
                .find(|c: char| c == '/' || c.is_whitespace() || c == '\'' || c == '"')
                .filter(|&i| rest[i..].starts_with('/'))
                .map_or(0, |i| i + 1);
            out.replace_range(at..at + needle.len() + end, "");
        }
    }
    out
}

/// A render the pane already has, opened as a page of its own without rendering it again
/// (`x-render-id` on the pane's render). Kept renders expire; then it is rendered as the query's
/// `format` says, as `render_page` would.
async fn kept_render(
    State(state): State<Arc<AppState>>,
    user: Result<AuthUser, StatusCode>,
    uri: axum::http::Uri,
    Path((vault, id, render)): Path<(String, String, String)>,
    q: Query<ExportIn>,
) -> Result<axum::response::Response, StatusCode> {
    let Ok(user) = user else { return Ok(sign_in_first(&uri)) };
    let vault_id: VaultId = vault.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    let note: NoteId = id.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    if auth::note_role(&state, &user, vault_id, note).await.is_none() {
        return Err(StatusCode::NOT_FOUND);
    }
    let Some((bytes, mime, disposition)) = state.renders.get(&render, &note.to_string()) else {
        return render_page(State(state), Ok(user), uri, Path((vault, id)), q).await;
    };
    let response = (
        [
            (header::CONTENT_TYPE, mime.to_owned()),
            (header::CONTENT_DISPOSITION, disposition),
            (header::CONTENT_SECURITY_POLICY, lemmate_core::quarto::PAGE_SANDBOX.to_owned()),
        ],
        bytes,
    )
        .into_response();
    Ok(if q.print.is_some() { lemmate_core::local::printable(response).await } else { response })
}

/// A render page opened where there is no session — another browser than the one signed in, as
/// an iPhone's "open in the browser" is — goes to the sign-in, which comes back here after
/// (`next`, which the web client only follows to a render).
fn sign_in_first(uri: &axum::http::Uri) -> axum::response::Response {
    let back = uri.path_and_query().map(|p| p.as_str()).unwrap_or("/");
    let next: String = back
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect();
    axum::response::Redirect::to(&format!("/?next={next}")).into_response()
}

/// A render opened as a page of its own — a browser tab rather than the app's frame, which some
/// browsers (WebKit on iOS) will not repaint as a deck turns its slides. `?format=` as for the
/// POST; the page is sandboxed by its headers (`quarto::PAGE_SANDBOX`) as the frame is by its
/// attribute.
async fn render_page(
    state: State<Arc<AppState>>,
    user: Result<AuthUser, StatusCode>,
    uri: axum::http::Uri,
    path: Path<(String, String)>,
    Query(q): Query<ExportIn>,
) -> Result<axum::response::Response, StatusCode> {
    let Ok(user) = user else { return Ok(sign_in_first(&uri)) };
    let print = q.print.is_some();
    let mut response = render_note(state, user, path, Json(ExportIn { view: true, ..q })).await?;
    response.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        header::HeaderValue::from_static(lemmate_core::quarto::PAGE_SANDBOX),
    );
    Ok(if print { lemmate_core::local::printable(response).await } else { response })
}

#[derive(Serialize)]
struct VersionOut {
    seq: i64,
    created_ms: i64,
    label: Option<String>,
    author: Option<String>,
}

impl From<lemmate_core::store::VersionRow> for VersionOut {
    fn from(v: lemmate_core::store::VersionRow) -> Self {
        VersionOut { seq: v.seq, created_ms: v.created_ms, label: v.label, author: v.author }
    }
}

#[derive(Deserialize)]
struct SaveVersion {
    #[serde(default)]
    label: Option<String>,
}

/// History is the note's: the caller needs a role on the note itself, and the note has to be in
/// the vault the path names — a role on one vault does not reach another's notes by id.
async fn version_access(
    state: &AppState,
    user: &AuthUser,
    vault: &str,
    id: &str,
    min: Role,
) -> Result<NoteId, StatusCode> {
    let vault: VaultId = vault.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    let id: NoteId = id.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    let role = auth::note_role(state, user, vault, id).await.ok_or(StatusCode::NOT_FOUND)?;
    let row = state.store.lock().await.note_by_id(id).map_err(internal)?;
    if row.is_none_or(|n| n.vault_id != vault) {
        return Err(StatusCode::NOT_FOUND);
    }
    if role < min {
        return Err(StatusCode::FORBIDDEN);
    }
    Ok(id)
}

/// The note's history as people read it: snapshots folded into editing sessions, each with what
/// changed since the one before (`lemmate_core::history`).
async fn list_versions(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Path((vault, id)): Path<(String, String)>,
) -> Result<Json<Vec<history::Entry>>, StatusCode> {
    let id = version_access(&state, &user, &vault, &id, Role::Viewer).await?;
    // Only the read holds the store; decoding and diffing every session does not.
    let journal = state.store.lock().await.journal(DocId::Note(id)).map_err(internal)?;
    let entries = tokio::task::spawn_blocking(move || history::entries(&journal))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .map_err(internal)?;
    Ok(Json(entries))
}

/// Snapshot the note now with a label (SPEC §9 "save version"); kept forever.
async fn save_version(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Path((vault, id)): Path<(String, String)>,
    Json(body): Json<SaveVersion>,
) -> Result<Json<VersionOut>, StatusCode> {
    let id = version_access(&state, &user, &vault, &id, Role::Editor).await?;
    let room = get_room(&state, DocId::Note(id)).await.map_err(internal)?;
    let doc = room.doc.lock().await;
    let label = history::clean_label(body.label.as_deref()).unwrap_or_else(|| "saved version".to_owned());
    let saved = state
        .store
        .lock()
        .await
        .save_version(DocId::Note(id), &doc.encode_full(), now_ms(), &label, Some(&user.display_name))
        .map_err(internal)?;
    match saved {
        Saved::New(v) => Ok(Json(v.into())),
        // Nothing changed since the last named version, which a save must not rename.
        Saved::Unchanged(_) => Err(StatusCode::CONFLICT),
    }
}

/// Name a version, rename it, or (`"label": null`, or blank) take its name away, after which it
/// is an ordinary snapshot again and pruned like one.
async fn label_version(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Path((vault, id, seq)): Path<(String, String, i64)>,
    Json(body): Json<SaveVersion>,
) -> Result<Json<VersionOut>, StatusCode> {
    let id = version_access(&state, &user, &vault, &id, Role::Editor).await?;
    let label = history::clean_label(body.label.as_deref());
    let row = state
        .store
        .lock()
        .await
        .set_version_label(DocId::Note(id), seq, label.as_deref())
        .map_err(internal)?
        .ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(row.into()))
}

#[derive(Serialize)]
struct VersionBody {
    seq: i64,
    content: String,
}

async fn get_version(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Path((vault, id, seq)): Path<(String, String, i64)>,
) -> Result<Json<VersionBody>, StatusCode> {
    let id = version_access(&state, &user, &vault, &id, Role::Viewer).await?;
    let doc = state.store.lock().await.load_doc_at(DocId::Note(id), seq).map_err(internal)?;
    Ok(Json(VersionBody { seq, content: doc.text() }))
}

#[derive(Serialize)]
struct TagCount {
    tag: String,
    count: u32,
}

async fn tags(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Path(vault): Path<String>,
) -> Result<Json<Vec<TagCount>>, StatusCode> {
    let vault: VaultId = vault.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    auth::require(&state, &user, vault, Role::Viewer).await?;
    let rows = state.store.lock().await.tags_in_vault(vault).map_err(internal)?;
    Ok(Json(rows.into_iter().map(|(tag, count)| TagCount { tag, count }).collect()))
}

#[derive(Deserialize)]
struct TagParams {
    tag: String,
}

async fn tagged(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Path(vault): Path<String>,
    Query(p): Query<TagParams>,
) -> Result<Json<Vec<NoteSummary>>, StatusCode> {
    let vault: VaultId = vault.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    auth::require(&state, &user, vault, Role::Viewer).await?;
    let rows = state.store.lock().await.notes_with_tag(vault, &p.tag).map_err(internal)?;
    Ok(Json(
        rows.into_iter()
            .map(|n| NoteSummary {
                id: n.id.to_string(),
                path: n.path,
                title: n.title,
                updated_at: n.updated_at,
            })
            .collect(),
    ))
}

async fn search_vault(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Path(vault): Path<String>,
    Query(p): Query<SearchParams>,
) -> Result<Json<Vec<SearchHitOut>>, StatusCode> {
    let vault: VaultId = vault.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    auth::require(&state, &user, vault, Role::Viewer).await?;
    let hits = state
        .store
        .lock()
        .await
        .search_in_vault(vault, &p.q, p.limit.min(100))
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    Ok(Json(
        hits.into_iter()
            .map(|h| SearchHitOut { note_id: h.note_id.to_string(), title: h.title, snippet: h.snippet })
            .collect(),
    ))
}

async fn list_notes(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Path(vault): Path<String>,
) -> Result<Json<Vec<NoteSummary>>, StatusCode> {
    let vault: VaultId = vault.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    auth::require(&state, &user, vault, Role::Viewer).await?;
    let rows = state.store.lock().await.list_notes(vault).map_err(internal)?;
    Ok(Json(
        rows.into_iter()
            .map(|n| NoteSummary {
                id: n.id.to_string(),
                path: n.path,
                title: n.title,
                updated_at: n.updated_at,
            })
            .collect(),
    ))
}

async fn get_note(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Path((vault, id)): Path<(String, String)>,
) -> Result<Json<NoteBody>, StatusCode> {
    let vault: VaultId = vault.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    let id: NoteId = id.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    if auth::note_role(&state, &user, vault, id).await.is_none() {
        return Err(StatusCode::NOT_FOUND);
    }
    let row = state
        .store
        .lock()
        .await
        .list_notes(vault)
        .map_err(internal)?
        .into_iter()
        .find(|n| n.id == id)
        .ok_or(StatusCode::NOT_FOUND)?;
    let room = get_room(&state, DocId::Note(id)).await.map_err(internal)?;
    let content = match &*room.doc.lock().await {
        RoomDoc::Note(d) => d.text(),
        RoomDoc::Vault(_) => return Err(StatusCode::NOT_FOUND),
    };
    Ok(Json(NoteBody { id: id.to_string(), path: row.path, title: row.title, content }))
}

async fn search(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Query(p): Query<SearchParams>,
) -> Result<Json<Vec<SearchHitOut>>, StatusCode> {
    let store = state.store.lock().await;
    let hits = match state.options.auth {
        AuthMode::Disabled => store.search(&p.q, p.limit.min(100)).map_err(|_| StatusCode::BAD_REQUEST)?,
        AuthMode::Enabled { .. } => {
            let mut all = Vec::new();
            let member_of: HashSet<VaultId> =
                store.vaults_of(&user.id).map_err(internal)?.into_iter().map(|(v, _, _)| v).collect();
            for v in member_of.iter().filter(|v| user.reaches(**v)) {
                all.extend(
                    store.search_in_vault(*v, &p.q, p.limit.min(100)).map_err(|_| StatusCode::BAD_REQUEST)?,
                );
            }
            // Notes shared with the user directly, from vaults they are not a member of: the
            // vault is searched and only those notes kept.
            let mut shared: HashMap<VaultId, HashSet<NoteId>> = HashMap::new();
            for (n, _) in store.notes_shared_with(&user.id).map_err(internal)? {
                if !member_of.contains(&n.vault_id) && user.reaches(n.vault_id) {
                    shared.entry(n.vault_id).or_default().insert(n.id);
                }
            }
            for (v, ids) in shared {
                let hits = store.search_in_vault(v, &p.q, 1000).map_err(|_| StatusCode::BAD_REQUEST)?;
                all.extend(hits.into_iter().filter(|h| ids.contains(&h.note_id)));
            }
            all.sort_by(|a, b| a.rank.partial_cmp(&b.rank).unwrap_or(std::cmp::Ordering::Equal));
            all.truncate(p.limit.min(100) as usize);
            all
        }
    };
    Ok(Json(
        hits.into_iter()
            .map(|h| SearchHitOut { note_id: h.note_id.to_string(), title: h.title, snippet: h.snippet })
            .collect(),
    ))
}

/// Idempotent, content-addressed upload: the URL names the blake3 hash the body must have.
async fn put_attachment(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Path((vault, hash)): Path<(String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, StatusCode> {
    let vault: VaultId = vault.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    auth::require(&state, &user, vault, Role::Editor).await?;
    if !is_valid_hash(&hash) || hash_bytes(&body) != hash {
        return Err(StatusCode::BAD_REQUEST);
    }
    let filename_hint = headers.get("x-filename").and_then(|v| v.to_str().ok()).map(str::to_owned);
    let mime = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .filter(|m| !m.is_empty() && *m != "application/octet-stream")
        .map(str::to_owned)
        .or_else(|| filename_hint.as_deref().map(mime_for_path))
        .unwrap_or_else(|| "application/octet-stream".to_owned());
    let (_, created) = state.attachments.put(vault, &body).map_err(internal)?;
    let row = AttachmentRow { hash, size: body.len() as u64, mime, filename_hint };
    state.store.lock().await.upsert_attachment(vault, &row).map_err(internal)?;
    Ok(if created { StatusCode::CREATED } else { StatusCode::OK })
}

async fn get_attachment(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Path((vault, hash)): Path<(String, String)>,
) -> Result<impl IntoResponse, StatusCode> {
    let vault: VaultId = vault.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    auth::require(&state, &user, vault, Role::Viewer).await?;
    if !is_valid_hash(&hash) {
        return Err(StatusCode::BAD_REQUEST);
    }
    let bytes = state.attachments.get(vault, &hash).map_err(internal)?.ok_or(StatusCode::NOT_FOUND)?;
    let mime = state
        .store
        .lock()
        .await
        .attachment(vault, &hash)
        .map_err(internal)?
        .map(|r| r.mime)
        .unwrap_or_else(|| "application/octet-stream".to_owned());
    Ok((blob_headers(&mime), bytes))
}

/// Types a browser may show in place from this origin: they run no script. Everything else —
/// HTML, SVG, XML, JavaScript, whatever an uploader claimed — is served as a download.
fn inline_safe(mime: &str) -> bool {
    let essence = mime.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    matches!(
        essence.as_str(),
        "image/png"
            | "image/jpeg"
            | "image/gif"
            | "image/webp"
            | "image/avif"
            | "image/bmp"
            | "image/x-icon"
            | "image/vnd.microsoft.icon"
            | "application/pdf"
            | "text/plain"
    ) || essence.starts_with("audio/")
        || essence.starts_with("video/")
}

/// Headers for a vault file's bytes, which anyone with edit rights chose (SPEC §9). The stored
/// type is only trusted to be shown in place when it cannot run script (`inline_safe`); the rest
/// downloads. Either way the response is sandboxed and never sniffed — a PDF only escapes the
/// sandbox because browsers' PDF viewers refuse to run inside one — and it is cached privately:
/// it was fetched with somebody's session.
fn blob_headers(mime: &str) -> HeaderMap {
    let inline = inline_safe(mime);
    let pdf = mime.trim().to_ascii_lowercase().starts_with("application/pdf");
    let mut h = HeaderMap::new();
    let value = |v: &str| {
        header::HeaderValue::from_str(v)
            .unwrap_or(header::HeaderValue::from_static("application/octet-stream"))
    };
    h.insert(header::CONTENT_TYPE, value(mime));
    h.insert(header::CACHE_CONTROL, header::HeaderValue::from_static("private, max-age=31536000, immutable"));
    h.insert(header::X_CONTENT_TYPE_OPTIONS, header::HeaderValue::from_static("nosniff"));
    h.insert(
        header::CONTENT_DISPOSITION,
        header::HeaderValue::from_static(if inline { "inline" } else { "attachment" }),
    );
    if !pdf {
        h.insert(header::CONTENT_SECURITY_POLICY, header::HeaderValue::from_static("sandbox"));
    }
    h
}

// ---- Files that are not notes (SPEC §9) ---------------------------------------------------------

#[derive(Deserialize)]
struct FileQuery {
    path: String,
}

/// Every file of the vault that is not a note, with the notes that use it.
async fn list_files(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Path(vault): Path<String>,
) -> Result<Json<Vec<lemmate_core::files::FileEntry>>, StatusCode> {
    let vault: VaultId = vault.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    auth::require(&state, &user, vault, Role::Viewer).await?;
    let (entries, kept) = match &*vault_room(&state, vault).await?.doc.lock().await {
        RoomDoc::Vault(v) => (v.attachment_entries(), v.kept_files()),
        RoomDoc::Note(_) => return Err(StatusCode::NOT_FOUND),
    };
    let mut store = state.store.lock().await;
    // The first listing after a start is also the first chance to notice a file that arrived
    // after its note, before this process was there to see it arrive.
    {
        let files: HashMap<String, String> = entries.iter().cloned().collect();
        let paths: Vec<String> = files.keys().cloned().collect();
        let read = blob_reader(&state, vault, &files);
        claim_waiting_files(&state, &mut store, vault, &paths, &read).map_err(internal)?;
    }
    let users = store.note_attachment_paths_in(vault).map_err(internal)?;
    let list = lemmate_core::files::listing(entries, &kept, &users, |_, hash| {
        store.attachment(vault, hash).ok().flatten().map(|r| r.size)
    });
    Ok(Json(list))
}

#[derive(Serialize)]
struct FileOut {
    path: String,
    hash: String,
}

/// Put a file at a path the caller chose (SPEC §9), kept whether or not a note uses it. A file
/// already there is a 409 unless `x-replace: true`; with `x-base-hash`, it is a 409 too when the
/// file changed since the caller read it — the answer carries the hash it has now, so a save
/// that would overwrite someone else's can ask first.
async fn put_file(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Path(vault): Path<String>,
    Query(q): Query<FileQuery>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<axum::response::Response, StatusCode> {
    let vault: VaultId = vault.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    auth::require(&state, &user, vault, Role::Editor).await?;
    let path = lemmate_core::files::file_path(&q.path).ok_or(StatusCode::BAD_REQUEST)?;
    let header = |name: &str| headers.get(name).and_then(|v| v.to_str().ok()).map(str::to_owned);
    let replace = header("x-replace").is_some_and(|v| v == "true" || v == "1");
    let base = header("x-base-hash");
    let vroom = vault_room(&state, vault).await?;
    let current = match &*vroom.doc.lock().await {
        RoomDoc::Vault(v) => v.attachment_hash(&path),
        RoomDoc::Note(_) => return Err(StatusCode::NOT_FOUND),
    };
    if let Some(current) = &current
        && (!replace || base.as_ref().is_some_and(|b| b != current))
    {
        return Ok((StatusCode::CONFLICT, Json(serde_json::json!({ "current": current }))).into_response());
    }
    let (hash, _) = state.attachments.put(vault, &body).map_err(internal)?;
    let row = AttachmentRow {
        hash: hash.clone(),
        size: body.len() as u64,
        mime: mime_for_path(&path),
        filename_hint: path.rsplit('/').next().map(str::to_owned),
    };
    state.store.lock().await.upsert_attachment(vault, &row).map_err(internal)?;
    let update = match &*vroom.doc.lock().await {
        RoomDoc::Vault(v) => v.put_kept_file(&path, &hash),
        RoomDoc::Note(_) => return Err(StatusCode::NOT_FOUND),
    };
    commit_change(&state, &vroom, update).await?;
    let status = if current.is_some() { StatusCode::OK } else { StatusCode::CREATED };
    Ok((status, Json(FileOut { path, hash })).into_response())
}

/// Take a file out of the vault. Its bytes stay in the blob store until the orphan purge, and a
/// note that still links to it shows a broken link — the file manager warns before asking.
async fn delete_file(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Path(vault): Path<String>,
    Query(q): Query<FileQuery>,
) -> Result<StatusCode, StatusCode> {
    let vault: VaultId = vault.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    auth::require(&state, &user, vault, Role::Editor).await?;
    let vroom = vault_room(&state, vault).await?;
    let update = match &*vroom.doc.lock().await {
        RoomDoc::Vault(v) => v.delete_file(&q.path),
        RoomDoc::Note(_) => return Err(StatusCode::NOT_FOUND),
    };
    if update.is_empty() {
        return Err(StatusCode::NOT_FOUND);
    }
    commit_change(&state, &vroom, update).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct MoveFile {
    from: String,
    to: String,
}

#[derive(Serialize)]
struct Moved {
    path: String,
    /// How many notes had their references rewritten.
    rewritten: usize,
}

/// Rename or move a file, and point every note that uses it at the new path — links, embeds and
/// front matter (`lemmate_core::files::rewrite_references`). 409 when `to` is taken.
async fn move_file(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Path(vault): Path<String>,
    Json(body): Json<MoveFile>,
) -> Result<Json<Moved>, StatusCode> {
    let vault: VaultId = vault.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    auth::require(&state, &user, vault, Role::Editor).await?;
    let to = lemmate_core::files::file_path(&body.to).ok_or(StatusCode::BAD_REQUEST)?;
    let from = body.from;
    if from == to {
        return Ok(Json(Moved { path: to, rewritten: 0 }));
    }
    let vroom = vault_room(&state, vault).await?;
    let (files, update) = match &*vroom.doc.lock().await {
        RoomDoc::Vault(v) => {
            if v.attachment_hash(&to).is_some() {
                return Err(StatusCode::CONFLICT);
            }
            let files: Vec<String> = v.attachment_entries().into_iter().map(|(p, _)| p).collect();
            (files, v.move_file(&from, &to).ok_or(StatusCode::NOT_FOUND)?)
        }
        RoomDoc::Note(_) => return Err(StatusCode::NOT_FOUND),
    };
    let users: Vec<NoteId> = state
        .store
        .lock()
        .await
        .note_attachment_paths_in(vault)
        .map_err(internal)?
        .into_iter()
        .filter(|(_, p)| *p == from)
        .map(|(id, _)| id)
        .collect();
    commit_change(&state, &vroom, update).await?;
    let mut rewritten = 0;
    for id in users {
        let Some(row) = state.store.lock().await.note_by_id(id).map_err(internal)? else { continue };
        let room = note_room(&state, id).await?;
        let update = match &*room.doc.lock().await {
            RoomDoc::Note(d) => {
                match lemmate_core::files::rewrite_references(&row.path, &d.text(), &from, &to, &files) {
                    Some(text) => d.set_text(&text),
                    None => continue,
                }
            }
            RoomDoc::Vault(_) => continue,
        };
        commit_change(&state, &room, update).await?;
        rewritten += 1;
    }
    Ok(Json(Moved { path: to, rewritten }))
}

fn internal(e: lemmate_core::Error) -> StatusCode {
    warn!(%e, "internal error");
    StatusCode::INTERNAL_SERVER_ERROR
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vault_paths_stay_inside_the_folder() {
        for ok in ["a.md", "Projects/Plan.md", "attachments/logo 2.png", "Notes...md", "ü/ß.qmd"] {
            assert!(safe_vault_path(ok), "{ok}");
        }
        for bad in [
            "",
            "/abs.md",
            "../up.md",
            "a/../b.md",
            "./a.md",
            ".lemmate/x.md",
            "a/.git/config",
            "a//b.md",
            "a/",
            "a\\b.md",
            "nul\0.md",
            "line\nbreak.md",
            "C:/x.md",
        ] {
            assert!(!safe_vault_path(bad), "{bad:?}");
        }
    }

    #[test]
    fn render_errors_do_not_name_the_servers_temp_dir() {
        let tmp = std::env::temp_dir();
        let tmp = tmp.to_string_lossy();
        let tmp = tmp.trim_end_matches('/');
        let msg = format!(
            "ERROR: {tmp}/notes-export-01K6ABCDEFGHJKMNPQRSTVWXYZ/Talks/deck.qmd: bad YAML\nsee {tmp}/x.log"
        );
        let clean = without_temp_paths(&msg);
        assert_eq!(clean, "ERROR: Talks/deck.qmd: bad YAML\nsee x.log");
        assert!(!clean.contains(tmp));
    }

    #[test]
    fn only_inert_types_are_shown_in_place() {
        for ok in ["image/png", "image/jpeg", "application/pdf", "text/plain; charset=utf-8", "video/mp4"] {
            assert!(inline_safe(ok), "{ok}");
        }
        for bad in
            ["text/html", "image/svg+xml", "application/xml", "text/javascript", "application/octet-stream"]
        {
            assert!(!inline_safe(bad), "{bad}");
        }
    }
}

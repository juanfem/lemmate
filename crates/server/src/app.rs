//! Router, shared state, WebSocket relay, and REST handlers (SPEC §7, §13.1).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::auth::{self, AuthMode, AuthUser};
use axum::body::Bytes;
use axum::extract::ws::{Message as WsMessage, WebSocket, WebSocketUpgrade};
use axum::extract::{DefaultBodyLimit, Multipart, Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use lemmate_core::attachments::{
    AttachmentStore, MAX_ATTACHMENT_BYTES, hash_bytes, is_valid_hash, mime_for_path,
};
use lemmate_core::import::{self, Upload, UploadReport};
use lemmate_core::store::{AttachmentRow, Role, now_ms};
use lemmate_core::sync::{Frame, Message, SyncMessage};
use lemmate_core::{DocId, NoteDoc, NoteId, RetentionPolicy, Store, VaultDoc, VaultId, markdown};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, broadcast};
use tower_http::services::{ServeDir, ServeFile};
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

pub struct AppState {
    pub store: Mutex<Store>,
    pub options: ServerOptions,
    pub attachments: AttachmentStore,
    /// Note docs seen before their vault entry exists, bound to the vault the creating
    /// connection was working in (a UI writes the note text before the vault map entry).
    pub note_vault_claims: Mutex<HashMap<NoteId, VaultId>>,
    rooms: Mutex<HashMap<String, Arc<Room>>>,
    bus: broadcast::Sender<Outbound>,
    next_conn: AtomicU64,
}

struct Room {
    id: DocId,
    doc: Mutex<RoomDoc>,
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
    Arc::new(AppState {
        store: Mutex::new(store),
        options,
        attachments,
        note_vault_claims: Mutex::new(HashMap::new()),
        rooms: Mutex::new(HashMap::new()),
        bus,
        next_conn: AtomicU64::new(1),
    })
}

pub fn router(state: Arc<AppState>) -> Router {
    let web_dir = state.options.web_dir.clone();
    let router = Router::new()
        .merge(auth::router())
        .route("/healthz", get(|| async { "ok" }))
        .route("/ws", get(ws_upgrade))
        .route("/api/v1/vaults", get(list_vaults))
        .route("/api/v1/vaults/{vault}/notes", get(list_notes).post(create_note))
        .route("/api/v1/vaults/{vault}/import", axum::routing::post(import_vault))
        .route(
            "/api/v1/vaults/{vault}/notes/{id}",
            get(get_note).put(put_note).patch(patch_note).delete(delete_note),
        )
        .route("/api/v1/vaults/{vault}/daily/{date}", get(daily_note))
        .route("/api/v1/vaults/{vault}/trash", get(list_trash))
        .route("/api/v1/vaults/{vault}/notes/{id}/restore", axum::routing::post(restore_note))
        .route("/api/v1/vaults/{vault}/notes/{id}/backlinks", get(backlinks))
        .route("/api/v1/vaults/{vault}/notes/{id}/export", axum::routing::post(export_note))
        .route("/api/v1/vaults/{vault}/notes/{id}/versions", get(list_versions).post(save_version))
        .route("/api/v1/vaults/{vault}/notes/{id}/versions/{seq}", get(get_version))
        .route("/api/v1/vaults/{vault}/tags", get(tags))
        .route("/api/v1/vaults/{vault}/tagged", get(tagged))
        .route("/api/v1/vaults/{vault}/search", get(search_vault))
        .route("/api/v1/search", get(search))
        .route("/api/v1/vaults/{vault}/attachments/{hash}", get(get_attachment).put(put_attachment))
        .layer(DefaultBodyLimit::max(MAX_ATTACHMENT_BYTES as usize));
    let router = match web_dir {
        // Single-page app: unknown paths fall back to index.html so `#/v/<id>` links work.
        Some(dir) => {
            router.fallback_service(ServeDir::new(&dir).fallback(ServeFile::new(dir.join("index.html"))))
        }
        None => router,
    };
    router.layer(TraceLayer::new_for_http()).with_state(state)
}

// ---- Sync over WebSocket --------------------------------------------------------------------

async fn ws_upgrade(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
    user: AuthUser,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state, user))
}

/// Per-connection authorization state.
struct Conn {
    id: u64,
    user: AuthUser,
    /// The vault this connection last gained access to; new note docs are bound to it.
    vault: Option<VaultId>,
}

async fn handle_socket(mut socket: WebSocket, state: Arc<AppState>, user: AuthUser) {
    let conn_id = state.next_conn.fetch_add(1, Ordering::Relaxed);
    let mut conn = Conn { id: conn_id, user, vault: None };
    let mut rx = state.bus.subscribe();
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
                Err(broadcast::error::RecvError::Lagged(n)) => warn!(conn_id, n, "ws client lagged; dropped frames"),
                Err(broadcast::error::RecvError::Closed) => break,
            },
        }
    }
    info!(conn_id, "ws disconnected");
}

/// Which vault a note doc belongs to: its row, a provisional claim, or the connection's vault.
async fn vault_of_note(state: &AppState, id: NoteId, conn_vault: Option<VaultId>) -> Option<VaultId> {
    if let Ok(Some(row)) = state.store.lock().await.note_by_id(id) {
        return Some(row.vault_id);
    }
    let mut claims = state.note_vault_claims.lock().await;
    if let Some(v) = claims.get(&id) {
        return Some(*v);
    }
    let v = conn_vault?;
    claims.insert(id, v);
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
    let allowed = if matches!(state.options.auth, AuthMode::Disabled) {
        true
    } else {
        let vault = match doc_id {
            DocId::Vault(v) => Some(v),
            DocId::Note(id) => vault_of_note(state, id, conn.vault).await,
        };
        let role = match (vault, doc_id) {
            (Some(v), DocId::Vault(_)) => auth::role_or_claim(state, &conn.user, v, true).await,
            (Some(v), DocId::Note(id)) => auth::note_role(state, &conn.user, v, id).await,
            (None, _) => None,
        };
        match role {
            Some(r) if is_write => r >= Role::Editor,
            Some(_) => true,
            None => false,
        }
    };
    if !allowed {
        warn!(conn_id, doc = %frame.doc_id, user = %conn.user.email, write = is_write, "denied");
        return vec![Frame::new(&frame.doc_id, &Message::Auth(Some("permission denied".into()))).encode()];
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
            {
                let doc = room.doc.lock().await;
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
            }
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
            if let Err(e) = derive_metadata(state, &room).await {
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

async fn get_room(state: &Arc<AppState>, id: DocId) -> lemmate_core::Result<Arc<Room>> {
    let key = id.to_string();
    let mut rooms = state.rooms.lock().await;
    if let Some(r) = rooms.get(&key) {
        return Ok(r.clone());
    }
    let store = state.store.lock().await;
    let doc = match id {
        DocId::Note(_) => RoomDoc::Note(store.load_doc(id)?),
        DocId::Vault(v) => RoomDoc::Vault(store.load_vault_doc(v)?),
    };
    drop(store);
    let room = Arc::new(Room { id, doc: Mutex::new(doc) });
    rooms.insert(key, room.clone());
    Ok(room)
}

/// Keep the relational tables (notes, tags, links, FTS) in step with the CRDT truth so the REST
/// and search endpoints reflect what clients synced (SPEC §4.2: metadata is derived, never a
/// second source of truth).
async fn derive_metadata(state: &Arc<AppState>, room: &Room) -> lemmate_core::Result<()> {
    let doc = room.doc.lock().await;
    let mut store = state.store.lock().await;
    match (&*doc, room.id) {
        (RoomDoc::Vault(v), DocId::Vault(vault_id)) => {
            let entries = v.entries();
            let live: std::collections::HashSet<NoteId> = entries.iter().map(|(id, _)| *id).collect();
            for row in store.list_notes(vault_id)? {
                if !live.contains(&row.id) {
                    store.trash_note(row.id)?;
                }
            }
            let attachment_paths: Vec<String> = v.attachment_entries().into_iter().map(|(p, _)| p).collect();
            for (id, path) in entries {
                let existing = store.note_by_id(id)?;
                let title = existing.as_ref().and_then(|r| r.title.clone()).or_else(|| {
                    std::path::Path::new(&path).file_stem().map(|s| s.to_string_lossy().into_owned())
                });
                store.upsert_note(id, vault_id, &path, title.as_deref())?;
                // Content may have arrived before the entry did (a browser creates the text
                // first): index it now that the row exists.
                if existing.is_none() {
                    let text = store.load_doc(DocId::Note(id))?.text();
                    if !text.is_empty() {
                        index_note_text(&mut store, id, &path, &text, &attachment_paths)?;
                    }
                }
            }
        }
        (RoomDoc::Note(n), DocId::Note(id)) => {
            if let Some(row) = store.note_by_id(id)? {
                let attachment_paths: Vec<String> = store
                    .load_vault_doc(row.vault_id)?
                    .attachment_entries()
                    .into_iter()
                    .map(|(p, _)| p)
                    .collect();
                index_note_text(&mut store, id, &row.path, &n.text(), &attachment_paths)?;
            } else {
                // No vault entry yet: keep the FTS/tags fresh; the title lands when the entry does.
                store.index_note(id, &markdown::index(&n.text())?)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Index one note's text: tags, links, FTS, title, and which vault attachments it references.
fn index_note_text(
    store: &mut Store,
    id: NoteId,
    path: &str,
    text: &str,
    attachment_paths: &[String],
) -> lemmate_core::Result<()> {
    let ix = markdown::index(text)?;
    store.index_note(id, &ix)?;
    let mut paths: Vec<String> = Vec::new();
    let targets = ix
        .wikilinks
        .iter()
        .filter(|w| w.embed)
        .map(|w| (w.target.clone(), true))
        .chain(ix.links.iter().map(|l| (l.clone(), false)));
    for (target, wiki) in targets {
        if let Some(p) = lemmate_core::attachments::resolve_reference(
            path,
            &target,
            wiki,
            |c| attachment_paths.iter().any(|e| e == c),
            || attachment_paths.to_vec(),
        ) && !paths.contains(&p)
        {
            paths.push(p);
        }
    }
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
    derive_metadata(state, room).await.map_err(internal)?;
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
    if p.is_empty() || p.contains("..") || p.contains('\\') {
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
    state.note_vault_claims.lock().await.insert(id, vault);
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
            continue;
        };
        match upload {
            Upload::Note { path, text, callouts, embeds } => {
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
            // Nothing here has a sidecar to keep these in; the relay stores them (§9).
            Upload::Daily(_) => {}
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

/// `Daily/YYYY-MM-DD.md`, created with a heading when missing (SPEC §9, §13.1).
async fn daily_note(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Path((vault, date)): Path<(String, String)>,
) -> Result<Json<NoteBody>, StatusCode> {
    let vault_id: VaultId = vault.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    auth::require(&state, &user, vault_id, Role::Editor).await?;
    if !date.chars().all(|c| c.is_ascii_digit() || c == '-') || date.len() != 10 {
        return Err(StatusCode::BAD_REQUEST);
    }
    let path = format!("Daily/{date}.md");
    let existing = state.store.lock().await.note_by_path(vault_id, &path).map_err(internal)?;
    if let Some(row) = existing {
        let room = note_room(&state, row.id).await?;
        let content = match &*room.doc.lock().await {
            RoomDoc::Note(d) => d.text(),
            RoomDoc::Vault(_) => String::new(),
        };
        return Ok(Json(NoteBody { id: row.id.to_string(), path: row.path, title: row.title, content }));
    }
    let (_, Json(body)) = create_note(
        State(state),
        user,
        Path(vault),
        Json(NewNote { path, content: format!("# {date}\n\n") }),
    )
    .await?;
    Ok(Json(body))
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
    let row = state
        .store
        .lock()
        .await
        .restore_note(id)
        .map_err(internal)?
        .filter(|r| r.vault_id == vault)
        .ok_or(StatusCode::NOT_FOUND)?;
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
    let rows: Vec<(VaultId, u32)> = match state.options.auth {
        AuthMode::Disabled => store.vaults().map_err(internal)?,
        AuthMode::Enabled { .. } => {
            store.vaults_of(&user.id).map_err(internal)?.into_iter().map(|(v, _, n)| (v, n)).collect()
        }
    };
    Ok(Json(rows.into_iter().map(|(id, notes)| VaultSummary { id: id.to_string(), notes }).collect()))
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
}

/// Render a note through pandoc (SPEC §12). Attachments resolve against the server's blob
/// store is future work; today links stay relative.
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
    let row = state.store.lock().await.note_by_id(id).map_err(internal)?.ok_or(StatusCode::NOT_FOUND)?;
    let room = note_room(&state, id).await?;
    let text = match &*room.doc.lock().await {
        RoomDoc::Note(d) => d.text(),
        RoomDoc::Vault(_) => return Err(StatusCode::NOT_FOUND),
    };
    let opts =
        lemmate_core::pandoc::ExportOptions { pandoc: state.options.pandoc.clone(), ..Default::default() };
    let (bytes, mime) =
        tokio::task::spawn_blocking(move || lemmate_core::pandoc::render(&text, format, &opts))
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

#[derive(Serialize)]
struct VersionOut {
    seq: i64,
    created_ms: i64,
    label: Option<String>,
    author: Option<String>,
}

#[derive(Deserialize)]
struct SaveVersion {
    #[serde(default)]
    label: Option<String>,
}

async fn list_versions(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Path((vault, id)): Path<(String, String)>,
) -> Result<Json<Vec<VersionOut>>, StatusCode> {
    let vault: VaultId = vault.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    let id: NoteId = id.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    auth::require(&state, &user, vault, Role::Viewer).await?;
    let rows = state.store.lock().await.versions(DocId::Note(id)).map_err(internal)?;
    Ok(Json(
        rows.into_iter()
            .map(|v| VersionOut { seq: v.seq, created_ms: v.created_ms, label: v.label, author: v.author })
            .collect(),
    ))
}

/// Snapshot the note now with a label (SPEC §9 "save version"); kept forever.
async fn save_version(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Path((vault, id)): Path<(String, String)>,
    Json(body): Json<SaveVersion>,
) -> Result<Json<VersionOut>, StatusCode> {
    let vault: VaultId = vault.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    let id: NoteId = id.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    auth::require(&state, &user, vault, Role::Editor).await?;
    let room = get_room(&state, DocId::Note(id)).await.map_err(internal)?;
    let doc = room.doc.lock().await;
    let label = body.label.unwrap_or_else(|| "saved version".to_owned());
    let now = now_ms();
    let seq = state
        .store
        .lock()
        .await
        .snapshot_labeled_at(DocId::Note(id), &doc.encode_full(), now, Some(&label), Some(&user.display_name))
        .map_err(internal)?;
    Ok(Json(VersionOut { seq, created_ms: now, label: Some(label), author: Some(user.display_name) }))
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
    let vault: VaultId = vault.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    let id: NoteId = id.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    auth::require(&state, &user, vault, Role::Viewer).await?;
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
            for (v, _, _) in store.vaults_of(&user.id).map_err(internal)? {
                all.extend(
                    store.search_in_vault(v, &p.q, p.limit.min(100)).map_err(|_| StatusCode::BAD_REQUEST)?,
                );
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
    Ok((
        [
            (header::CONTENT_TYPE, mime),
            (header::CACHE_CONTROL, "public, max-age=31536000, immutable".to_owned()),
        ],
        bytes,
    ))
}

fn internal(e: lemmate_core::Error) -> StatusCode {
    warn!(%e, "internal error");
    StatusCode::INTERNAL_SERVER_ERROR
}

//! Router, shared state, WebSocket relay, and REST handlers (SPEC §7, §13.1).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use axum::body::Bytes;
use axum::extract::ws::{Message as WsMessage, WebSocket, WebSocketUpgrade};
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use notes_core::attachments::{
    AttachmentStore, MAX_ATTACHMENT_BYTES, hash_bytes, is_valid_hash, mime_for_path,
};
use notes_core::store::{AttachmentRow, now_ms};
use notes_core::sync::{Frame, Message, SyncMessage};
use notes_core::{DocId, NoteDoc, NoteId, RetentionPolicy, Store, VaultDoc, VaultId, markdown};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, broadcast};
use tower_http::trace::TraceLayer;
use tracing::{info, warn};
use yrs::StateVector;

#[derive(Debug, Clone)]
pub struct ServerOptions {
    pub policy: RetentionPolicy,
    /// Root of the content-addressed blob store.
    pub attachments_dir: std::path::PathBuf,
}

impl Default for ServerOptions {
    fn default() -> Self {
        Self {
            policy: RetentionPolicy::default(),
            attachments_dir: std::env::temp_dir().join("notes-attachments"),
        }
    }
}

pub struct AppState {
    pub store: Mutex<Store>,
    pub options: ServerOptions,
    pub attachments: AttachmentStore,
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
    fn apply_update(&self, u: &[u8]) -> notes_core::Result<bool> {
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
        rooms: Mutex::new(HashMap::new()),
        bus,
        next_conn: AtomicU64::new(1),
    })
}

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/ws", get(ws_upgrade))
        .route("/api/v1/vaults/{vault}/notes", get(list_notes))
        .route("/api/v1/vaults/{vault}/notes/{id}", get(get_note))
        .route("/api/v1/search", get(search))
        .route("/api/v1/vaults/{vault}/attachments/{hash}", get(get_attachment).put(put_attachment))
        .layer(DefaultBodyLimit::max(MAX_ATTACHMENT_BYTES as usize))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

// ---- Sync over WebSocket --------------------------------------------------------------------

async fn ws_upgrade(ws: WebSocketUpgrade, State(state): State<Arc<AppState>>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(mut socket: WebSocket, state: Arc<AppState>) {
    let conn_id = state.next_conn.fetch_add(1, Ordering::Relaxed);
    let mut rx = state.bus.subscribe();
    let mut subscribed: HashSet<String> = HashSet::new();
    info!(conn_id, "ws connected");

    loop {
        tokio::select! {
            incoming = socket.recv() => match incoming {
                Some(Ok(WsMessage::Binary(bytes))) => {
                    let replies = handle_frame(&state, conn_id, &bytes, &mut subscribed).await;
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

/// Process one inbound frame; returns frames to send back to *this* connection.
async fn handle_frame(
    state: &Arc<AppState>,
    conn_id: u64,
    bytes: &[u8],
    subscribed: &mut HashSet<String>,
) -> Vec<Vec<u8>> {
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

async fn get_room(state: &Arc<AppState>, id: DocId) -> notes_core::Result<Arc<Room>> {
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
async fn derive_metadata(state: &Arc<AppState>, room: &Room) -> notes_core::Result<()> {
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
            for (id, path) in entries {
                let title = store.note_by_id(id)?.and_then(|r| r.title).or_else(|| {
                    std::path::Path::new(&path).file_stem().map(|s| s.to_string_lossy().into_owned())
                });
                store.upsert_note(id, vault_id, &path, title.as_deref())?;
            }
        }
        (RoomDoc::Note(n), DocId::Note(id)) => {
            let ix = markdown::index(&n.text())?;
            store.index_note(id, &ix)?;
        }
        _ => {}
    }
    Ok(())
}

// ---- REST -------------------------------------------------------------------------------------

#[derive(Serialize)]
struct NoteSummary {
    id: String,
    path: String,
    title: Option<String>,
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

async fn list_notes(
    State(state): State<Arc<AppState>>,
    Path(vault): Path<String>,
) -> Result<Json<Vec<NoteSummary>>, StatusCode> {
    let vault: VaultId = vault.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    let rows = state.store.lock().await.list_notes(vault).map_err(internal)?;
    Ok(Json(
        rows.into_iter()
            .map(|n| NoteSummary { id: n.id.to_string(), path: n.path, title: n.title })
            .collect(),
    ))
}

async fn get_note(
    State(state): State<Arc<AppState>>,
    Path((vault, id)): Path<(String, String)>,
) -> Result<Json<NoteBody>, StatusCode> {
    let vault: VaultId = vault.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    let id: NoteId = id.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
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
    Query(p): Query<SearchParams>,
) -> Result<Json<Vec<SearchHitOut>>, StatusCode> {
    let hits =
        state.store.lock().await.search(&p.q, p.limit.min(100)).map_err(|_| StatusCode::BAD_REQUEST)?;
    Ok(Json(
        hits.into_iter()
            .map(|h| SearchHitOut { note_id: h.note_id.to_string(), title: h.title, snippet: h.snippet })
            .collect(),
    ))
}

/// Idempotent, content-addressed upload: the URL names the blake3 hash the body must have.
async fn put_attachment(
    State(state): State<Arc<AppState>>,
    Path((vault, hash)): Path<(String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, StatusCode> {
    let vault: VaultId = vault.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
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
    Path((vault, hash)): Path<(String, String)>,
) -> Result<impl IntoResponse, StatusCode> {
    let vault: VaultId = vault.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
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

fn internal(e: notes_core::Error) -> StatusCode {
    warn!(%e, "internal error");
    StatusCode::INTERNAL_SERVER_ERROR
}

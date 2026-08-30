//! The local relay (SPEC §3.2, §14): an HTTP server on loopback that lets a UI in the same
//! machine use the engine as its server — `/ws` speaks the frame protocol against the engine's
//! in-memory docs, `/api/v1/…` answers from the sidecar store, and the built web client is
//! served at `/`. Everything works with the real server unreachable; edits are journaled and
//! pushed when it comes back.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use axum::Router;
use axum::extract::ws::{Message as WsMessage, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::get;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};
use tower_http::services::{ServeDir, ServeFile};

use crate::error::{Error, Result};
use crate::ids::{NoteId, VaultId};
use crate::store::{NoteRow, SearchHit};

/// Events the relay feeds into the engine loop.
pub enum LocalEvent {
    PeerConnected { id: u64, tx: mpsc::UnboundedSender<Vec<u8>> },
    PeerFrame { id: u64, bytes: Vec<u8> },
    PeerGone { id: u64 },
    Query { query: LocalQuery, reply: oneshot::Sender<LocalReply> },
}

pub enum LocalQuery {
    Vaults,
    Notes,
    Note(NoteId),
    Search {
        q: String,
        limit: u32,
    },
    Backlinks(NoteId),
    Tags,
    Attachment(String),
    /// Store uploaded bytes under `attachments/<name>` (deduplicated) and return the path.
    StoreAttachment {
        name: String,
        bytes: Vec<u8>,
    },
}

pub enum LocalReply {
    Vaults(Vec<(VaultId, u32)>),
    Notes(Vec<NoteRow>),
    Note(Option<(NoteRow, String)>),
    Search(Vec<SearchHit>),
    Backlinks(Vec<NoteRow>),
    Tags(Vec<(String, u32)>),
    Attachment(Option<(Vec<u8>, String)>),
    Stored { path: String, hash: String },
    Error(String),
}

#[derive(Debug, Clone)]
pub struct LocalOptions {
    pub bind: SocketAddr,
    /// Built web client (`ui/dist`) to serve at `/`.
    pub web_dir: Option<PathBuf>,
}

pub(crate) struct LocalState {
    tx: mpsc::UnboundedSender<LocalEvent>,
    next_peer: AtomicU64,
}

/// Bind the relay and start serving; returns the bound address and the event receiver the
/// engine must drain.
pub(crate) async fn serve(
    opts: &LocalOptions,
) -> Result<(SocketAddr, mpsc::UnboundedReceiver<LocalEvent>, tokio::task::JoinHandle<()>)> {
    let (tx, rx) = mpsc::unbounded_channel();
    let state = Arc::new(LocalState { tx, next_peer: AtomicU64::new(1) });
    let listener = tokio::net::TcpListener::bind(opts.bind).await?;
    let addr = listener.local_addr()?;
    let router = Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/ws", get(ws_upgrade))
        .route("/api/v1/vaults", get(vaults))
        .route("/api/v1/vaults/{vault}/notes", get(notes))
        .route("/api/v1/vaults/{vault}/notes/{id}", get(note))
        .route("/api/v1/vaults/{vault}/notes/{id}/backlinks", get(backlinks))
        .route("/api/v1/vaults/{vault}/tags", get(tags))
        .route("/api/v1/vaults/{vault}/search", get(search))
        .route("/api/v1/vaults/{vault}/attachments/{hash}", get(attachment).put(put_attachment))
        .layer(axum::extract::DefaultBodyLimit::max(crate::attachments::MAX_ATTACHMENT_BYTES as usize));
    let router = match &opts.web_dir {
        Some(dir) => {
            router.fallback_service(ServeDir::new(dir).fallback(ServeFile::new(dir.join("index.html"))))
        }
        None => router,
    };
    let app = router.with_state(state);
    let task = tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            tracing::warn!(%e, "local relay stopped");
        }
    });
    Ok((addr, rx, task))
}

async fn ws_upgrade(ws: WebSocketUpgrade, State(state): State<Arc<LocalState>>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| peer_session(socket, state))
}

async fn peer_session(mut socket: WebSocket, state: Arc<LocalState>) {
    let id = state.next_peer.fetch_add(1, Ordering::Relaxed);
    let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
    if state.tx.send(LocalEvent::PeerConnected { id, tx }).is_err() {
        return;
    }
    loop {
        tokio::select! {
            msg = socket.recv() => match msg {
                Some(Ok(WsMessage::Binary(b))) => {
                    if state.tx.send(LocalEvent::PeerFrame { id, bytes: b.to_vec() }).is_err() { break }
                }
                Some(Ok(WsMessage::Close(_))) | None | Some(Err(_)) => break,
                Some(Ok(_)) => {}
            },
            out = rx.recv() => match out {
                Some(bytes) => if socket.send(WsMessage::Binary(bytes.into())).await.is_err() { break },
                None => break,
            },
        }
    }
    let _ = state.tx.send(LocalEvent::PeerGone { id });
}

async fn ask(state: &LocalState, query: LocalQuery) -> std::result::Result<LocalReply, StatusCode> {
    let (reply, rx) = oneshot::channel();
    state.tx.send(LocalEvent::Query { query, reply }).map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    match rx.await {
        Ok(LocalReply::Error(e)) => {
            tracing::warn!(%e, "local query");
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
        Ok(r) => Ok(r),
        Err(_) => Err(StatusCode::SERVICE_UNAVAILABLE),
    }
}

// Same JSON shapes as notes-server, so the web client does not know which one it talks to.
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
}
#[derive(Serialize)]
struct NoteBody {
    id: String,
    path: String,
    title: Option<String>,
    content: String,
}
#[derive(Serialize)]
struct SearchHitOut {
    note_id: String,
    title: Option<String>,
    snippet: String,
}
#[derive(Serialize)]
struct TagCount {
    tag: String,
    count: u32,
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

fn summaries(rows: Vec<NoteRow>) -> Vec<NoteSummary> {
    rows.into_iter().map(|n| NoteSummary { id: n.id.to_string(), path: n.path, title: n.title }).collect()
}

type Resp<T> = std::result::Result<axum::Json<T>, StatusCode>;

async fn vaults(State(s): State<Arc<LocalState>>) -> Resp<Vec<VaultSummary>> {
    match ask(&s, LocalQuery::Vaults).await? {
        LocalReply::Vaults(v) => Ok(axum::Json(
            v.into_iter().map(|(id, notes)| VaultSummary { id: id.to_string(), notes }).collect(),
        )),
        _ => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

async fn notes(State(s): State<Arc<LocalState>>, Path(_vault): Path<String>) -> Resp<Vec<NoteSummary>> {
    match ask(&s, LocalQuery::Notes).await? {
        LocalReply::Notes(rows) => Ok(axum::Json(summaries(rows))),
        _ => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

async fn note(
    State(s): State<Arc<LocalState>>,
    Path((_vault, id)): Path<(String, String)>,
) -> Resp<NoteBody> {
    let id: NoteId = id.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    match ask(&s, LocalQuery::Note(id)).await? {
        LocalReply::Note(Some((row, content))) => {
            Ok(axum::Json(NoteBody { id: row.id.to_string(), path: row.path, title: row.title, content }))
        }
        LocalReply::Note(None) => Err(StatusCode::NOT_FOUND),
        _ => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

async fn backlinks(
    State(s): State<Arc<LocalState>>,
    Path((_vault, id)): Path<(String, String)>,
) -> Resp<Vec<NoteSummary>> {
    let id: NoteId = id.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    match ask(&s, LocalQuery::Backlinks(id)).await? {
        LocalReply::Backlinks(rows) => Ok(axum::Json(summaries(rows))),
        _ => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

async fn tags(State(s): State<Arc<LocalState>>, Path(_vault): Path<String>) -> Resp<Vec<TagCount>> {
    match ask(&s, LocalQuery::Tags).await? {
        LocalReply::Tags(t) => {
            Ok(axum::Json(t.into_iter().map(|(tag, count)| TagCount { tag, count }).collect()))
        }
        _ => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

async fn search(
    State(s): State<Arc<LocalState>>,
    Path(_vault): Path<String>,
    Query(p): Query<SearchParams>,
) -> Resp<Vec<SearchHitOut>> {
    match ask(&s, LocalQuery::Search { q: p.q, limit: p.limit.min(100) }).await? {
        LocalReply::Search(hits) => Ok(axum::Json(
            hits.into_iter()
                .map(|h| SearchHitOut { note_id: h.note_id.to_string(), title: h.title, snippet: h.snippet })
                .collect(),
        )),
        _ => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

async fn attachment(
    State(s): State<Arc<LocalState>>,
    Path((_vault, hash)): Path<(String, String)>,
) -> std::result::Result<impl IntoResponse, StatusCode> {
    match ask(&s, LocalQuery::Attachment(hash)).await? {
        LocalReply::Attachment(Some((bytes, mime))) => Ok(([(header::CONTENT_TYPE, mime)], bytes)),
        LocalReply::Attachment(None) => Err(StatusCode::NOT_FOUND),
        _ => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

#[derive(Serialize)]
struct Stored {
    path: String,
    hash: String,
}

/// Upload from a local UI: the engine writes the file into the vault, where it is picked up
/// like any attachment (hashed, uploaded, recorded in the vault doc once a note references it).
async fn put_attachment(
    State(s): State<Arc<LocalState>>,
    Path((_vault, hash)): Path<(String, String)>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> Resp<Stored> {
    if !crate::attachments::is_valid_hash(&hash) || crate::attachments::hash_bytes(&body) != hash {
        return Err(StatusCode::BAD_REQUEST);
    }
    let name = headers.get("x-filename").and_then(|v| v.to_str().ok()).unwrap_or("file").to_owned();
    match ask(&s, LocalQuery::StoreAttachment { name, bytes: body.to_vec() }).await? {
        LocalReply::Stored { path, hash } => Ok(axum::Json(Stored { path, hash })),
        _ => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

impl From<LocalQuery> for LocalEvent {
    fn from(query: LocalQuery) -> Self {
        let (reply, _) = oneshot::channel();
        LocalEvent::Query { query, reply }
    }
}

pub(crate) fn err_reply(e: Error) -> LocalReply {
    LocalReply::Error(e.to_string())
}

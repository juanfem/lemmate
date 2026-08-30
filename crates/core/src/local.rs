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
    Tagged(String),
    Versions(NoteId),
    VersionAt(NoteId, i64),
    SaveVersion(NoteId, String),
    Attachment(String),
    // Writes (SPEC §13.1), performed on the projected files so the usual machinery applies.
    CreateNote {
        path: String,
        content: String,
    },
    ReplaceNote {
        id: NoteId,
        content: String,
    },
    RenameNote {
        id: NoteId,
        path: String,
    },
    DeleteNote(NoteId),
    Daily(String),
    Trash,
    Restore(NoteId),
    Export {
        id: NoteId,
        format: crate::pandoc::Format,
    },
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
    Tagged(Vec<NoteRow>),
    Versions(Vec<crate::store::VersionRow>),
    VersionAt(Option<String>),
    SavedVersion(crate::store::VersionRow),
    Attachment(Option<(Vec<u8>, String)>),
    /// A note after a write: row + content. `None` → not found.
    Written(Option<(NoteRow, String)>),
    Conflict(String),
    Done,
    Exported(Vec<u8>, &'static str),
    Trash(Vec<(NoteRow, String)>),
    Stored {
        path: String,
        hash: String,
    },
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
        .route("/api/v1/local/setup", get(configured))
        .route("/api/v1/vaults", get(vaults))
        .route("/api/v1/vaults/{vault}/notes", get(notes).post(create_note))
        .route(
            "/api/v1/vaults/{vault}/notes/{id}",
            get(note).put(replace_note).patch(rename_note).delete(delete_note),
        )
        .route("/api/v1/vaults/{vault}/daily/{date}", get(daily))
        .route("/api/v1/vaults/{vault}/notes/{id}/export", axum::routing::post(export_note))
        .route("/api/v1/vaults/{vault}/trash", get(trash))
        .route("/api/v1/vaults/{vault}/notes/{id}/restore", axum::routing::post(restore))
        .route("/api/v1/vaults/{vault}/notes/{id}/backlinks", get(backlinks))
        .route("/api/v1/vaults/{vault}/notes/{id}/versions", get(versions).post(save_version))
        .route("/api/v1/vaults/{vault}/notes/{id}/versions/{seq}", get(version_at))
        .route("/api/v1/vaults/{vault}/tags", get(tags))
        .route("/api/v1/vaults/{vault}/tagged", get(tagged))
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

// Same JSON shapes as lemmate-server, so the web client does not know which one it talks to.
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

#[derive(Deserialize)]
struct NewNote {
    path: String,
    #[serde(default)]
    content: String,
}
#[derive(Deserialize)]
struct PutNote {
    content: String,
}
#[derive(Deserialize)]
struct PatchNote {
    path: String,
}

fn written(
    reply: LocalReply,
    created: bool,
) -> std::result::Result<(StatusCode, axum::Json<NoteBody>), StatusCode> {
    match reply {
        LocalReply::Written(Some((row, content))) => Ok((
            if created { StatusCode::CREATED } else { StatusCode::OK },
            axum::Json(NoteBody { id: row.id.to_string(), path: row.path, title: row.title, content }),
        )),
        LocalReply::Written(None) => Err(StatusCode::NOT_FOUND),
        LocalReply::Conflict(_) => Err(StatusCode::CONFLICT),
        _ => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

async fn create_note(
    State(s): State<Arc<LocalState>>,
    Path(_vault): Path<String>,
    axum::Json(body): axum::Json<NewNote>,
) -> std::result::Result<(StatusCode, axum::Json<NoteBody>), StatusCode> {
    written(ask(&s, LocalQuery::CreateNote { path: body.path, content: body.content }).await?, true)
}

async fn replace_note(
    State(s): State<Arc<LocalState>>,
    Path((_vault, id)): Path<(String, String)>,
    axum::Json(body): axum::Json<PutNote>,
) -> std::result::Result<(StatusCode, axum::Json<NoteBody>), StatusCode> {
    let id: NoteId = id.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    written(ask(&s, LocalQuery::ReplaceNote { id, content: body.content }).await?, false)
}

async fn rename_note(
    State(s): State<Arc<LocalState>>,
    Path((_vault, id)): Path<(String, String)>,
    axum::Json(body): axum::Json<PatchNote>,
) -> std::result::Result<StatusCode, StatusCode> {
    let id: NoteId = id.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    match ask(&s, LocalQuery::RenameNote { id, path: body.path }).await? {
        LocalReply::Done => Ok(StatusCode::NO_CONTENT),
        LocalReply::Written(None) => Err(StatusCode::NOT_FOUND),
        LocalReply::Conflict(_) => Err(StatusCode::CONFLICT),
        _ => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

async fn delete_note(
    State(s): State<Arc<LocalState>>,
    Path((_vault, id)): Path<(String, String)>,
) -> std::result::Result<StatusCode, StatusCode> {
    let id: NoteId = id.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    match ask(&s, LocalQuery::DeleteNote(id)).await? {
        LocalReply::Done => Ok(StatusCode::NO_CONTENT),
        LocalReply::Written(None) => Err(StatusCode::NOT_FOUND),
        _ => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

async fn daily(
    State(s): State<Arc<LocalState>>,
    Path((_vault, date)): Path<(String, String)>,
) -> Resp<NoteBody> {
    if date.len() != 10 || !date.chars().all(|c| c.is_ascii_digit() || c == '-') {
        return Err(StatusCode::BAD_REQUEST);
    }
    match ask(&s, LocalQuery::Daily(date)).await? {
        LocalReply::Written(Some((row, content))) => {
            Ok(axum::Json(NoteBody { id: row.id.to_string(), path: row.path, title: row.title, content }))
        }
        _ => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

#[derive(Serialize)]
struct TrashOut {
    id: String,
    path: String,
    title: Option<String>,
    deleted_at: String,
}

async fn trash(State(s): State<Arc<LocalState>>, Path(_vault): Path<String>) -> Resp<Vec<TrashOut>> {
    match ask(&s, LocalQuery::Trash).await? {
        LocalReply::Trash(rows) => Ok(axum::Json(
            rows.into_iter()
                .map(|(n, d)| TrashOut { id: n.id.to_string(), path: n.path, title: n.title, deleted_at: d })
                .collect(),
        )),
        _ => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

async fn restore(
    State(s): State<Arc<LocalState>>,
    Path((_vault, id)): Path<(String, String)>,
) -> Resp<NoteSummary> {
    let id: NoteId = id.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    match ask(&s, LocalQuery::Restore(id)).await? {
        LocalReply::Written(Some((row, _))) => {
            Ok(axum::Json(NoteSummary { id: row.id.to_string(), path: row.path, title: row.title }))
        }
        LocalReply::Written(None) => Err(StatusCode::NOT_FOUND),
        _ => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

#[derive(Deserialize)]
struct ExportIn {
    format: String,
}

/// Export through pandoc with the vault's `export/` folder as resources (SPEC §12).
async fn export_note(
    State(s): State<Arc<LocalState>>,
    Path((_vault, id)): Path<(String, String)>,
    axum::Json(body): axum::Json<ExportIn>,
) -> std::result::Result<impl IntoResponse, StatusCode> {
    let id: NoteId = id.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    let format = crate::pandoc::Format::parse(&body.format).ok_or(StatusCode::BAD_REQUEST)?;
    if !crate::pandoc::pandoc_available(None) {
        return Err(StatusCode::NOT_IMPLEMENTED);
    }
    match ask(&s, LocalQuery::Export { id, format }).await? {
        LocalReply::Exported(bytes, mime) => Ok((
            [
                (header::CONTENT_TYPE, mime.to_owned()),
                (
                    header::CONTENT_DISPOSITION,
                    format!("attachment; filename=\"note.{}\"", format.extension()),
                ),
            ],
            bytes,
        )),
        LocalReply::Written(None) => Err(StatusCode::NOT_FOUND),
        _ => Err(StatusCode::UNPROCESSABLE_ENTITY),
    }
}

#[derive(Serialize)]
struct VersionOut {
    seq: i64,
    created_ms: i64,
    label: Option<String>,
    author: Option<String>,
}
#[derive(Serialize)]
struct VersionBody {
    seq: i64,
    content: String,
}
#[derive(Deserialize)]
struct SaveVersion {
    #[serde(default)]
    label: Option<String>,
}
fn version_out(v: crate::store::VersionRow) -> VersionOut {
    VersionOut { seq: v.seq, created_ms: v.created_ms, label: v.label, author: v.author }
}

async fn versions(
    State(s): State<Arc<LocalState>>,
    Path((_vault, id)): Path<(String, String)>,
) -> Resp<Vec<VersionOut>> {
    let id: NoteId = id.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    match ask(&s, LocalQuery::Versions(id)).await? {
        LocalReply::Versions(v) => Ok(axum::Json(v.into_iter().map(version_out).collect())),
        _ => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

async fn save_version(
    State(s): State<Arc<LocalState>>,
    Path((_vault, id)): Path<(String, String)>,
    axum::Json(body): axum::Json<SaveVersion>,
) -> Resp<VersionOut> {
    let id: NoteId = id.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    match ask(&s, LocalQuery::SaveVersion(id, body.label.unwrap_or_else(|| "saved version".into()))).await? {
        LocalReply::SavedVersion(v) => Ok(axum::Json(version_out(v))),
        _ => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

async fn version_at(
    State(s): State<Arc<LocalState>>,
    Path((_vault, id, seq)): Path<(String, String, i64)>,
) -> Resp<VersionBody> {
    let id: NoteId = id.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    match ask(&s, LocalQuery::VersionAt(id, seq)).await? {
        LocalReply::VersionAt(Some(content)) => Ok(axum::Json(VersionBody { seq, content })),
        LocalReply::VersionAt(None) => Err(StatusCode::NOT_FOUND),
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

#[derive(Deserialize)]
struct TagParams {
    tag: String,
}

async fn tagged(
    State(s): State<Arc<LocalState>>,
    Path(_vault): Path<String>,
    Query(p): Query<TagParams>,
) -> Resp<Vec<NoteSummary>> {
    match ask(&s, LocalQuery::Tagged(p.tag)).await? {
        LocalReply::Tagged(rows) => Ok(axum::Json(summaries(rows))),
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

// ---- First-run setup (SPEC §14 desktop) -------------------------------------------------------

/// What the desktop shell needs before it can start the engine.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SetupRequest {
    pub vault_dir: String,
    pub server_url: String,
    #[serde(default)]
    pub vault_id: Option<String>,
    #[serde(default)]
    pub ca_cert: Option<String>,
    /// Sign in (or register) on the server and save the token before starting.
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub register: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct SetupStatus {
    pub configured: bool,
    pub config_path: String,
    /// Suggested default vault directory for the form.
    pub suggested_vault_dir: String,
}

pub(crate) struct SetupState {
    config_path: PathBuf,
    suggested: PathBuf,
    done: tokio::sync::Mutex<Option<oneshot::Sender<SetupRequest>>>,
}

/// Serve the web client in "setup mode" on loopback: the UI sees `configured: false` on
/// `GET /api/v1/local/setup`, shows its setup form, and `POST`s the answers; the request is
/// handed back to the caller (which writes the config, logs in, and starts the real relay).
pub async fn serve_setup(
    bind: SocketAddr,
    web_dir: Option<PathBuf>,
    config_path: PathBuf,
    suggested_vault_dir: PathBuf,
) -> Result<(SocketAddr, oneshot::Receiver<SetupRequest>, tokio::task::JoinHandle<()>)> {
    let (tx, rx) = oneshot::channel();
    let state = Arc::new(SetupState {
        config_path,
        suggested: suggested_vault_dir,
        done: tokio::sync::Mutex::new(Some(tx)),
    });
    let listener = tokio::net::TcpListener::bind(bind).await?;
    let addr = listener.local_addr()?;
    let router = Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/api/v1/local/setup", get(setup_status).post(setup_submit))
        .route("/api/v1/auth/me", get(|| async { StatusCode::NOT_FOUND }))
        .route("/api/v1/vaults", get(|| async { axum::Json(Vec::<()>::new()) }));
    let router = match web_dir {
        Some(dir) => {
            router.fallback_service(ServeDir::new(&dir).fallback(ServeFile::new(dir.join("index.html"))))
        }
        None => router,
    };
    let app = router.with_state(state);
    let task = tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            tracing::warn!(%e, "setup server stopped");
        }
    });
    Ok((addr, rx, task))
}

async fn setup_status(State(s): State<Arc<SetupState>>) -> axum::Json<SetupStatus> {
    axum::Json(SetupStatus {
        configured: false,
        config_path: s.config_path.display().to_string(),
        suggested_vault_dir: s.suggested.display().to_string(),
    })
}

async fn setup_submit(
    State(s): State<Arc<SetupState>>,
    axum::Json(req): axum::Json<SetupRequest>,
) -> StatusCode {
    if req.vault_dir.trim().is_empty()
        || !(req.server_url.starts_with("http://") || req.server_url.starts_with("https://"))
    {
        return StatusCode::BAD_REQUEST;
    }
    match s.done.lock().await.take() {
        Some(tx) => {
            let _ = tx.send(req);
            StatusCode::ACCEPTED
        }
        None => StatusCode::CONFLICT,
    }
}

/// On a configured relay, the UI asks the same endpoint and gets `configured: true`.
pub(crate) async fn configured() -> axum::Json<serde_json::Value> {
    axum::Json(serde_json::json!({ "configured": true }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn setup_mode_hands_the_form_back_once() {
        let (addr, rx, task) = serve_setup(
            "127.0.0.1:0".parse().unwrap(),
            None,
            PathBuf::from("/tmp/x.toml"),
            PathBuf::from("/home/me/notes"),
        )
        .await
        .unwrap();
        let base = format!("http://{addr}");
        let status: serde_json::Value = tokio::task::spawn_blocking({
            let base = base.clone();
            move || {
                serde_json::from_str(
                    &ureq::get(format!("{base}/api/v1/local/setup"))
                        .call()
                        .unwrap()
                        .body_mut()
                        .read_to_string()
                        .unwrap(),
                )
                .unwrap()
            }
        })
        .await
        .unwrap();
        assert_eq!(status["configured"], false);
        assert_eq!(status["suggested_vault_dir"], "/home/me/notes");
        // The UI's usual probes must not break the shell while unconfigured.
        let me = tokio::task::spawn_blocking({
            let base = base.clone();
            move || {
                ureq::get(format!("{base}/api/v1/auth/me"))
                    .call()
                    .map(|r| r.status().as_u16())
                    .unwrap_or_else(|e| match e {
                        ureq::Error::StatusCode(c) => c,
                        _ => 0,
                    })
            }
        })
        .await
        .unwrap();
        assert_eq!(me, 404);

        let submit = |body: serde_json::Value| {
            let base = base.clone();
            tokio::task::spawn_blocking(move || {
                match ureq::post(format!("{base}/api/v1/local/setup"))
                    .header("content-type", "application/json")
                    .send(body.to_string().as_bytes())
                {
                    Ok(r) => r.status().as_u16(),
                    Err(ureq::Error::StatusCode(c)) => c,
                    Err(e) => panic!("{e}"),
                }
            })
        };
        assert_eq!(submit(serde_json::json!({"vault_dir": "", "server_url": "x"})).await.unwrap(), 400);
        assert_eq!(
            submit(
                serde_json::json!({"vault_dir": "/v", "server_url": "https://s.example", "register": true})
            )
            .await
            .unwrap(),
            202
        );
        let req = rx.await.unwrap();
        assert_eq!(req.vault_dir, "/v");
        assert!(req.register);
        assert_eq!(
            submit(serde_json::json!({"vault_dir": "/v", "server_url": "https://s.example"})).await.unwrap(),
            409
        );
        task.abort();
    }
}

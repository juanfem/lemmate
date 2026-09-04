//! The local relay (SPEC §3.2, §14): an HTTP server on loopback that lets a UI in the same
//! machine use the engine as its server — `/ws` speaks the frame protocol against the engines'
//! in-memory docs, `/api/v1/…` answers from the sidecar stores, and the built web client is
//! served at `/`. Everything works with the real server unreachable; edits are journaled and
//! pushed when it comes back.
//!
//! One relay fronts **one engine per vault** (SPEC §9, "one workspace"): the desktop opens every
//! vault the account can read, each with its own folder, sidecar and connection, and the UI sees
//! them the way it sees the server's — one socket, frames addressed by doc id, `/api/v1/vaults`
//! listing them all. Routing a frame to the right engine is what [`Routes`] is for.

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use axum::Router;
use axum::extract::ws::{Message as WsMessage, WebSocket, WebSocketUpgrade};
use axum::extract::{Multipart, Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::get;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};

use crate::error::{Error, Result};
use crate::ids::{DocId, NoteId, VaultId};
use crate::import::UploadReport;
use crate::store::{NoteRow, SearchHit};
use crate::sync::Frame;

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
    /// One batch of an uploaded Obsidian vault: (vault-relative path, bytes) per picked file.
    Import {
        files: Vec<(String, Vec<u8>)>,
    },
    // Merging one vault into another (SPEC §3.2, `crate::merge`). The relay drives both
    // engines: it surveys them, moves the files across one at a time, and retires the source.
    /// What this vault holds, by path — the input to [`crate::merge::plan`].
    Survey,
    /// Read one vault-relative file as bytes.
    ReadFile(String),
    /// Write one vault-relative file. A note path is processed immediately, so the destination
    /// adopts the id in its front matter before the next one arrives; anything else is left for
    /// the note that references it to pick up.
    WriteFile {
        path: String,
        bytes: Vec<u8>,
    },
    /// Delete this vault's files and its sidecar, and stop. There is no undo: the caller has
    /// copied everything somewhere else first.
    Retire,
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
    Imported(UploadReport),
    Survey {
        name: Option<String>,
        notes: Vec<(NoteId, String)>,
        attachments: Vec<(String, String)>,
        /// Why this vault cannot be merged away right now, if it cannot: a vault that syncs
        /// with a server has to be deleted there too, and that needs the server.
        blocked: Option<String>,
    },
    File(Option<Vec<u8>>),
    /// What retiring a vault left behind: files the vault did not know about (so they were not
    /// copied anywhere), and whether the folder itself is gone.
    Retired {
        left: Vec<String>,
        folder_removed: bool,
    },
    Error(String),
}

#[derive(Debug, Clone)]
pub struct LocalOptions {
    pub bind: SocketAddr,
    /// Built web client (`ui/dist`) to serve at `/`.
    pub web_dir: Option<PathBuf>,
    /// Where a vault the UI creates gets its folder (SPEC §9). `None` — the default — means a
    /// relay with a fixed set of vaults: frames for any other vault are dropped, as before.
    pub vault_root: Option<PathBuf>,
    /// The configuration file this shell will rewrite if the UI asks to connect a server
    /// (SPEC §3.2). `None` — a relay configured by flags, like `lemmate serve` — cannot be
    /// reconfigured from the page, and `POST /api/v1/local/connect` says so.
    pub config_path: Option<PathBuf>,
}

/// A loopback port for `vault_dir`, the same one every time.
///
/// A shell that points its webview at `http://127.0.0.1:{port}` makes that string the page's
/// **origin**, and a browser partitions `localStorage` by origin — so binding port 0 quietly
/// throws away everything the UI keeps there (open tabs and panes, pinned tabs, sidebar width,
/// the file browser's mode and folds) on every launch. Deriving the port from the vault
/// directory gives each vault a stable one with no extra state file to keep in step.
///
/// Callers must still fall back to an ephemeral port when the bind fails: the port may be
/// taken, and a forgotten layout beats a shell that will not start.
///
/// The range is IANA's dynamic/private one. A derived port is guessable by other processes on
/// the machine, but the relay already listens without authentication on loopback and anything
/// local can find it by scanning, so predictability costs nothing that was not spent already.
pub fn stable_port(vault_dir: &std::path::Path) -> u16 {
    const FIRST: u32 = 49152;
    const COUNT: u32 = 65536 - FIRST;
    // FNV-1a written out rather than `DefaultHasher`, whose output is explicitly not stable
    // across Rust releases — this port has to survive a toolchain upgrade.
    let path = vault_dir.canonicalize().unwrap_or_else(|_| vault_dir.to_path_buf());
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in path.as_os_str().as_encoded_bytes() {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    let offset = u32::try_from(hash % u64::from(COUNT)).unwrap_or(0);
    u16::try_from(FIRST + offset).unwrap_or(0)
}

/// One engine behind the relay, and the vault it serves.
struct EngineRef {
    vault: VaultId,
    tx: mpsc::UnboundedSender<LocalEvent>,
}

pub(crate) struct LocalState {
    /// Not fixed at start: a UI that creates a vault (SPEC §9) adds one here, through
    /// [`Registrar`], without dropping the connection.
    engines: RwLock<Vec<EngineRef>>,
    /// The local UIs currently connected, so an engine that arrives late can still reach them.
    peers: RwLock<HashMap<u64, mpsc::UnboundedSender<Vec<u8>>>>,
    routes: Arc<Routes>,
    /// Where a request to open an unknown vault goes; `None` on a relay with a fixed set.
    wanted: Option<mpsc::UnboundedSender<VaultId>>,
    /// The server the engines behind this relay sync with, or `None` when they are standalone
    /// (SPEC §3.2). What the UI does with it is cosmetic — a standalone app has no "offline" to
    /// report and no account to name — but it is the only way for a page served on loopback to
    /// tell the two apart.
    upstream: Option<String>,
    /// Set when the shell can rewrite its configuration; see [`LocalOptions::config_path`].
    config_path: Option<PathBuf>,
    /// Where a request to connect a server goes, and `None` when nothing is listening.
    connect: Option<mpsc::UnboundedSender<ConnectAsk>>,
    next_peer: AtomicU64,
}

impl LocalState {
    fn send_to(&self, vault: VaultId, ev: LocalEvent) -> bool {
        match self.sender(vault) {
            Some(tx) => tx.send(ev).is_ok(),
            None => false,
        }
    }

    /// A clone of one engine's channel. Cloned rather than borrowed because the callers are
    /// async: a lock guard must not be held across an await.
    fn sender(&self, vault: VaultId) -> Option<mpsc::UnboundedSender<LocalEvent>> {
        let engines = self.engines.read().ok()?;
        engines.iter().find(|e| e.vault == vault).map(|e| e.tx.clone())
    }

    /// Every engine's channel, in the order the vaults were opened.
    fn senders(&self) -> Vec<mpsc::UnboundedSender<LocalEvent>> {
        match self.engines.read() {
            Ok(engines) => engines.iter().map(|e| e.tx.clone()).collect(),
            Err(_) => Vec::new(),
        }
    }

    /// The same event to every engine: a local UI arriving or leaving concerns all of them.
    fn broadcast(&self, ev: impl Fn() -> LocalEvent) {
        if let Ok(engines) = self.engines.read() {
            for engine in engines.iter() {
                let _ = engine.tx.send(ev());
            }
        }
    }

    fn holds(&self, vault: VaultId) -> bool {
        self.engines.read().map(|e| e.iter().any(|e| e.vault == vault)).unwrap_or(false)
    }

    fn only_engine(&self) -> Option<VaultId> {
        let engines = self.engines.read().ok()?;
        (engines.len() == 1).then(|| engines[0].vault)
    }

    /// Route one frame from a local UI to the engine that owns its doc.
    ///
    /// A vault frame names its vault outright. A note frame names only the note, and the UI
    /// writes a new note's text *before* its vault entry, so the first frames of a new note can
    /// arrive before any engine has heard of it: [`Routes::hold`] keeps them until one claims
    /// the note. With a single vault there is nothing to decide, and the engine's own
    /// pending-doc path (which journals what it receives) handles it as it always has.
    ///
    /// A frame for an unknown *vault* is a vault the UI has just created. It is held the same
    /// way while the shell opens a folder and an engine for it — see [`Registrar::add`].
    fn route_frame(&self, peer: u64, bytes: Vec<u8>) {
        let Ok(doc_id) = Frame::peek_doc_id(&bytes) else { return };
        let Ok(doc) = doc_id.parse::<DocId>() else { return };
        let vault = match doc {
            DocId::Vault(v) if self.holds(v) => v,
            DocId::Vault(v) => {
                self.routes.hold(doc, peer, bytes);
                if let Some(wanted) = &self.wanted {
                    let _ = wanted.send(v);
                }
                return;
            }
            DocId::Note(n) => match self.routes.owner(n).or_else(|| self.only_engine()) {
                Some(v) => v,
                None => {
                    self.routes.hold(doc, peer, bytes);
                    return;
                }
            },
        };
        self.send_to(vault, LocalEvent::PeerFrame { id: peer, bytes });
    }
}

impl LocalState {
    /// Stop routing to a vault: its engine has retired (merged into another) and is about to
    /// return. The counterpart of [`Registrar::add`].
    fn forget(&self, vault: VaultId) {
        if let Ok(mut engines) = self.engines.write() {
            engines.retain(|e| e.vault != vault);
        }
        // Claiming nothing for it drops every note the routing table had under this vault.
        self.routes.claim(vault, &HashSet::new());
    }
}

/// Adds an engine to a running relay: what a shell uses to open a vault a UI has just created.
pub(crate) struct Registrar(Arc<LocalState>);

impl Registrar {
    /// Register `vault` and return the event receiver its engine must drain. `None` when the
    /// relay already holds that vault, which is what makes a repeated request harmless.
    pub(crate) fn add(&self, vault: VaultId) -> Option<mpsc::UnboundedReceiver<LocalEvent>> {
        let mut engines = self.0.engines.write().ok()?;
        if engines.iter().any(|e| e.vault == vault) {
            return None;
        }
        let (tx, rx) = mpsc::unbounded_channel();
        // Everyone already connected is this engine's peer too, or it could never answer them.
        if let Ok(peers) = self.0.peers.read() {
            for (id, peer) in peers.iter() {
                let _ = tx.send(LocalEvent::PeerConnected { id: *id, tx: peer.clone() });
            }
        }
        engines.push(EngineRef { vault, tx });
        Some(rx)
    }
}

/// Which vault owns which note, and the frames waiting for an answer.
///
/// Shared between the relay and every engine: engines publish the notes they hold (see
/// `Engine::sync_routes`), the relay reads the map to address a frame. Frames for a note nobody
/// owns yet are held rather than dropped — they are usually a note being created, and dropping
/// them would lose whatever the UI typed before it wrote the vault entry.
#[derive(Default)]
pub(crate) struct Routes {
    table: Mutex<RouteTable>,
}

#[derive(Default)]
struct RouteTable {
    owner: HashMap<NoteId, VaultId>,
    held: HashMap<DocId, Vec<Held>>,
}

struct Held {
    peer: u64,
    bytes: Vec<u8>,
    since: Instant,
}

/// How long a frame waits for a vault to claim its doc, and how much waits at once. A doc
/// nobody ever claims is a bug elsewhere; these bounds keep it from being a leak.
const HOLD_FOR: Duration = Duration::from_secs(60);
const HOLD_DOCS: usize = 256;
const HOLD_FRAMES: usize = 64;

impl Routes {
    pub(crate) fn owner(&self, note: NoteId) -> Option<VaultId> {
        self.table.lock().ok()?.owner.get(&note).copied()
    }

    fn hold(&self, doc: DocId, peer: u64, bytes: Vec<u8>) {
        let Ok(mut t) = self.table.lock() else { return };
        let now = Instant::now();
        t.held.retain(|_, frames| {
            frames.retain(|h| now.duration_since(h.since) < HOLD_FOR);
            !frames.is_empty()
        });
        if t.held.len() >= HOLD_DOCS && !t.held.contains_key(&doc) {
            return;
        }
        let frames = t.held.entry(doc).or_default();
        if frames.len() >= HOLD_FRAMES {
            frames.remove(0);
        }
        frames.push(Held { peer, bytes, since: now });
    }

    /// Record the notes `vault` holds, and take back the frames held for it: those for notes it
    /// has just taken on, and — the first time it claims anything — those for the vault doc
    /// itself, which is how a vault the UI created reaches the engine opened for it.
    pub(crate) fn claim(&self, vault: VaultId, notes: &HashSet<NoteId>) -> Vec<(u64, Vec<u8>)> {
        let Ok(mut t) = self.table.lock() else { return Vec::new() };
        t.owner.retain(|id, v| *v != vault || notes.contains(id));
        let mut released: Vec<Held> = t.held.remove(&DocId::Vault(vault)).unwrap_or_default();
        for id in notes {
            if t.owner.insert(*id, vault).is_none()
                && let Some(frames) = t.held.remove(&DocId::Note(*id))
            {
                released.extend(frames);
            }
        }
        released.sort_by_key(|h| h.since);
        released.into_iter().map(|h| (h.peer, h.bytes)).collect()
    }

    /// A peer that went away is not coming back for its held frames.
    fn forget_peer(&self, peer: u64) {
        let Ok(mut t) = self.table.lock() else { return };
        t.held.retain(|_, frames| {
            frames.retain(|h| h.peer != peer);
            !frames.is_empty()
        });
    }
}

/// What [`serve`] hands back: the bound address, one event receiver per vault (in the order
/// they were given), the server task, the routing table the engines publish into, and — when
/// the caller allows new vaults — the requests for them and the way to answer.
pub(crate) struct Served {
    pub addr: SocketAddr,
    pub events: Vec<mpsc::UnboundedReceiver<LocalEvent>>,
    pub task: tokio::task::JoinHandle<()>,
    pub routes: Arc<Routes>,
    /// Vaults a UI has created and the relay does not hold yet.
    pub wanted: Option<mpsc::UnboundedReceiver<VaultId>>,
    /// Requests from the UI to give this standalone app a server; `None` when the shell named
    /// no configuration file to write.
    pub connect: Option<mpsc::UnboundedReceiver<ConnectAsk>>,
    pub registrar: Registrar,
}

/// Bind the relay and start serving one engine per vault; each engine must drain its receiver.
///
/// `upstream` is the server those engines sync with, if any; it is reported to the UI on
/// `GET /api/v1/local/setup` and used for nothing else here.
pub(crate) async fn serve(
    opts: &LocalOptions,
    vault_ids: &[VaultId],
    upstream: Option<String>,
) -> Result<Served> {
    let mut engines = Vec::with_capacity(vault_ids.len());
    let mut events = Vec::with_capacity(vault_ids.len());
    for vault in vault_ids {
        let (tx, rx) = mpsc::unbounded_channel();
        engines.push(EngineRef { vault: *vault, tx });
        events.push(rx);
    }
    let routes = Arc::new(Routes::default());
    let (wanted_tx, wanted_rx) = mpsc::unbounded_channel();
    let (connect_tx, connect_rx) = mpsc::unbounded_channel();
    let reconfigurable = opts.config_path.is_some();
    let state = Arc::new(LocalState {
        engines: RwLock::new(engines),
        peers: RwLock::new(HashMap::new()),
        routes: routes.clone(),
        wanted: opts.vault_root.is_some().then_some(wanted_tx),
        upstream,
        config_path: opts.config_path.clone(),
        connect: reconfigurable.then_some(connect_tx),
        next_peer: AtomicU64::new(1),
    });
    let registrar = Registrar(state.clone());
    let listener = tokio::net::TcpListener::bind(opts.bind).await?;
    let addr = listener.local_addr()?;
    let router = Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/ws", get(ws_upgrade))
        .route("/api/v1/local/setup", get(configured))
        .route("/api/v1/local/connect", axum::routing::post(connect_server))
        .route("/api/v1/local/merge", axum::routing::post(merge_vaults))
        .route("/api/v1/vaults", get(vaults))
        .route("/api/v1/search", get(search_all))
        .route("/api/v1/vaults/{vault}/notes", get(notes).post(create_note))
        .route("/api/v1/vaults/{vault}/import", axum::routing::post(import_vault))
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
        Some(dir) => router.fallback_service(crate::web::client(dir)),
        None => router,
    };
    let app = router.with_state(state);
    let task = tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            tracing::warn!(%e, "local relay stopped");
        }
    });
    Ok(Served {
        addr,
        events,
        task,
        routes,
        wanted: opts.vault_root.is_some().then_some(wanted_rx),
        connect: reconfigurable.then_some(connect_rx),
        registrar,
    })
}

async fn ws_upgrade(ws: WebSocketUpgrade, State(state): State<Arc<LocalState>>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| peer_session(socket, state))
}

/// One local UI. Every engine hears about the peer — each fans its own docs out to it — while
/// frames coming the other way go to the one engine that owns the doc they name.
async fn peer_session(mut socket: WebSocket, state: Arc<LocalState>) {
    let id = state.next_peer.fetch_add(1, Ordering::Relaxed);
    let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
    if let Ok(mut peers) = state.peers.write() {
        peers.insert(id, tx.clone());
    }
    state.broadcast(|| LocalEvent::PeerConnected { id, tx: tx.clone() });
    loop {
        tokio::select! {
            msg = socket.recv() => match msg {
                Some(Ok(WsMessage::Binary(b))) => state.route_frame(id, b.to_vec()),
                Some(Ok(WsMessage::Close(_))) | None | Some(Err(_)) => break,
                Some(Ok(_)) => {}
            },
            out = rx.recv() => match out {
                Some(bytes) => if socket.send(WsMessage::Binary(bytes.into())).await.is_err() { break },
                None => break,
            },
        }
    }
    if let Ok(mut peers) = state.peers.write() {
        peers.remove(&id);
    }
    state.broadcast(|| LocalEvent::PeerGone { id });
    state.routes.forget_peer(id);
}

/// Put a query to the engine serving `vault`; 404 when this relay does not hold that vault.
async fn ask(
    state: &LocalState,
    vault: &str,
    query: LocalQuery,
) -> std::result::Result<LocalReply, StatusCode> {
    let vault: VaultId = vault.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    let tx = state.sender(vault).ok_or(StatusCode::NOT_FOUND)?;
    ask_engine(&tx, query).await
}

async fn ask_engine(
    engine: &mpsc::UnboundedSender<LocalEvent>,
    query: LocalQuery,
) -> std::result::Result<LocalReply, StatusCode> {
    let (reply, rx) = oneshot::channel();
    engine.send(LocalEvent::Query { query, reply }).map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
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
    rows.into_iter()
        .map(|n| NoteSummary { id: n.id.to_string(), path: n.path, title: n.title, updated_at: n.updated_at })
        .collect()
}

type Resp<T> = std::result::Result<axum::Json<T>, StatusCode>;

async fn vaults(State(s): State<Arc<LocalState>>) -> Resp<Vec<VaultSummary>> {
    let engines = s.senders();
    let mut out = Vec::with_capacity(engines.len());
    for engine in &engines {
        if let LocalReply::Vaults(v) = ask_engine(engine, LocalQuery::Vaults).await? {
            out.extend(v.into_iter().map(|(id, notes)| VaultSummary { id: id.to_string(), notes }));
        }
    }
    Ok(axum::Json(out))
}

/// Search every vault this relay holds (SPEC §10), the endpoint the web client uses when it
/// does not know — or care — which vault a hit is in. Each engine ranks its own notes and the
/// lists are concatenated: FTS scores from separate SQLite databases are not comparable, so
/// there is nothing honest to merge on.
async fn search_all(
    State(s): State<Arc<LocalState>>,
    Query(p): Query<SearchParams>,
) -> Resp<Vec<SearchHitOut>> {
    let limit = p.limit.min(100);
    let mut out = Vec::new();
    for engine in &s.senders() {
        let q = LocalQuery::Search { q: p.q.clone(), limit };
        if let LocalReply::Search(hits) = ask_engine(engine, q).await? {
            out.extend(hits.into_iter().map(hit_out));
        }
        if out.len() >= limit as usize {
            break;
        }
    }
    out.truncate(limit as usize);
    Ok(axum::Json(out))
}

fn hit_out(h: SearchHit) -> SearchHitOut {
    SearchHitOut { note_id: h.note_id.to_string(), title: h.title, snippet: h.snippet }
}

async fn notes(State(s): State<Arc<LocalState>>, Path(vault): Path<String>) -> Resp<Vec<NoteSummary>> {
    match ask(&s, &vault, LocalQuery::Notes).await? {
        LocalReply::Notes(rows) => Ok(axum::Json(summaries(rows))),
        _ => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

async fn note(State(s): State<Arc<LocalState>>, Path((vault, id)): Path<(String, String)>) -> Resp<NoteBody> {
    let id: NoteId = id.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    match ask(&s, &vault, LocalQuery::Note(id)).await? {
        LocalReply::Note(Some((row, content))) => {
            Ok(axum::Json(NoteBody { id: row.id.to_string(), path: row.path, title: row.title, content }))
        }
        LocalReply::Note(None) => Err(StatusCode::NOT_FOUND),
        _ => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

async fn backlinks(
    State(s): State<Arc<LocalState>>,
    Path((vault, id)): Path<(String, String)>,
) -> Resp<Vec<NoteSummary>> {
    let id: NoteId = id.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    match ask(&s, &vault, LocalQuery::Backlinks(id)).await? {
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
    Path(vault): Path<String>,
    axum::Json(body): axum::Json<NewNote>,
) -> std::result::Result<(StatusCode, axum::Json<NoteBody>), StatusCode> {
    written(ask(&s, &vault, LocalQuery::CreateNote { path: body.path, content: body.content }).await?, true)
}

async fn replace_note(
    State(s): State<Arc<LocalState>>,
    Path((vault, id)): Path<(String, String)>,
    axum::Json(body): axum::Json<PutNote>,
) -> std::result::Result<(StatusCode, axum::Json<NoteBody>), StatusCode> {
    let id: NoteId = id.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    written(ask(&s, &vault, LocalQuery::ReplaceNote { id, content: body.content }).await?, false)
}

async fn rename_note(
    State(s): State<Arc<LocalState>>,
    Path((vault, id)): Path<(String, String)>,
    axum::Json(body): axum::Json<PatchNote>,
) -> std::result::Result<StatusCode, StatusCode> {
    let id: NoteId = id.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    match ask(&s, &vault, LocalQuery::RenameNote { id, path: body.path }).await? {
        LocalReply::Done => Ok(StatusCode::NO_CONTENT),
        LocalReply::Written(None) => Err(StatusCode::NOT_FOUND),
        LocalReply::Conflict(_) => Err(StatusCode::CONFLICT),
        _ => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

async fn delete_note(
    State(s): State<Arc<LocalState>>,
    Path((vault, id)): Path<(String, String)>,
) -> std::result::Result<StatusCode, StatusCode> {
    let id: NoteId = id.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    match ask(&s, &vault, LocalQuery::DeleteNote(id)).await? {
        LocalReply::Done => Ok(StatusCode::NO_CONTENT),
        LocalReply::Written(None) => Err(StatusCode::NOT_FOUND),
        _ => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

async fn daily(
    State(s): State<Arc<LocalState>>,
    Path((vault, date)): Path<(String, String)>,
) -> Resp<NoteBody> {
    if date.len() != 10 || !date.chars().all(|c| c.is_ascii_digit() || c == '-') {
        return Err(StatusCode::BAD_REQUEST);
    }
    match ask(&s, &vault, LocalQuery::Daily(date)).await? {
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

async fn trash(State(s): State<Arc<LocalState>>, Path(vault): Path<String>) -> Resp<Vec<TrashOut>> {
    match ask(&s, &vault, LocalQuery::Trash).await? {
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
    Path((vault, id)): Path<(String, String)>,
) -> Resp<NoteSummary> {
    let id: NoteId = id.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    match ask(&s, &vault, LocalQuery::Restore(id)).await? {
        LocalReply::Written(Some((row, _))) => Ok(axum::Json(NoteSummary {
            id: row.id.to_string(),
            path: row.path,
            title: row.title,
            updated_at: None,
        })),
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
    Path((vault, id)): Path<(String, String)>,
    axum::Json(body): axum::Json<ExportIn>,
) -> std::result::Result<impl IntoResponse, StatusCode> {
    let id: NoteId = id.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    let format = crate::pandoc::Format::parse(&body.format).ok_or(StatusCode::BAD_REQUEST)?;
    if !crate::pandoc::pandoc_available(None) {
        return Err(StatusCode::NOT_IMPLEMENTED);
    }
    match ask(&s, &vault, LocalQuery::Export { id, format }).await? {
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
    Path((vault, id)): Path<(String, String)>,
) -> Resp<Vec<VersionOut>> {
    let id: NoteId = id.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    match ask(&s, &vault, LocalQuery::Versions(id)).await? {
        LocalReply::Versions(v) => Ok(axum::Json(v.into_iter().map(version_out).collect())),
        _ => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

async fn save_version(
    State(s): State<Arc<LocalState>>,
    Path((vault, id)): Path<(String, String)>,
    axum::Json(body): axum::Json<SaveVersion>,
) -> Resp<VersionOut> {
    let id: NoteId = id.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    match ask(&s, &vault, LocalQuery::SaveVersion(id, body.label.unwrap_or_else(|| "saved version".into())))
        .await?
    {
        LocalReply::SavedVersion(v) => Ok(axum::Json(version_out(v))),
        _ => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

async fn version_at(
    State(s): State<Arc<LocalState>>,
    Path((vault, id, seq)): Path<(String, String, i64)>,
) -> Resp<VersionBody> {
    let id: NoteId = id.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    match ask(&s, &vault, LocalQuery::VersionAt(id, seq)).await? {
        LocalReply::VersionAt(Some(content)) => Ok(axum::Json(VersionBody { seq, content })),
        LocalReply::VersionAt(None) => Err(StatusCode::NOT_FOUND),
        _ => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

async fn tags(State(s): State<Arc<LocalState>>, Path(vault): Path<String>) -> Resp<Vec<TagCount>> {
    match ask(&s, &vault, LocalQuery::Tags).await? {
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
    Path(vault): Path<String>,
    Query(p): Query<TagParams>,
) -> Resp<Vec<NoteSummary>> {
    match ask(&s, &vault, LocalQuery::Tagged(p.tag)).await? {
        LocalReply::Tagged(rows) => Ok(axum::Json(summaries(rows))),
        _ => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

async fn search(
    State(s): State<Arc<LocalState>>,
    Path(vault): Path<String>,
    Query(p): Query<SearchParams>,
) -> Resp<Vec<SearchHitOut>> {
    match ask(&s, &vault, LocalQuery::Search { q: p.q, limit: p.limit.min(100) }).await? {
        LocalReply::Search(hits) => Ok(axum::Json(hits.into_iter().map(hit_out).collect())),
        _ => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

async fn attachment(
    State(s): State<Arc<LocalState>>,
    Path((vault, hash)): Path<(String, String)>,
) -> std::result::Result<impl IntoResponse, StatusCode> {
    match ask(&s, &vault, LocalQuery::Attachment(hash)).await? {
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
    Path((vault, hash)): Path<(String, String)>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> Resp<Stored> {
    if !crate::attachments::is_valid_hash(&hash) || crate::attachments::hash_bytes(&body) != hash {
        return Err(StatusCode::BAD_REQUEST);
    }
    let name = headers.get("x-filename").and_then(|v| v.to_str().ok()).unwrap_or("file").to_owned();
    match ask(&s, &vault, LocalQuery::StoreAttachment { name, bytes: body.to_vec() }).await? {
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

/// Obsidian import (SPEC §11.4), the same endpoint the server offers: a multipart body whose
/// parts are the picked files, each named by its vault-relative path. Here the engine writes
/// them into the vault folder, so they travel to the server as ordinary local edits.
async fn import_vault(
    State(s): State<Arc<LocalState>>,
    Path(vault): Path<String>,
    mut form: Multipart,
) -> Resp<UploadReport> {
    let mut files = Vec::new();
    while let Some(field) = form.next_field().await.map_err(|_| StatusCode::BAD_REQUEST)? {
        let rel = field.file_name().or_else(|| field.name()).unwrap_or_default().to_owned();
        let bytes = field.bytes().await.map_err(|_| StatusCode::BAD_REQUEST)?;
        files.push((rel, bytes.to_vec()));
    }
    match ask(&s, &vault, LocalQuery::Import { files }).await? {
        LocalReply::Imported(report) => Ok(axum::Json(report)),
        _ => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

pub(crate) fn err_reply(e: Error) -> LocalReply {
    LocalReply::Error(e.to_string())
}

// ---- First-run setup (SPEC §14 desktop) -------------------------------------------------------

/// What the desktop shell needs before it can start the engines: a folder, and — only if the
/// notes are to sync — a server.
///
/// No vault is named. With a server the shell opens every vault the account can read, one
/// folder each under `root_dir` (SPEC §9), and which vaults those are is the server's answer,
/// not the user's to type; standalone, it opens whatever folders are under the root and creates
/// one on a first run.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SetupRequest {
    pub root_dir: String,
    /// `None` (or empty) sets the app up standalone: no server, no account, nothing on the wire.
    #[serde(default)]
    pub server_url: Option<String>,
    #[serde(default)]
    pub ca_cert: Option<String>,
    /// Sign in (or register) on the server and save the token before starting.
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub register: bool,
    /// Registration invite (SPEC §11.1), so the first run of the desktop app can create an
    /// account on a server where registration is closed. The whole URL or the bare token.
    #[serde(default)]
    pub invite: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SetupStatus {
    pub configured: bool,
    pub config_path: String,
    /// Suggested default folder for the form: the root the vaults go under.
    pub suggested_root_dir: String,
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
    suggested_root_dir: PathBuf,
) -> Result<(SocketAddr, oneshot::Receiver<SetupRequest>, tokio::task::JoinHandle<()>)> {
    let (tx, rx) = oneshot::channel();
    let state = Arc::new(SetupState {
        config_path,
        suggested: suggested_root_dir,
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
        Some(dir) => router.fallback_service(crate::web::client(&dir)),
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
        suggested_root_dir: s.suggested.display().to_string(),
    })
}

async fn setup_submit(
    State(s): State<Arc<SetupState>>,
    axum::Json(req): axum::Json<SetupRequest>,
) -> StatusCode {
    if req.root_dir.trim().is_empty() {
        return StatusCode::BAD_REQUEST;
    }
    // A server is optional, but a half-typed one is a mistake, not a request to go standalone.
    if let Some(url) = req.server_url.as_deref().map(str::trim).filter(|u| !u.is_empty())
        && !(url.starts_with("http://") || url.starts_with("https://"))
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

/// On a configured relay, the UI asks the same endpoint and gets `configured: true`, plus the
/// mode it is running in: `"local"` for a standalone app, `"synced"` for one with a server.
///
/// `can_connect` says whether `POST /api/v1/local/connect` will be listened to, so the UI only
/// offers "connect a server" where there is a configuration file to write it into.
pub(crate) async fn configured(State(s): State<Arc<LocalState>>) -> axum::Json<serde_json::Value> {
    axum::Json(serde_json::json!({
        "configured": true,
        "mode": if s.upstream.is_some() { "synced" } else { "local" },
        "server": s.upstream,
        "can_connect": s.connect.is_some(),
        "config_path": s.config_path.as_ref().map(|p| p.display().to_string()),
    }))
}

// ---- Merging one vault into another (SPEC §3.2) -----------------------------------------------

/// Fold `from` into `into`, both held by this relay.
#[derive(Debug, Clone, Deserialize)]
pub struct MergeRequest {
    pub from: String,
    pub into: String,
    /// Folder inside the destination for the source's tree; `null` uses the source vault's
    /// name, `""` merges at the destination's root.
    #[serde(default)]
    pub folder: Option<String>,
    /// Work out the plan and change nothing. The dialog asks for this first.
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Debug, Serialize)]
struct MergeResponse {
    plan: crate::merge::MergePlan,
    applied: bool,
    /// Files the retired folder still held — nothing the vault knew about, so nothing that was
    /// copied — and whether the folder itself is gone.
    left: Vec<String>,
    folder_removed: bool,
}

/// The whole operation, in the order that makes it safe: survey, plan, copy, and only then
/// destroy. Anything that fails before the last step leaves both vaults exactly as they were.
async fn merge_vaults(
    State(s): State<Arc<LocalState>>,
    axum::Json(req): axum::Json<MergeRequest>,
) -> std::result::Result<axum::Json<MergeResponse>, (StatusCode, String)> {
    let bad = |m: &str| (StatusCode::BAD_REQUEST, m.to_owned());
    let from: VaultId = req.from.parse().map_err(|_| bad("`from` is not a vault id"))?;
    let into: VaultId = req.into.parse().map_err(|_| bad("`into` is not a vault id"))?;
    if from == into {
        return Err(bad("a vault cannot be merged into itself"));
    }
    let fail = |e: StatusCode| (e, "the vaults could not be read".to_owned());
    let LocalReply::Survey { name, notes, attachments, blocked } =
        ask(&s, &req.from, LocalQuery::Survey).await.map_err(fail)?
    else {
        return Err(fail(StatusCode::INTERNAL_SERVER_ERROR));
    };
    // Refused here, before anything is copied: the alternative is a merge that half-happens.
    if let Some(why) = blocked {
        return Err((StatusCode::CONFLICT, why));
    }
    let source = crate::merge::Survey { notes, attachments };
    let source_name = name;
    let LocalReply::Survey { notes, attachments, .. } =
        ask(&s, &req.into, LocalQuery::Survey).await.map_err(fail)?
    else {
        return Err(fail(StatusCode::INTERNAL_SERVER_ERROR));
    };
    let dest = crate::merge::Survey { notes, attachments };

    let folder =
        req.folder.clone().unwrap_or_else(|| crate::merge::default_folder(source_name.as_deref(), from));
    let plan = crate::merge::plan(from, into, &folder, &source, &dest);
    if req.dry_run {
        return Ok(axum::Json(MergeResponse {
            plan,
            applied: false,
            left: Vec::new(),
            folder_removed: false,
        }));
    }

    // Attachments first: a note is indexed the moment it lands, and an image already in place
    // is one the destination records straight away instead of on the next sweep.
    let moved = |e: StatusCode| (e, "the files could not be copied".to_owned());
    for a in plan.attachments.iter().filter(|a| a.fate != crate::merge::AttachmentFate::Same) {
        let LocalReply::File(Some(bytes)) =
            ask(&s, &req.from, LocalQuery::ReadFile(a.from.clone())).await.map_err(moved)?
        else {
            // The vault doc names a file this disk does not have; the notes still point at the
            // hash, and a synced destination will fetch it from the server.
            tracing::warn!(path = %a.from, "attachment missing while merging");
            continue;
        };
        let reply =
            ask(&s, &req.into, LocalQuery::WriteFile { path: a.to.clone(), bytes }).await.map_err(moved)?;
        if let LocalReply::Conflict(p) = reply {
            return Err((StatusCode::CONFLICT, format!("{p} already exists in the destination")));
        }
    }

    let rewrites = plan.attachment_rewrites();
    for n in &plan.notes {
        let LocalReply::File(Some(bytes)) =
            ask(&s, &req.from, LocalQuery::ReadFile(n.from.clone())).await.map_err(moved)?
        else {
            return Err((StatusCode::INTERNAL_SERVER_ERROR, format!("{} could not be read", n.from)));
        };
        // Only the notes moving with the attachment need rewriting, and only where one had to
        // be renamed; `rewrite_references` is a no-op otherwise.
        let text = String::from_utf8(bytes)
            .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, format!("{} is not text", n.from)))?;
        let text = crate::merge::rewrite_references(&text, &rewrites);
        let reply =
            ask(&s, &req.into, LocalQuery::WriteFile { path: n.to.clone(), bytes: text.into_bytes() })
                .await
                .map_err(moved)?;
        if let LocalReply::Conflict(p) = reply {
            return Err((StatusCode::CONFLICT, format!("{p} already exists in the destination")));
        }
    }

    // Everything is somewhere else now, so the source can go.
    let LocalReply::Retired { left, folder_removed } =
        ask(&s, &req.from, LocalQuery::Retire).await.map_err(|e| (e, "the merge did not finish".into()))?
    else {
        return Err((StatusCode::INTERNAL_SERVER_ERROR, "the source vault would not retire".into()));
    };
    s.forget(from);
    tracing::info!(%from, %into, notes = plan.notes.len(), "merged");
    Ok(axum::Json(MergeResponse { plan, applied: true, left, folder_removed }))
}

// ---- Connecting a standalone app to a server (SPEC §3.2) --------------------------------------

/// What the UI sends to give a running standalone app a server.
///
/// The account is optional the same way it is at setup: a server started with `--no-auth` wants
/// none, and a token saved earlier by `lemmate login` is used when no password is given.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConnectRequest {
    pub server_url: String,
    #[serde(default)]
    pub ca_cert: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub register: bool,
    #[serde(default)]
    pub invite: Option<String>,
}

/// One such request, with the channel the shell answers on.
///
/// The shell is the half that can sign in and rewrite the configuration file, and it is also
/// the half that knows whether that worked — so the HTTP response waits for it, and the dialog
/// can say "wrong password" instead of leaving the user to guess why nothing changed.
#[derive(Debug)]
pub struct ConnectAsk {
    pub request: ConnectRequest,
    pub reply: oneshot::Sender<std::result::Result<(), String>>,
}

async fn connect_server(
    State(s): State<Arc<LocalState>>,
    axum::Json(request): axum::Json<ConnectRequest>,
) -> std::result::Result<StatusCode, (StatusCode, String)> {
    let url = request.server_url.trim();
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err((StatusCode::BAD_REQUEST, "the server URL must start with http:// or https://".into()));
    }
    let Some(tx) = &s.connect else {
        return Err((
            StatusCode::NOT_IMPLEMENTED,
            "this app has no configuration file to write a server into".into(),
        ));
    };
    let (reply, answer) = oneshot::channel();
    if tx.send(ConnectAsk { request, reply }).is_err() {
        return Err((StatusCode::SERVICE_UNAVAILABLE, "the app is no longer listening".into()));
    }
    match answer.await {
        Ok(Ok(())) => Ok(StatusCode::ACCEPTED),
        Ok(Err(msg)) => Err((StatusCode::BAD_GATEWAY, msg)),
        Err(_) => Err((StatusCode::SERVICE_UNAVAILABLE, "the app stopped before answering".into())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_port_is_the_same_every_time_and_differs_per_vault() {
        let a = std::path::Path::new("/home/me/notes");
        let b = std::path::Path::new("/home/me/work");
        assert_eq!(stable_port(a), stable_port(a), "the whole point is that it does not move");
        assert_ne!(stable_port(a), stable_port(b), "two vaults must not fight over one port");
        for p in [a, b, std::path::Path::new("")] {
            assert!(stable_port(p) >= 49152, "must land in the dynamic range, got {}", stable_port(p));
        }
    }

    /// The value is a wire-ish constant: change the derivation and every existing install
    /// silently forgets its layout once, so this is a decision to make deliberately.
    #[test]
    fn stable_port_derivation_is_pinned() {
        // A path that cannot exist, so `canonicalize` fails and the raw bytes are hashed.
        assert_eq!(stable_port(std::path::Path::new("/nonexistent-vault-for-tests")), 54678);
    }

    /// Setting up with no server at all: the standalone app (SPEC §3.2).
    #[tokio::test]
    async fn setup_accepts_a_configuration_with_no_server() {
        let (addr, rx, task) = serve_setup(
            "127.0.0.1:0".parse().unwrap(),
            None,
            PathBuf::from("/tmp/x.toml"),
            PathBuf::from("/home/me/notes"),
        )
        .await
        .unwrap();
        let base = format!("http://{addr}");
        let code = tokio::task::spawn_blocking(move || {
            match ureq::post(format!("{base}/api/v1/local/setup"))
                .header("content-type", "application/json")
                .send(serde_json::json!({"root_dir": "/v"}).to_string().as_bytes())
            {
                Ok(r) => r.status().as_u16(),
                Err(ureq::Error::StatusCode(c)) => c,
                Err(e) => panic!("{e}"),
            }
        })
        .await
        .unwrap();
        assert_eq!(code, 202);
        let req = rx.await.unwrap();
        assert_eq!(req.root_dir, "/v");
        assert_eq!(req.server_url, None, "no server means standalone, not a default one");
        task.abort();
    }

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
        assert_eq!(status["suggested_root_dir"], "/home/me/notes");
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
        assert_eq!(submit(serde_json::json!({"root_dir": "", "server_url": "x"})).await.unwrap(), 400);
        // The server is optional now, but a half-typed one is still a mistake.
        assert_eq!(
            submit(serde_json::json!({"root_dir": "/v", "server_url": "notaurl"})).await.unwrap(),
            400
        );
        assert_eq!(
            submit(
                serde_json::json!({"root_dir": "/v", "server_url": "https://s.example", "register": true})
            )
            .await
            .unwrap(),
            202
        );
        let req = rx.await.unwrap();
        assert_eq!(req.root_dir, "/v");
        assert!(req.register);
        assert_eq!(
            submit(serde_json::json!({"root_dir": "/v", "server_url": "https://s.example"})).await.unwrap(),
            409
        );
        task.abort();
    }
}

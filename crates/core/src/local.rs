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
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use axum::Router;
use axum::extract::ws::{Message as WsMessage, WebSocket, WebSocketUpgrade};
use axum::extract::{Multipart, Path, Query, State};
use axum::http::{HeaderValue, Method, StatusCode, header};
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
    PeerConnected { id: u64, tx: Outbox },
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
    /// Name a snapshot, rename it, or (`None`) take its name away.
    LabelVersion(NoteId, i64, Option<String>),
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
    /// The vault's files that are not notes, with the notes using each (SPEC §9).
    Files,
    /// Write a file at a chosen path, kept whether or not a note uses it.
    PutFile {
        path: String,
        bytes: Vec<u8>,
        replace: bool,
        /// The hash the caller last saw there; a different one now is a conflict.
        base: Option<String>,
    },
    DeleteFile(String),
    MoveFile {
        from: String,
        to: String,
    },
    /// What a Quarto render of a note reads. The render itself runs outside the engine: it
    /// takes seconds, and the engine has sync to keep up meanwhile.
    RenderSource(NoteId),
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
    /// A note's whole CRDT state, so a merge carries its history across instead of making the
    /// destination insert the text again as if it were new (SPEC §3.2).
    NoteState(NoteId),
    /// A note's CRDT state from a vault being merged in, recorded before its file is written so
    /// the destination adopts the note *with* its history.
    AdoptState {
        id: NoteId,
        state: Vec<u8>,
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
    /// `None` → no such note in this vault.
    Versions(Option<Vec<crate::history::Entry>>),
    VersionAt(Option<String>),
    SavedVersion(crate::store::Saved),
    /// `None` → no such note, or no snapshot at that seq.
    Labelled(Option<crate::store::VersionRow>),
    Attachment(Option<(Vec<u8>, String)>),
    /// A note after a write: row + content. `None` → not found.
    Written(Option<(NoteRow, String)>),
    Conflict(String),
    Done,
    Exported(Vec<u8>, &'static str),
    Files(Vec<crate::files::FileEntry>),
    /// A file written: its path and hash, and whether it is new.
    FileWritten {
        path: String,
        hash: String,
        created: bool,
    },
    /// A file is already at that path: the hash it has.
    FileConflict(String),
    FileMoved {
        path: String,
        rewritten: usize,
    },
    RenderSource {
        path: String,
        text: String,
        attachments: Vec<String>,
        /// The vault's folder, where the attachments (and `export/`) are read from.
        root: PathBuf,
    },
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
    NoteState(Option<Vec<u8>>),
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
    /// Listen on an address other than loopback. Off, [`serve`] refuses one: the relay answers
    /// for every vault it holds, so reaching it from the network is a decision, not a default.
    /// Every request still needs the relay's key.
    pub allow_remote: bool,
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
/// The range is IANA's dynamic/private one. A derived port is guessable, which costs nothing:
/// knowing where the relay listens is not enough to use it without its per-launch key (see
/// [`Guard`]).
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
    peers: RwLock<HashMap<u64, Outbox>>,
    routes: Arc<Routes>,
    /// Where a request to open an unknown vault goes; `None` on a relay with a fixed set.
    wanted: Option<mpsc::UnboundedSender<VaultId>>,
    /// The server the engines behind this relay sync with, or `None` when they are standalone
    /// (SPEC §3.2). The UI learns from it whether there is an "offline" to report, and the relay
    /// uses its token to say who is signed in (`auth_me`).
    upstream: Option<Upstream>,
    /// Set when the shell can rewrite its configuration; see [`LocalOptions::config_path`].
    config_path: Option<PathBuf>,
    /// Where a request to connect a server goes, and `None` when nothing is listening.
    connect: Option<mpsc::UnboundedSender<ConnectAsk>>,
    /// Where a request to sign out of the server goes; the same shell listens as for `connect`.
    sign_out: Option<mpsc::UnboundedSender<SignOutAsk>>,
    next_peer: AtomicU64,
    /// Renders made for viewing, to be opened again without rendering again.
    renders: crate::quarto::RenderCache,
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
///
/// The bound is on bytes, not on frames: a new note's first frames are what the UI typed before
/// it wrote the vault entry, and dropping the oldest of them would lose the start of the note.
/// A doc that reaches its share has its updates merged into one (`compact_held`), which for
/// typing — many small updates to one text — is a fraction of the size.
const HOLD_FOR: Duration = Duration::from_secs(60);
const HOLD_DOCS: usize = 256;
const HOLD_DOC_BYTES: usize = 8 << 20;
const HOLD_TOTAL_BYTES: usize = 64 << 20;

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
            tracing::warn!(%doc, "too many docs waiting for a vault; dropping a frame");
            return;
        }
        let total: usize = t.held.values().flatten().map(|h| h.bytes.len()).sum();
        let frames = t.held.entry(doc).or_default();
        let size = |frames: &Vec<Held>| frames.iter().map(|h| h.bytes.len()).sum::<usize>();
        let mine = size(frames);
        if mine + bytes.len() > HOLD_DOC_BYTES {
            compact_held(frames);
        }
        let others = total - mine;
        let mine = size(frames);
        if (mine > 0 && mine + bytes.len() > HOLD_DOC_BYTES) || others + mine + bytes.len() > HOLD_TOTAL_BYTES
        {
            // Still too big after merging: a peer sending far more than a note being created
            // ever does. The doc is bounded either way, and the peer's next handshake resends
            // whatever it has that the engine lacks.
            tracing::warn!(%doc, held = mine, "held frames over their bound; dropping a frame");
            return;
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

/// Fold one doc's held frames into as few as say the same thing: every update into one (yrs
/// merges them losslessly), the latest state-vector request from each peer, and no presence —
/// which is stale by the time anyone reads it.
fn compact_held(frames: &mut Vec<Held>) {
    use crate::sync::{Message, SyncMessage};
    let Some(first) = frames.first() else { return };
    let since = first.since;
    let mut doc_id = None;
    let mut updates: Vec<Vec<u8>> = Vec::new();
    let mut update_peer = first.peer;
    let mut step1: HashMap<u64, Held> = HashMap::new();
    let mut kept: Vec<Held> = Vec::new();
    for h in frames.drain(..) {
        let Ok(frame) = Frame::decode(&h.bytes) else { continue };
        match frame.message() {
            Ok(Message::Sync(SyncMessage::Update(u) | SyncMessage::SyncStep2(u))) => {
                doc_id.get_or_insert(frame.doc_id);
                update_peer = h.peer;
                updates.push(u);
            }
            Ok(Message::Sync(SyncMessage::SyncStep1(_))) => {
                step1.insert(h.peer, h);
            }
            Ok(Message::Awareness(_) | Message::AwarenessQuery) => {}
            _ => kept.push(h),
        }
    }
    kept.extend(step1.into_values());
    if let Some(doc_id) = doc_id {
        let parts: Vec<&[u8]> = updates.iter().map(Vec::as_slice).collect();
        match yrs::merge_updates_v1(parts) {
            Ok(merged) => {
                let bytes = Frame::new(doc_id, &Message::Sync(SyncMessage::Update(merged))).encode();
                kept.push(Held { peer: update_peer, bytes, since });
            }
            Err(e) => {
                // Not mergeable means not decodable, which the engine would refuse anyway.
                tracing::warn!(%e, "held updates do not merge; dropping them");
            }
        }
    }
    kept.sort_by_key(|h| h.since);
    *frames = kept;
}

/// What [`serve`] hands back: the bound address, one event receiver per vault (in the order
/// they were given), the server task, the routing table the engines publish into, and — when
/// the caller allows new vaults — the requests for them and the way to answer.
pub(crate) struct Served {
    pub addr: SocketAddr,
    /// The per-launch secret every request must carry (see [`Guard`]).
    pub key: String,
    pub events: Vec<mpsc::UnboundedReceiver<LocalEvent>>,
    pub task: tokio::task::JoinHandle<()>,
    pub routes: Arc<Routes>,
    /// Vaults a UI has created and the relay does not hold yet.
    pub wanted: Option<mpsc::UnboundedReceiver<VaultId>>,
    /// Requests from the UI to give this standalone app a server; `None` when the shell named
    /// no configuration file to write.
    pub connect: Option<mpsc::UnboundedReceiver<ConnectAsk>>,
    /// Requests from the UI to sign out of the server; the same shell listens.
    pub sign_out: Option<mpsc::UnboundedReceiver<SignOutAsk>>,
    pub registrar: Registrar,
}

/// The server a relay's engines sync with, and how they sign in to it.
#[derive(Debug, Clone)]
pub(crate) struct Upstream {
    pub url: String,
    pub token: Option<String>,
    pub ca_cert: Option<PathBuf>,
}

/// Bind the relay and start serving one engine per vault; each engine must drain its receiver.
///
/// `upstream` is the server those engines sync with, if any: reported to the UI on
/// `GET /api/v1/local/setup`, and asked on the UI's behalf who its token belongs to.
pub(crate) async fn serve(
    opts: &LocalOptions,
    vault_ids: &[VaultId],
    upstream: Option<Upstream>,
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
    let (sign_out_tx, sign_out_rx) = mpsc::unbounded_channel();
    let reconfigurable = opts.config_path.is_some();
    let state = Arc::new(LocalState {
        engines: RwLock::new(engines),
        peers: RwLock::new(HashMap::new()),
        routes: routes.clone(),
        wanted: opts.vault_root.is_some().then_some(wanted_tx),
        upstream,
        config_path: opts.config_path.clone(),
        connect: reconfigurable.then_some(connect_tx),
        sign_out: reconfigurable.then_some(sign_out_tx),
        next_peer: AtomicU64::new(1),
        renders: crate::quarto::RenderCache::new(),
    });
    let registrar = Registrar(state.clone());
    if !opts.bind.ip().is_loopback() {
        if !opts.allow_remote {
            return Err(Error::Sync(format!(
                "refusing to serve the local relay on {}, which is not loopback; allow it explicitly \
                 if that is what you want",
                opts.bind
            )));
        }
        tracing::warn!(bind = %opts.bind, "the local relay is reachable from the network; anyone with its key can read and write every vault it holds");
    }
    let listener = tokio::net::TcpListener::bind(opts.bind).await?;
    let addr = listener.local_addr()?;
    let guard = Arc::new(Guard { key: Some(new_key()?), port: addr.port(), any_host: opts.allow_remote });
    let key = guard.key.clone().unwrap_or_default();
    let router = Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/ws", get(ws_upgrade))
        .route("/api/v1/local/setup", get(configured))
        .route("/api/v1/local/connect", axum::routing::post(connect_server))
        .route("/api/v1/local/merge", axum::routing::post(merge_vaults))
        .route("/api/v1/auth/me", get(auth_me))
        // The server's own features — sharing, members — answered by the server (`upstream`).
        .route(
            "/api/v1/vaults/{vault}/notes/{id}/shares",
            get(upstream_proxy).put(upstream_proxy).delete(upstream_proxy),
        )
        .route("/api/v1/vaults/{vault}/members", get(upstream_proxy).put(upstream_proxy))
        .route("/api/v1/vaults/{vault}/members/{user}", axum::routing::delete(upstream_proxy))
        .route("/api/v1/shared-with-me", get(upstream_proxy))
        .route("/api/v1/auth/logout", axum::routing::post(sign_out))
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
        .route("/api/v1/vaults/{vault}/notes/{id}/render", get(render_page).post(render_note))
        .route("/api/v1/vaults/{vault}/notes/{id}/render/{render}", get(kept_render))
        .route("/api/v1/vaults/{vault}/files", get(list_files).put(put_file).delete(delete_file))
        .route("/api/v1/vaults/{vault}/files/move", axum::routing::post(move_file))
        .route("/api/v1/vaults/{vault}/trash", get(trash))
        .route("/api/v1/vaults/{vault}/notes/{id}/restore", axum::routing::post(restore))
        .route("/api/v1/vaults/{vault}/notes/{id}/backlinks", get(backlinks))
        .route("/api/v1/vaults/{vault}/notes/{id}/versions", get(versions).post(save_version))
        .route("/api/v1/vaults/{vault}/notes/{id}/versions/{seq}", get(version_at).patch(label_version))
        .route("/api/v1/vaults/{vault}/tags", get(tags))
        .route("/api/v1/vaults/{vault}/tagged", get(tagged))
        .route("/api/v1/vaults/{vault}/search", get(search))
        .route("/api/v1/vaults/{vault}/attachments/{hash}", get(attachment).put(put_attachment))
        .layer(axum::extract::DefaultBodyLimit::max(crate::attachments::MAX_ATTACHMENT_BYTES as usize));
    let router = match &opts.web_dir {
        Some(dir) => router.fallback_service(crate::web::client(dir)),
        None => router,
    };
    // Outermost, so it covers the static client and the fallback as well as the API.
    let app = router.with_state(state).layer(axum::middleware::from_fn_with_state(guard, guard_request));
    let task = tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            tracing::warn!(%e, "local relay stopped");
        }
    });
    Ok(Served {
        addr,
        key,
        events,
        task,
        routes,
        wanted: opts.vault_root.is_some().then_some(wanted_rx),
        connect: reconfigurable.then_some(connect_rx),
        sign_out: reconfigurable.then_some(sign_out_rx),
        registrar,
    })
}

async fn ws_upgrade(ws: WebSocketUpgrade, State(state): State<Arc<LocalState>>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| peer_session(socket, state))
}

/// How much may wait for one local UI before it is disconnected. A window that stops reading —
/// suspended, or wedged — would otherwise grow the relay without bound; reconnecting costs it
/// one handshake, which sends whatever it missed.
const PEER_QUEUE_BYTES: usize = 64 << 20;

/// One local UI. Every engine hears about the peer — each fans its own docs out to it — while
/// frames coming the other way go to the one engine that owns the doc they name.
async fn peer_session(socket: WebSocket, state: Arc<LocalState>) {
    use futures_util::{SinkExt, StreamExt};
    let id = state.next_peer.fetch_add(1, Ordering::Relaxed);
    let (tx, mut rx) = outbox(PEER_QUEUE_BYTES);
    if let Ok(mut peers) = state.peers.write() {
        peers.insert(id, tx.clone());
    }
    state.broadcast(|| LocalEvent::PeerConnected { id, tx: tx.clone() });
    let (mut sink, mut stream) = socket.split();
    // Writing on a task of its own, so a peer that does not read cannot stall the reading side
    // (or hide that its queue is overflowing).
    let mut writer = tokio::spawn(async move {
        while let Some(bytes) = rx.recv().await {
            if sink.send(WsMessage::Binary(bytes.into())).await.is_err() {
                break;
            }
        }
    });
    loop {
        tokio::select! {
            msg = stream.next() => match msg {
                Some(Ok(WsMessage::Binary(b))) => state.route_frame(id, b.to_vec()),
                Some(Ok(WsMessage::Close(_))) | None | Some(Err(_)) => break,
                Some(Ok(_)) => {}
            },
            _ = tx.overflowed() => {
                tracing::warn!(peer = id, "a local UI is not keeping up; disconnecting it");
                break;
            }
            _ = &mut writer => break,
        }
    }
    writer.abort();
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
    if crate::daily::Date::parse(&date).is_none() {
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
    /// A render to look at in the app rather than to save (`quarto::RenderOptions::viewing`).
    #[serde(default)]
    view: bool,
    /// `?print-pdf` on a render opened as a page: open it to be printed (`quarto::for_print`).
    #[serde(default, rename = "print-pdf")]
    print: Option<String>,
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

/// A render the pane already has, opened again without rendering (see the server's
/// `kept_render`); rendered afresh once it has expired.
async fn kept_render(
    State(s): State<Arc<LocalState>>,
    Path((vault, id, render)): Path<(String, String, String)>,
    q: Query<ExportIn>,
) -> std::result::Result<axum::response::Response, StatusCode> {
    let Some((bytes, mime, disposition)) = s.renders.get(&render, &id) else {
        return render_page(State(s), Path((vault, id)), q).await;
    };
    let response = (
        [
            (header::CONTENT_TYPE, mime.to_owned()),
            (header::CONTENT_DISPOSITION, disposition),
            (header::CONTENT_SECURITY_POLICY, crate::quarto::PAGE_SANDBOX.to_owned()),
        ],
        bytes,
    )
        .into_response();
    Ok(if q.print.is_some() { printable(response).await } else { response })
}

/// A render page made to be printed (`?print-pdf`): a page gets the script that opens the print
/// dialog and the sandbox that lets it; anything else (a PDF, a Word file) goes as it came. The
/// server's render pages go through here too.
pub async fn printable(response: axum::response::Response) -> axum::response::Response {
    let html = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|t| t.starts_with("text/html"));
    if !html || !response.status().is_success() {
        return response;
    }
    let (mut parts, body) = response.into_parts();
    let Ok(bytes) = axum::body::to_bytes(body, usize::MAX).await else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    parts.headers.remove(header::CONTENT_LENGTH);
    parts.headers.insert(
        header::CONTENT_SECURITY_POLICY,
        axum::http::HeaderValue::from_static(crate::quarto::PRINT_SANDBOX),
    );
    axum::response::Response::from_parts(parts, crate::quarto::for_print(bytes.to_vec(), true).into())
}

/// A render as a page of its own, sandboxed by its headers (see the server's `render_page`).
async fn render_page(
    state: State<Arc<LocalState>>,
    path: Path<(String, String)>,
    Query(q): Query<ExportIn>,
) -> std::result::Result<axum::response::Response, StatusCode> {
    let print = q.print.is_some();
    let mut response =
        render_note(state, path, axum::Json(ExportIn { view: true, ..q })).await?.into_response();
    response.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        axum::http::HeaderValue::from_static(crate::quarto::PAGE_SANDBOX),
    );
    Ok(if print { printable(response).await } else { response })
}

#[derive(Deserialize)]
struct FileQuery {
    path: String,
}

/// The vault's files that are not notes — the same shape as the server's.
async fn list_files(
    State(s): State<Arc<LocalState>>,
    Path(vault): Path<String>,
) -> std::result::Result<impl IntoResponse, StatusCode> {
    match ask(&s, &vault, LocalQuery::Files).await? {
        LocalReply::Files(list) => Ok(axum::Json(list)),
        _ => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

/// Write a file at a chosen path (same headers and answers as the server's `put_file`).
async fn put_file(
    State(s): State<Arc<LocalState>>,
    Path(vault): Path<String>,
    Query(q): Query<FileQuery>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> std::result::Result<axum::response::Response, StatusCode> {
    let path = crate::files::file_path(&q.path).ok_or(StatusCode::BAD_REQUEST)?;
    let header = |name: &str| headers.get(name).and_then(|v| v.to_str().ok()).map(str::to_owned);
    let replace = header("x-replace").is_some_and(|v| v == "true" || v == "1");
    let query = LocalQuery::PutFile { path, bytes: body.to_vec(), replace, base: header("x-base-hash") };
    Ok(match ask(&s, &vault, query).await? {
        LocalReply::FileWritten { path, hash, created } => (
            if created { StatusCode::CREATED } else { StatusCode::OK },
            axum::Json(serde_json::json!({ "path": path, "hash": hash })),
        )
            .into_response(),
        LocalReply::FileConflict(current) => {
            (StatusCode::CONFLICT, axum::Json(serde_json::json!({ "current": current }))).into_response()
        }
        _ => return Err(StatusCode::INTERNAL_SERVER_ERROR),
    })
}

async fn delete_file(
    State(s): State<Arc<LocalState>>,
    Path(vault): Path<String>,
    Query(q): Query<FileQuery>,
) -> std::result::Result<StatusCode, StatusCode> {
    match ask(&s, &vault, LocalQuery::DeleteFile(q.path)).await? {
        LocalReply::Done => Ok(StatusCode::NO_CONTENT),
        _ => Err(StatusCode::NOT_FOUND),
    }
}

#[derive(Deserialize)]
struct MoveIn {
    from: String,
    to: String,
}

async fn move_file(
    State(s): State<Arc<LocalState>>,
    Path(vault): Path<String>,
    axum::Json(body): axum::Json<MoveIn>,
) -> std::result::Result<impl IntoResponse, StatusCode> {
    let to = crate::files::file_path(&body.to).ok_or(StatusCode::BAD_REQUEST)?;
    match ask(&s, &vault, LocalQuery::MoveFile { from: body.from, to }).await? {
        LocalReply::FileMoved { path, rewritten } => {
            Ok(axum::Json(serde_json::json!({ "path": path, "rewritten": rewritten })))
        }
        LocalReply::FileConflict(_) => Err(StatusCode::CONFLICT),
        _ => Err(StatusCode::NOT_FOUND),
    }
}

/// Render through Quarto (SPEC §5.6), from the vault's own folder. This machine is the user's
/// own, so unlike the server there is no switch to turn it off: it is `quarto` run by hand.
async fn render_note(
    State(s): State<Arc<LocalState>>,
    Path((vault, id)): Path<(String, String)>,
    axum::Json(body): axum::Json<ExportIn>,
) -> std::result::Result<impl IntoResponse, StatusCode> {
    let id: NoteId = id.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    // `"preview"`: whatever page the note itself declares (`quarto::preview_format`).
    // `"auto"`: what the note declares; `"preview"`: the page it declares (`quarto.rs`).
    let preview = body.format == "preview" || body.format == "auto";
    let format = if preview {
        crate::quarto::Format::Html
    } else {
        crate::quarto::Format::parse(&body.format).ok_or(StatusCode::BAD_REQUEST)?
    };
    let LocalReply::RenderSource { path, text, attachments, root } =
        ask(&s, &vault, LocalQuery::RenderSource(id)).await?
    else {
        return Err(StatusCode::NOT_FOUND);
    };
    let format = match body.format.as_str() {
        "auto" => crate::quarto::declared_format(&text),
        "preview" => crate::quarto::preview_format(&text),
        _ => format,
    };
    let view = body.view;
    let opts = crate::quarto::RenderOptions { viewing: view, ..Default::default() };
    let rendered = tokio::task::spawn_blocking(move || {
        // A deck's PDF also needs a Chrome to print it; without one the UI falls back to the
        // browser's print dialog, as it does without Quarto.
        if !crate::quarto::quarto_available(None)
            || (format == crate::quarto::Format::SlidesPdf && !crate::chrome::chrome_available(None))
        {
            return Ok(None);
        }
        let proj = crate::projection::Projection::new(root);
        let read = |p: &str| proj.read_bytes(p).ok();
        crate::quarto::render(&path, &text, format, &attachments, read, &opts).map(|r| Some((r, path)))
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    match rendered {
        Ok(Some(((bytes, mime), path))) => {
            let disposition = crate::quarto::disposition(&path, format);
            let kept = view.then(crate::quarto::RenderCache::new_id);
            let bytes = match (&kept, format) {
                (Some(k), crate::quarto::Format::RevealJs) => {
                    crate::quarto::with_speaker(bytes, &vault, &id.to_string(), k)
                }
                _ => bytes,
            };
            if let Some(k) = &kept {
                s.renders.put_as(k, &id.to_string(), &bytes, mime, &disposition);
            }
            let mut response = (
                [(header::CONTENT_TYPE, mime.to_owned()), (header::CONTENT_DISPOSITION, disposition)],
                bytes,
            )
                .into_response();
            if let Some(kept) = kept.and_then(|k| axum::http::HeaderValue::from_str(&k).ok()) {
                response.headers_mut().insert("x-render-id", kept);
            }
            Ok(response)
        }
        Ok(None) => Err(StatusCode::NOT_IMPLEMENTED),
        Err(e) => {
            tracing::warn!(%e, "quarto render");
            // Quarto's own words, not wrapped in ours: the pane shows them as they are.
            let msg = match e {
                crate::error::Error::Export(m) => m,
                other => other.to_string(),
            };
            Ok((StatusCode::UNPROCESSABLE_ENTITY, msg).into_response())
        }
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
#[derive(Deserialize)]
struct LabelVersion {
    #[serde(default)]
    label: Option<String>,
}
fn version_out(v: crate::store::VersionRow) -> VersionOut {
    VersionOut { seq: v.seq, created_ms: v.created_ms, label: v.label, author: v.author }
}

async fn versions(
    State(s): State<Arc<LocalState>>,
    Path((vault, id)): Path<(String, String)>,
) -> Resp<Vec<crate::history::Entry>> {
    let id: NoteId = id.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    match ask(&s, &vault, LocalQuery::Versions(id)).await? {
        LocalReply::Versions(Some(v)) => Ok(axum::Json(v)),
        LocalReply::Versions(None) => Err(StatusCode::NOT_FOUND),
        _ => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

async fn save_version(
    State(s): State<Arc<LocalState>>,
    Path((vault, id)): Path<(String, String)>,
    axum::Json(body): axum::Json<SaveVersion>,
) -> Resp<VersionOut> {
    let id: NoteId = id.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    let label = crate::history::clean_label(body.label.as_deref()).unwrap_or_else(|| "saved version".into());
    match ask(&s, &vault, LocalQuery::SaveVersion(id, label)).await? {
        LocalReply::SavedVersion(crate::store::Saved::New(v)) => Ok(axum::Json(version_out(v))),
        // Nothing changed since the last named version, which a save must not rename.
        LocalReply::SavedVersion(crate::store::Saved::Unchanged(_)) => Err(StatusCode::CONFLICT),
        _ => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

async fn label_version(
    State(s): State<Arc<LocalState>>,
    Path((vault, id, seq)): Path<(String, String, i64)>,
    axum::Json(body): axum::Json<LabelVersion>,
) -> Resp<VersionOut> {
    let id: NoteId = id.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    let label = crate::history::clean_label(body.label.as_deref());
    match ask(&s, &vault, LocalQuery::LabelVersion(id, seq, label)).await? {
        LocalReply::Labelled(Some(v)) => Ok(axum::Json(version_out(v))),
        LocalReply::Labelled(None) => Err(StatusCode::NOT_FOUND),
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
) -> std::result::Result<axum::response::Response, StatusCode> {
    match ask(&s, &vault, LocalQuery::Attachment(hash)).await? {
        LocalReply::Attachment(Some((bytes, mime))) => Ok(attachment_response(bytes, &mime)),
        LocalReply::Attachment(None) => Err(StatusCode::NOT_FOUND),
        _ => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

/// An attachment's bytes as the relay serves them: whatever a vault holds was written by
/// someone, possibly someone else, and is served from the relay's own origin — where a script
/// could use the relay's key. So nothing is sniffed, nothing runs (`sandbox`), and anything a
/// browser would render as a document of its own (HTML, SVG, XML, scripts, and whatever is not
/// on the short list of media) is a download instead.
fn attachment_response(bytes: Vec<u8>, mime: &str) -> axum::response::Response {
    let essence = mime.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    let inline = matches!(
        essence.as_str(),
        "image/png"
            | "image/jpeg"
            | "image/gif"
            | "image/webp"
            | "image/avif"
            | "image/bmp"
            | "audio/mpeg"
            | "audio/ogg"
            | "audio/wav"
            | "audio/webm"
            | "audio/flac"
            | "video/mp4"
            | "video/webm"
            | "video/ogg"
            | "application/pdf"
            | "text/plain"
    );
    let mut response = (
        [
            (header::CONTENT_TYPE, mime.to_owned()),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff".into()),
            (header::CONTENT_SECURITY_POLICY, "sandbox".into()),
            (header::CACHE_CONTROL, "private, max-age=31536000, immutable".into()),
        ],
        bytes,
    )
        .into_response();
    if !inline {
        response.headers_mut().insert(header::CONTENT_DISPOSITION, HeaderValue::from_static("attachment"));
    }
    response
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

// ---- Queues to peers ----------------------------------------------------------------------------

/// A queue to something that may read slower than we write — a local UI, or the server —
/// bounded by the bytes waiting in it.
///
/// Frames are CRDT updates, so silently dropping one would leave that peer behind for good.
/// A queue that would pass its bound refuses the frame and says so instead
/// ([`Outbox::overflowed`]); its owner closes the connection, and the handshake on reconnect sends
/// the peer everything it is missing. A single frame bigger than the bound still goes through an
/// empty queue: a large note must not be impossible to send.
#[derive(Clone)]
pub struct Outbox {
    tx: mpsc::UnboundedSender<Vec<u8>>,
    queued: Arc<AtomicUsize>,
    limit: usize,
    full: Arc<AtomicBool>,
    overflow: Arc<tokio::sync::Notify>,
}

pub(crate) struct OutboxRx {
    rx: mpsc::UnboundedReceiver<Vec<u8>>,
    queued: Arc<AtomicUsize>,
}

pub(crate) fn outbox(limit: usize) -> (Outbox, OutboxRx) {
    let (tx, rx) = mpsc::unbounded_channel();
    let queued = Arc::new(AtomicUsize::new(0));
    let full = Arc::new(AtomicBool::new(false));
    let overflow = Arc::new(tokio::sync::Notify::new());
    (Outbox { tx, queued: queued.clone(), limit, full, overflow }, OutboxRx { rx, queued })
}

impl Outbox {
    /// Queue one frame; `false` when it was not (the peer is gone, or too far behind).
    pub(crate) fn send(&self, bytes: Vec<u8>) -> bool {
        if self.full.load(Ordering::Relaxed) {
            return false;
        }
        let len = bytes.len();
        let before = self.queued.fetch_add(len, Ordering::Relaxed);
        if before > 0 && before + len > self.limit {
            self.queued.fetch_sub(len, Ordering::Relaxed);
            self.full.store(true, Ordering::Relaxed);
            self.overflow.notify_one();
            return false;
        }
        if self.tx.send(bytes).is_err() {
            self.queued.fetch_sub(len, Ordering::Relaxed);
            return false;
        }
        true
    }

    /// Resolves once a frame has been refused for want of room.
    pub(crate) async fn overflowed(&self) {
        self.overflow.notified().await
    }
}

impl OutboxRx {
    pub(crate) async fn recv(&mut self) -> Option<Vec<u8>> {
        let bytes = self.rx.recv().await?;
        self.queued.fetch_sub(bytes.len(), Ordering::Relaxed);
        Some(bytes)
    }
}

// ---- Who may use the relay ------------------------------------------------------------------------

/// The relay's front door (SPEC §3.2). Loopback is not a boundary: every page in every browser
/// on this machine can send requests to `127.0.0.1`, and a DNS name rebound to it can read the
/// answers. So:
///
/// - **Host** must name the relay as a loopback address or `localhost`, on its own port — a
///   rebound name fails here, before anything is read;
/// - an **Origin**, when the browser sends one, must be the relay's own — no other site's page
///   may drive it;
/// - and every request carries the **per-launch key**: the `lemmate_relay_<port>` cookie, set
///   when a page is first opened as `/?key=…` (the URL the shell or the CLI hands out), or
///   `Authorization: Bearer <key>` for programs. A WebSocket may give it as `?key=` too, since
///   a script cannot set headers on one.
///
/// `/healthz` answers anyone; it says nothing but "ok". The setup server uses the same guard
/// without a key: there is nothing to read before a vault exists, but a rebound page must not
/// fill in the form.
pub(crate) struct Guard {
    key: Option<String>,
    port: u16,
    /// Listening beyond loopback by request ([`LocalOptions::allow_remote`]): the Host is then
    /// whatever name the relay was reached by. Origin and key still apply.
    any_host: bool,
}

/// 32 random bytes, hex: the relay's key for this launch.
fn new_key() -> Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| Error::Sync(format!("no randomness for the relay key: {e}")))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

impl Guard {
    fn cookie_name(&self) -> String {
        // Per port: cookies are not, and two relays on one machine must not sign each other out.
        format!("lemmate_relay_{}", self.port)
    }

    fn host_ok(&self, host: &str) -> bool {
        if self.any_host {
            return true;
        }
        let (name, port) = match host.strip_prefix('[') {
            Some(rest) => match rest.split_once(']') {
                Some((ip, tail)) => (ip, tail.strip_prefix(':')),
                None => return false,
            },
            None => match host.rsplit_once(':') {
                Some((n, p)) => (n, Some(p)),
                None => (host, None),
            },
        };
        let port_ok = match port {
            Some(p) => p.parse::<u16>().is_ok_and(|p| p == self.port),
            None => self.port == 80,
        };
        let name_ok = name.eq_ignore_ascii_case("localhost")
            || name.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback());
        port_ok && name_ok
    }

    fn key_is(&self, given: &str) -> bool {
        let Some(key) = &self.key else { return true };
        // Constant time: the comparison must not say how much of a guess was right.
        given.len() == key.len() && given.bytes().zip(key.bytes()).fold(0u8, |acc, (a, b)| acc | (a ^ b)) == 0
    }
}

async fn guard_request(
    State(g): State<Arc<Guard>>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    if req.uri().path() == "/healthz" {
        return next.run(req).await;
    }
    let headers = req.headers();
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
        .or_else(|| req.uri().authority().map(|a| a.to_string()));
    let Some(host) = host.filter(|h| g.host_ok(h)) else {
        return (
            StatusCode::MISDIRECTED_REQUEST,
            "this is a local relay; reach it by its loopback address\n",
        )
            .into_response();
    };
    if let Some(origin) = headers.get(header::ORIGIN) {
        let own = format!("http://{host}");
        if !origin.to_str().is_ok_and(|o| o.eq_ignore_ascii_case(&own)) {
            return (StatusCode::FORBIDDEN, "cross-origin requests to the local relay are refused\n")
                .into_response();
        }
    }
    if g.key.is_none() {
        return next.run(req).await;
    }
    let query = req.uri().query().unwrap_or("");
    let given = query.split('&').find_map(|kv| kv.strip_prefix("key="));
    if let Some(given) = given {
        if !g.key_is(given) {
            return (StatusCode::UNAUTHORIZED, "wrong key for this relay\n").into_response();
        }
        let upgrade = headers.contains_key(header::UPGRADE);
        if req.method() == Method::GET && !upgrade {
            // A page opened with its key: remember the key in a cookie, and show the address
            // without it, so it is not left in the history or copied into a shared link.
            let rest: Vec<&str> =
                query.split('&').filter(|kv| !kv.starts_with("key=") && !kv.is_empty()).collect();
            let location = if rest.is_empty() {
                req.uri().path().to_owned()
            } else {
                format!("{}?{}", req.uri().path(), rest.join("&"))
            };
            // Lax, not Strict: a page reached from an opaque origin — the speaker view a sandboxed
            // render opens — must still carry it. Cross-site requests are stopped by the Host
            // and Origin checks above, not by the cookie.
            let cookie = format!(
                "{}={}; HttpOnly; SameSite=Lax; Path=/",
                g.cookie_name(),
                g.key.as_deref().unwrap_or("")
            );
            return (StatusCode::SEE_OTHER, [(header::LOCATION, location), (header::SET_COOKIE, cookie)])
                .into_response();
        }
        return next.run(req).await;
    }
    let bearer = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .is_some_and(|t| g.key_is(t.trim()));
    let name = g.cookie_name();
    let cookie = headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| kv.trim().split_once('='))
        .any(|(k, v)| k == name && g.key_is(v));
    if bearer || cookie {
        return next.run(req).await;
    }
    (
        StatusCode::UNAUTHORIZED,
        "this local relay needs its key: open the address the app or `lemmate serve` printed, which ends in ?key=…\n",
    )
        .into_response()
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
    /// A personal access token to save instead of signing in with a password: the way onto a
    /// server that signs in only through an identity provider.
    #[serde(default)]
    pub token: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SetupStatus {
    pub configured: bool,
    pub config_path: String,
    /// Suggested default folder for the form: the root the vaults go under.
    pub suggested_root_dir: String,
}

/// One submitted setup form, with the channel the shell answers on — as [`ConnectAsk`] does, so
/// the form can say "wrong password" or "cannot reach the server" and be submitted again,
/// instead of spinning forever on a setup that failed out of its sight.
#[derive(Debug)]
pub struct SetupAsk {
    pub request: SetupRequest,
    pub reply: oneshot::Sender<std::result::Result<(), String>>,
}

pub(crate) struct SetupState {
    config_path: PathBuf,
    suggested: PathBuf,
    /// The shell's end. Taken (`None`) once a setup has succeeded; the lock also keeps two
    /// submissions from running at once.
    asks: tokio::sync::Mutex<Option<mpsc::UnboundedSender<SetupAsk>>>,
}

/// Serve the web client in "setup mode" on loopback: the UI sees `configured: false` on
/// `GET /api/v1/local/setup`, shows its setup form, and `POST`s the answers; each submission is
/// handed to the caller (which writes the config, logs in, and starts the real relay) and the
/// response waits for its answer: `200` when it worked, `422 {"error"}` when it did not, and
/// the form may then be sent again.
pub async fn serve_setup(
    bind: SocketAddr,
    web_dir: Option<PathBuf>,
    config_path: PathBuf,
    suggested_root_dir: PathBuf,
) -> Result<(SocketAddr, mpsc::UnboundedReceiver<SetupAsk>, tokio::task::JoinHandle<()>)> {
    let (tx, rx) = mpsc::unbounded_channel();
    let state = Arc::new(SetupState {
        config_path,
        suggested: suggested_root_dir,
        asks: tokio::sync::Mutex::new(Some(tx)),
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
    let guard = Arc::new(Guard { key: None, port: addr.port(), any_host: false });
    let app = router.with_state(state).layer(axum::middleware::from_fn_with_state(guard, guard_request));
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
) -> axum::response::Response {
    let refuse = |code: StatusCode, error: String| {
        (code, axum::Json(serde_json::json!({ "error": error }))).into_response()
    };
    if req.root_dir.trim().is_empty() {
        return refuse(StatusCode::BAD_REQUEST, "choose a folder for the notes".into());
    }
    // A server is optional, but a half-typed one is a mistake, not a request to go standalone.
    if let Some(url) = req.server_url.as_deref().map(str::trim).filter(|u| !u.is_empty())
        && !(url.starts_with("http://") || url.starts_with("https://"))
    {
        return refuse(StatusCode::BAD_REQUEST, "the server URL must start with http:// or https://".into());
    }
    // Held across the shell's answer: one submission at a time.
    let mut asks = s.asks.lock().await;
    let Some(tx) = asks.as_ref() else {
        return refuse(StatusCode::CONFLICT, "this app is already set up".into());
    };
    let (reply, answer) = oneshot::channel();
    if tx.send(SetupAsk { request: req, reply }).is_err() {
        return refuse(StatusCode::SERVICE_UNAVAILABLE, "the app is not listening for a setup".into());
    }
    match answer.await {
        Ok(Ok(())) => {
            asks.take();
            StatusCode::OK.into_response()
        }
        Ok(Err(error)) => refuse(StatusCode::UNPROCESSABLE_ENTITY, error),
        Err(_) => refuse(StatusCode::SERVICE_UNAVAILABLE, "the setup did not finish".into()),
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
        "server": s.upstream.as_ref().map(|u| u.url.clone()),
        "ca_cert": s.upstream.as_ref().and_then(|u| u.ca_cert.as_ref()).map(|p| p.display().to_string()),
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
        // The note's history goes first, so the destination adopts the note it already knows
        // rather than inserting this text into a fresh doc — which, with a server holding the
        // same id, would merge into the text twice.
        if let Ok(id) = n.id.parse::<NoteId>()
            && let LocalReply::NoteState(Some(state)) =
                ask(&s, &req.from, LocalQuery::NoteState(id)).await.map_err(moved)?
        {
            ask(&s, &req.into, LocalQuery::AdoptState { id, state }).await.map_err(moved)?;
        }
        let reply =
            ask(&s, &req.into, LocalQuery::WriteFile { path: n.to.clone(), bytes: text.into_bytes() })
                .await
                .map_err(moved)?;
        if let LocalReply::Conflict(p) = reply {
            return Err((StatusCode::CONFLICT, format!("{p} already exists in the destination")));
        }
    }

    // Nothing is deleted until the destination holds every note under its own id: a note it
    // did not adopt would be lost with the source.
    let LocalReply::Survey { notes: landed, .. } =
        ask(&s, &req.into, LocalQuery::Survey).await.map_err(fail)?
    else {
        return Err(fail(StatusCode::INTERNAL_SERVER_ERROR));
    };
    let landed: HashSet<String> = landed.into_iter().map(|(id, _)| id.to_string()).collect();
    if let Some(missing) = plan.notes.iter().find(|n| !landed.contains(&n.id)) {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("{} did not arrive in the destination; nothing was removed", missing.from),
        ));
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
/// none, and a token saved earlier by `lemmate login` is used when neither a password nor a
/// token is given.
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
    /// As in [`SetupRequest::token`].
    #[serde(default)]
    pub token: Option<String>,
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

// ---- The account behind the relay ---------------------------------------------------------------

/// Who this relay's engines sync as, asked of the server with their token, so the UI can name
/// the account and offer to sign out of it. A standalone relay has no account (404). A server
/// that refuses the token — or wants one and there is none — is **signed out**: 401 with
/// `{"signed_out": <server>}`,
/// which the UI shows as such rather than as a password form. A server that cannot be reached
/// is neither — 503, and the UI simply names nobody.
async fn auth_me(State(s): State<Arc<LocalState>>) -> axum::response::Response {
    use axum::response::IntoResponse;
    let Some(up) = s.upstream.clone() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let signed_out = || {
        (StatusCode::UNAUTHORIZED, axum::Json(serde_json::json!({ "signed_out": up.url }))).into_response()
    };
    // Asked even without a token: a server started with `--no-auth` needs none, and answers.
    let (url, ca, token) = (
        format!("{}/api/v1/auth/me", crate::credentials::key(&up.url)),
        up.ca_cert.clone(),
        up.token.clone(),
    );
    let answer = tokio::task::spawn_blocking(move || -> std::result::Result<(u16, String), String> {
        let agent = crate::tls::http_agent(ca.as_deref()).map_err(|e| e.to_string())?;
        let mut req = agent.get(&url);
        if let Some(t) = &token {
            req = req.header("authorization", &format!("Bearer {t}"));
        }
        match req.call() {
            Ok(mut r) => Ok((r.status().as_u16(), r.body_mut().read_to_string().map_err(|e| e.to_string())?)),
            Err(ureq::Error::StatusCode(code)) => Ok((code, String::new())),
            Err(e) => Err(e.to_string()),
        }
    })
    .await;
    match answer {
        Ok(Ok((200, body))) => match serde_json::from_str::<serde_json::Value>(&body) {
            Ok(v) => axum::Json(v).into_response(),
            Err(_) => StatusCode::BAD_GATEWAY.into_response(),
        },
        Ok(Ok((401, _))) => signed_out(),
        _ => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

/// Forward a request for one of the server's own features — note shares, vault members, notes
/// shared with you — to the server, as the account whose token the engines sync with, and hand
/// back its answer. These are the server's to decide (roles, accounts, links), not the vault
/// folder's, so the relay only carries them; a standalone relay has no server to ask (404).
///
/// Only the routes listed in `serve` come here. Anything on this machine can reach them — as it
/// can every other route of a relay that listens unauthenticated on loopback.
async fn upstream_proxy(
    State(s): State<Arc<LocalState>>,
    method: axum::http::Method,
    uri: axum::http::Uri,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    let Some(up) = s.upstream.clone() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let path = uri.path_and_query().map(|p| p.as_str()).unwrap_or("/").to_owned();
    let url = format!("{}{path}", crate::credentials::key(&up.url));
    let content_type = headers.get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).map(str::to_owned);
    let answer = tokio::task::spawn_blocking(
        move || -> std::result::Result<(u16, Option<String>, Vec<u8>), String> {
            let agent = crate::tls::http_agent(up.ca_cert.as_deref()).map_err(|e| e.to_string())?;
            let mut req = ureq::http::Request::builder().method(method.as_str()).uri(&url);
            if let Some(t) = &up.token {
                req = req.header("authorization", format!("Bearer {t}"));
            }
            if let Some(ct) = &content_type {
                req = req.header("content-type", ct);
            }
            let req = req.body(body.to_vec()).map_err(|e| e.to_string())?;
            match agent.run(req) {
                Ok(mut r) => {
                    let ct = r.headers().get("content-type").and_then(|v| v.to_str().ok()).map(str::to_owned);
                    let bytes = r.body_mut().read_to_vec().map_err(|e| e.to_string())?;
                    Ok((r.status().as_u16(), ct, bytes))
                }
                Err(ureq::Error::StatusCode(code)) => Ok((code, None, Vec::new())),
                Err(e) => Err(e.to_string()),
            }
        },
    )
    .await;
    match answer {
        Ok(Ok((status, ct, bytes))) => {
            let status = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY);
            match ct {
                Some(ct) => (status, [(header::CONTENT_TYPE, ct)], bytes).into_response(),
                None => (status, bytes).into_response(),
            }
        }
        _ => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

/// A request from the UI to sign out of the server, for the shell to carry out — revoke the
/// token upstream, forget it here, and restart signed out — with the channel it answers on.
#[derive(Debug)]
pub struct SignOutAsk {
    pub reply: oneshot::Sender<std::result::Result<(), String>>,
}

async fn sign_out(State(s): State<Arc<LocalState>>) -> std::result::Result<StatusCode, (StatusCode, String)> {
    if s.upstream.is_none() {
        return Err((StatusCode::NOT_FOUND, "this app has no server to sign out of".into()));
    }
    let Some(tx) = &s.sign_out else {
        return Err((
            StatusCode::NOT_IMPLEMENTED,
            "signing out is for the desktop app; here, `lemmate logout --server …`".into(),
        ));
    };
    let (reply, answer) = oneshot::channel();
    if tx.send(SignOutAsk { reply }).is_err() {
        return Err((StatusCode::SERVICE_UNAVAILABLE, "the app is no longer listening".into()));
    }
    match answer.await {
        Ok(Ok(())) => Ok(StatusCode::NO_CONTENT),
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

    /// A note's first frames wait for a vault to claim it — however many there are. Typing a
    /// long first paragraph before the vault entry is written used to lose its beginning.
    #[test]
    fn held_frames_are_never_dropped_and_merge_when_large() {
        use crate::sync::{Message, SyncMessage};
        let routes = Routes::default();
        let note = NoteId::new();
        let typed = crate::doc::NoteDoc::new();
        let mut expected = String::new();
        // A few hundred keystrokes, each its own frame, then enough bulk to pass the bound.
        let append = |doc: &crate::doc::NoteDoc, text: String| {
            let at = doc.text().len() as u32;
            doc.apply_ops(&[crate::diff::TextOp::Insert { at, text }])
        };
        for i in 0..500 {
            let update = append(&typed, (i % 10).to_string());
            expected = typed.text();
            let frame = Frame::new(note.to_string(), &Message::Sync(SyncMessage::Update(update))).encode();
            routes.hold(DocId::Note(note), 1, frame);
        }
        // Then a large paste, sent again and again (a UI resending after reconnects): past the
        // bound in frames, a fraction of it once merged.
        let paste = append(&typed, "x".repeat(1 << 20));
        let frame = Frame::new(note.to_string(), &Message::Sync(SyncMessage::Update(paste))).encode();
        for _ in 0..12 {
            routes.hold(DocId::Note(note), 1, frame.clone());
        }
        let vault = VaultId::new();
        let released = routes.claim(vault, &HashSet::from([note]));
        let got = crate::doc::NoteDoc::new();
        for (_, bytes) in released {
            if let Ok(Message::Sync(SyncMessage::Update(u))) = Frame::decode(&bytes).unwrap().message() {
                got.apply_update(&u).unwrap();
            }
        }
        assert!(got.text().starts_with(&expected), "the first keystrokes are all there");
        assert_eq!(got.text(), typed.text());
    }

    #[tokio::test]
    async fn a_queue_past_its_bound_says_so_instead_of_growing() {
        let (tx, mut rx) = outbox(100);
        assert!(tx.send(vec![0; 500]), "one big frame still goes through an empty queue");
        assert!(!tx.send(vec![0; 10]), "but nothing more until it drains");
        tokio::time::timeout(std::time::Duration::from_secs(1), tx.overflowed()).await.unwrap();
        assert_eq!(rx.recv().await.map(|b| b.len()), Some(500));
        assert!(!tx.send(vec![0; 10]), "an overflowed queue stays closed: its peer is to be dropped");
    }

    #[test]
    fn the_guard_knows_its_own_host_and_key() {
        let g = Guard { key: Some("k".repeat(64)), port: 4242, any_host: false };
        for ok in ["127.0.0.1:4242", "localhost:4242", "LOCALHOST:4242", "[::1]:4242", "127.1.2.3:4242"] {
            assert!(g.host_ok(ok), "{ok}");
        }
        for bad in [
            "127.0.0.1:4243",
            "evil.example:4242",
            "127.0.0.1.nip.io:4242",
            "localhost",
            "[::1]",
            "0.0.0.0:4242",
        ] {
            assert!(!g.host_ok(bad), "{bad}");
        }
        assert!(g.key_is(&"k".repeat(64)) && !g.key_is(&"k".repeat(63)) && !g.key_is(&"j".repeat(64)));
        assert!(Guard { key: None, port: 80, any_host: false }.host_ok("localhost"));
    }

    /// Setting up with no server at all: the standalone app (SPEC §3.2).
    #[tokio::test]
    async fn setup_accepts_a_configuration_with_no_server() {
        let (addr, mut rx, task) = serve_setup(
            "127.0.0.1:0".parse().unwrap(),
            None,
            PathBuf::from("/tmp/x.toml"),
            PathBuf::from("/home/me/notes"),
        )
        .await
        .unwrap();
        // The shell's half: take the form and say it worked.
        let seen = tokio::spawn(async move {
            let ask = rx.recv().await.unwrap();
            let _ = ask.reply.send(Ok(()));
            ask.request
        });
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
        assert_eq!(code, 200);
        let req = seen.await.unwrap();
        assert_eq!(req.root_dir, "/v");
        assert_eq!(req.server_url, None, "no server means standalone, not a default one");
        task.abort();
    }

    #[tokio::test]
    async fn setup_mode_hands_the_form_back_until_it_works() {
        let (addr, mut rx, task) = serve_setup(
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

        // (status, body): error bodies are read too, since they carry the message for the form.
        let submit = |body: serde_json::Value| {
            let base = base.clone();
            tokio::task::spawn_blocking(move || {
                let agent: ureq::Agent =
                    ureq::Agent::config_builder().http_status_as_error(false).build().into();
                let mut r = agent
                    .post(format!("{base}/api/v1/local/setup"))
                    .header("content-type", "application/json")
                    .send(body.to_string().as_bytes())
                    .unwrap();
                (r.status().as_u16(), r.body_mut().read_to_string().unwrap_or_default())
            })
        };
        // The shell's half: the first form fails (a wrong password, say), the second works.
        let shell = tokio::spawn(async move {
            let mut seen = Vec::new();
            for answer in [Err("signing in: wrong password".to_owned()), Ok(())] {
                let ask = rx.recv().await.unwrap();
                seen.push(ask.request);
                let _ = ask.reply.send(answer);
            }
            seen
        });
        assert_eq!(submit(serde_json::json!({"root_dir": "", "server_url": "x"})).await.unwrap().0, 400);
        // The server is optional now, but a half-typed one is still a mistake.
        assert_eq!(
            submit(serde_json::json!({"root_dir": "/v", "server_url": "notaurl"})).await.unwrap().0,
            400
        );
        let form = serde_json::json!({"root_dir": "/v", "server_url": "https://s.example", "register": true});
        // A setup that fails says why, and the form can be sent again…
        let (code, body) = submit(form.clone()).await.unwrap();
        assert_eq!(code, 422);
        assert!(body.contains("wrong password"), "{body}");
        // …until it works.
        assert_eq!(submit(form).await.unwrap().0, 200);
        let seen = shell.await.unwrap();
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[1].root_dir, "/v");
        assert!(seen[1].register);
        // Once set up, there is nothing more to submit.
        assert_eq!(
            submit(serde_json::json!({"root_dir": "/v", "server_url": "https://s.example"})).await.unwrap().0,
            409
        );
        task.abort();
    }
}

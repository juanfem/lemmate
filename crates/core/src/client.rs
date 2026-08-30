//! The client sync engine (SPEC §6.3, §7): keeps a projected vault directory, the local sidecar
//! store, and the server in agreement.
//!
//! One engine per vault. It owns every note doc in memory, persists every update to
//! `<vault>/.notes/local.db`, writes remote changes to disk (debounced), ingests local file
//! changes as CRDT edits, and speaks the framed Yjs protocol over a WebSocket. Being offline is
//! not an error: everything is journaled locally and the handshake on reconnect sends what the
//! other side is missing. The same engine will back the Tauri shells.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use std::net::SocketAddr;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message as WsMsg;
use tokio_tungstenite::{Connector, connect_async_tls_with_config};
use tracing::{debug, info, warn};

use crate::attachments::{MAX_ATTACHMENT_BYTES, hash_bytes, mime_for_path, resolve_reference};
use crate::doc::NoteDoc;
use crate::error::{Error, Result};
use crate::frontmatter;
use crate::ids::{DocId, NoteId, VaultId};
pub use crate::local::LocalOptions;
use crate::local::{LocalEvent, LocalQuery, LocalReply, err_reply};
use crate::markdown::{self, NoteIndex};
use crate::projection::{Projection, ingest_external_edit};
use crate::store::{RetentionPolicy, Store, now_ms};
use crate::sync::{Frame, Message, SyncMessage};
use crate::vault_doc::VaultDoc;
use crate::watcher::{FsEvent, VaultWatcher};

/// Quiet period after the last filesystem event before a path is processed.
pub const FS_DEBOUNCE: Duration = Duration::from_millis(300);
/// Quiet period after the last remote change before a note is written to disk.
pub const PROJECT_DEBOUNCE: Duration = Duration::from_millis(500);
/// A file that vanishes and a new file with identical content within this window is a rename.
pub const RENAME_WINDOW: Duration = Duration::from_secs(2);
const TICK: Duration = Duration::from_millis(100);
const MAINTENANCE_INTERVAL: Duration = Duration::from_secs(60);
const UPLOAD_RETRY: Duration = Duration::from_secs(5);
const RECONNECT_MIN: Duration = Duration::from_secs(1);
const RECONNECT_MAX: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
pub struct SyncOptions {
    pub vault_dir: PathBuf,
    /// `http://host:port` or `https://host:port`; `/ws` is appended.
    pub server_url: String,
    /// Required to join an existing vault into an empty directory; otherwise taken from the
    /// sidecar, or generated for a brand-new vault.
    pub vault_id: Option<VaultId>,
    /// Reconcile, exchange updates until both sides are quiet, then return.
    pub once: bool,
    /// PEM file of a private CA to trust for `wss://` / `https://` instead of the public roots.
    pub ca_cert: Option<PathBuf>,
    /// Session or personal access token for the server (`Authorization: Bearer`).
    pub token: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncReport {
    pub vault_id: VaultId,
    pub notes: usize,
}

/// Run the sync loop. In `once` mode returns when in sync; otherwise runs until the task is
/// cancelled (it reconnects forever).
pub async fn run(opts: SyncOptions) -> Result<SyncReport> {
    crate::tls::install_crypto_provider();
    let engine = Engine::open(&opts)?;
    let (_tx, rx) = mpsc::unbounded_channel();
    run_inner(engine, opts, rx).await
}

/// A running engine with its local relay (SPEC §3.2): the address to point a UI at, and the
/// task to await or abort.
pub struct LocalHandle {
    pub addr: SocketAddr,
    pub vault_id: VaultId,
    task: tokio::task::JoinHandle<Result<SyncReport>>,
    server: tokio::task::JoinHandle<()>,
}

impl LocalHandle {
    pub async fn wait(self) -> Result<SyncReport> {
        let r = self.task.await.map_err(|e| Error::Sync(e.to_string()))?;
        self.server.abort();
        r
    }
    pub fn abort(&self) {
        self.task.abort();
        self.server.abort();
    }
}

/// Bind the local relay, start the engine in a background task, and return once the listener
/// is up. The relay keeps working while the real server is unreachable.
pub async fn start(opts: SyncOptions, local: LocalOptions) -> Result<LocalHandle> {
    crate::tls::install_crypto_provider();
    let engine = Engine::open(&opts)?;
    let vault_id = engine.vault_id;
    let (addr, rx, server) = crate::local::serve(&local).await?;
    info!(%addr, "local relay listening");
    let task = tokio::spawn(run_inner(engine, opts, rx));
    Ok(LocalHandle { addr, vault_id, task, server })
}

async fn run_inner(
    mut engine: Engine,
    opts: SyncOptions,
    mut local_rx: mpsc::UnboundedReceiver<LocalEvent>,
) -> Result<SyncReport> {
    engine.reconcile_disk()?;
    engine.maintain_all()?;

    let (fs_tx, mut fs_rx) = mpsc::unbounded_channel::<FsEvent>();
    let (std_tx, std_rx) = std::sync::mpsc::channel::<FsEvent>();
    let _watcher = VaultWatcher::start(engine.proj.clone(), std_tx)?;
    std::thread::spawn(move || {
        while let Ok(ev) = std_rx.recv() {
            if fs_tx.send(ev).is_err() {
                break;
            }
        }
    });

    let ws_url = ws_url(&opts.server_url)?;
    let ca = opts.ca_cert.as_deref();
    let tls = if ws_url.starts_with("wss://") { Some(crate::tls::client_config(ca)?) } else { None };
    let agent = crate::tls::http_agent(ca)?;
    let (job_tx, job_rx) = mpsc::unbounded_channel::<TransferJob>();
    let (done_tx, mut done_rx) = mpsc::unbounded_channel::<TransferDone>();
    tokio::spawn(transfer_worker(
        agent,
        http_url(&opts.server_url)?,
        opts.token.clone(),
        engine.vault_id,
        engine.proj.root().to_path_buf(),
        job_rx,
        done_tx,
    ));
    engine.transfers = Some(job_tx);
    let mut local_alive = true;
    let mut backoff = RECONNECT_MIN;
    loop {
        let connector = tls.clone().map(Connector::Rustls);
        let request = ws_request(&ws_url, opts.token.as_deref())?;
        match connect_async_tls_with_config(request, None, false, connector).await {
            Ok((ws, _)) => {
                info!(url = %ws_url, "connected");
                backoff = RECONNECT_MIN;
                let (mut sink, mut stream) = ws.split();
                let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Vec<u8>>();
                let writer = tokio::spawn(async move {
                    while let Some(bytes) = out_rx.recv().await {
                        if sink.send(WsMsg::Binary(bytes.into())).await.is_err() {
                            break;
                        }
                    }
                });
                engine.on_connect(out_tx);

                let mut ticker = tokio::time::interval(TICK);
                let mut idle_since: Option<Instant> = None;
                let finished = loop {
                    tokio::select! {
                        msg = stream.next() => match msg {
                            Some(Ok(WsMsg::Binary(b))) => engine.handle_frame(Origin::Server, &b),
                            Some(Ok(WsMsg::Close(_))) | None | Some(Err(_)) => break false,
                            Some(Ok(_)) => {}
                        },
                        ev = fs_rx.recv() => if let Some(ev) = ev { engine.on_fs_event(ev) },
                        done = done_rx.recv() => if let Some(done) = done { engine.on_transfer_done(done)? },
                        ev = local_rx.recv(), if local_alive => match ev {
                            Some(ev) => engine.on_local_event(ev),
                            None => local_alive = false,
                        },
                        _ = ticker.tick() => {
                            engine.tick()?;
                            if let Some(msg) = engine.fatal.take() {
                                return Err(Error::Sync(msg));
                            }
                            if opts.once {
                                if engine.is_idle() {
                                    let since = *idle_since.get_or_insert_with(Instant::now);
                                    if since.elapsed() >= FS_DEBOUNCE { break true; }
                                } else {
                                    idle_since = None;
                                }
                            }
                        }
                    }
                };
                engine.on_disconnect();
                writer.abort();
                if finished {
                    return Ok(engine.report());
                }
                if opts.once {
                    return Err(Error::Sync("connection lost before sync completed".into()));
                }
                warn!("connection lost; reconnecting");
            }
            Err(e) => {
                if opts.once {
                    return Err(Error::Sync(format!("cannot connect to {ws_url}: {e}")));
                }
                warn!(%e, "connect failed; retrying in {backoff:?}");
            }
        }
        // Offline: local peers, the projection, and the journal keep working.
        let deadline = Instant::now() + backoff;
        while Instant::now() < deadline {
            tokio::select! {
                ev = fs_rx.recv() => if let Some(ev) = ev { engine.on_fs_event(ev) },
                done = done_rx.recv() => if let Some(done) = done { engine.on_transfer_done(done)? },
                ev = local_rx.recv(), if local_alive => match ev {
                    Some(ev) => engine.on_local_event(ev),
                    None => local_alive = false,
                },
                _ = tokio::time::sleep(TICK) => engine.tick()?,
            }
        }
        backoff = (backoff * 2).min(RECONNECT_MAX);
    }
}

/// The upgrade request, carrying the bearer token when we have one.
fn ws_request(
    url: &str,
    token: Option<&str>,
) -> Result<tokio_tungstenite::tungstenite::handshake::client::Request> {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let mut req = url.into_client_request().map_err(|e| Error::Sync(e.to_string()))?;
    if let Some(t) = token {
        let value = format!("Bearer {t}")
            .parse()
            .map_err(|_| Error::Sync("token is not a valid header value".into()))?;
        req.headers_mut().insert("authorization", value);
    }
    Ok(req)
}

fn ws_url(server: &str) -> Result<String> {
    let base = server.trim_end_matches('/');
    let ws = if let Some(rest) = base.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = base.strip_prefix("http://") {
        format!("ws://{rest}")
    } else if base.starts_with("ws://") || base.starts_with("wss://") {
        base.to_owned()
    } else {
        return Err(Error::Sync(format!("server url must start with http(s):// or ws(s)://, got {server}")));
    };
    Ok(if ws.ends_with("/ws") { ws } else { format!("{ws}/ws") })
}

/// `http(s)://host[:port]` base for the REST API, derived from whatever URL form was given.
fn http_url(server: &str) -> Result<String> {
    let base = server.trim_end_matches('/');
    let base = base.strip_suffix("/ws").unwrap_or(base);
    if base.starts_with("http://") || base.starts_with("https://") {
        Ok(base.to_owned())
    } else if let Some(rest) = base.strip_prefix("wss://") {
        Ok(format!("https://{rest}"))
    } else if let Some(rest) = base.strip_prefix("ws://") {
        Ok(format!("http://{rest}"))
    } else {
        Err(Error::Sync(format!("server url must start with http(s):// or ws(s)://, got {server}")))
    }
}

// ---- Attachment transfers ----------------------------------------------------------------------

#[derive(Debug)]
enum TransferJob {
    Upload { path: String },
    Download { path: String, hash: String },
}

#[derive(Debug)]
enum TransferDone {
    Uploaded { path: String, hash: String },
    Downloaded { path: String, hash: String, bytes: Vec<u8> },
    Failed { path: String, upload: bool, error: String },
}

/// Runs attachment HTTP transfers off the engine's loop, one at a time, in blocking tasks.
async fn transfer_worker(
    agent: ureq::Agent,
    base: String,
    token: Option<String>,
    vault: VaultId,
    root: PathBuf,
    mut jobs: mpsc::UnboundedReceiver<TransferJob>,
    done: mpsc::UnboundedSender<TransferDone>,
) {
    while let Some(job) = jobs.recv().await {
        let (agent, base, token, root) = (agent.clone(), base.clone(), token.clone(), root.clone());
        let result = tokio::task::spawn_blocking(move || {
            run_transfer(&agent, &base, token.as_deref(), vault, &root, job)
        })
        .await
        .unwrap_or_else(|e| TransferDone::Failed {
            path: String::new(),
            upload: false,
            error: e.to_string(),
        });
        if done.send(result).is_err() {
            break;
        }
    }
}

fn run_transfer(
    agent: &ureq::Agent,
    base: &str,
    token: Option<&str>,
    vault: VaultId,
    root: &std::path::Path,
    job: TransferJob,
) -> TransferDone {
    let url = |hash: &str| format!("{base}/api/v1/vaults/{vault}/attachments/{hash}");
    let bearer = token.map(|t| format!("Bearer {t}"));
    match job {
        TransferJob::Upload { path } => {
            let bytes = match std::fs::read(root.join(&path)) {
                Ok(b) => b,
                Err(e) => return TransferDone::Failed { path, upload: true, error: e.to_string() },
            };
            let hash = hash_bytes(&bytes);
            // Content-addressed: skip the body if the server already has these bytes.
            let mut head = agent.head(&url(&hash));
            if let Some(b) = &bearer {
                head = head.header("authorization", b);
            }
            match head.call() {
                Ok(_) => return TransferDone::Uploaded { path, hash },
                Err(ureq::Error::StatusCode(404)) => {}
                Err(e) => return TransferDone::Failed { path, upload: true, error: e.to_string() },
            }
            let name = path.rsplit('/').next().unwrap_or(&path).to_owned();
            let mut put = agent
                .put(&url(&hash))
                .header("content-type", &mime_for_path(&path))
                .header("x-filename", &name);
            if let Some(b) = &bearer {
                put = put.header("authorization", b);
            }
            match put.send(&bytes[..]) {
                Ok(_) => TransferDone::Uploaded { path, hash },
                Err(e) => TransferDone::Failed { path, upload: true, error: e.to_string() },
            }
        }
        TransferJob::Download { path, hash } => {
            let mut get = agent.get(&url(&hash));
            if let Some(b) = &bearer {
                get = get.header("authorization", b);
            }
            let bytes = get
                .call()
                .and_then(|mut r| r.body_mut().with_config().limit(MAX_ATTACHMENT_BYTES).read_to_vec());
            match bytes {
                Ok(bytes) if hash_bytes(&bytes) == hash => TransferDone::Downloaded { path, hash, bytes },
                Ok(_) => TransferDone::Failed { path, upload: false, error: "hash mismatch".into() },
                Err(e) => TransferDone::Failed { path, upload: false, error: e.to_string() },
            }
        }
    }
}

/// Where a frame came from: the real server, or a local UI connected to the relay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Origin {
    Server,
    Peer(u64),
}

struct Peer {
    tx: mpsc::UnboundedSender<Vec<u8>>,
    subs: HashSet<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Handshake {
    Sent,
    Step2Received,
    Done,
}

struct NoteState {
    doc: NoteDoc,
    path: String,
}

struct PendingRemoval {
    path: String,
    id: NoteId,
    content_hash: String,
    since: Instant,
}

pub struct Engine {
    store: Store,
    proj: Projection,
    vault_id: VaultId,
    vault: VaultDoc,
    notes: HashMap<NoteId, NoteState>,
    by_path: HashMap<String, NoteId>,
    /// Notes with remote content changes not yet written to disk, and when they last changed.
    dirty: HashMap<NoteId, Instant>,
    /// Vault-relative paths touched on disk, and when they were last touched.
    pending_fs: HashMap<String, Instant>,
    pending_removals: Vec<PendingRemoval>,
    handshakes: HashMap<String, Handshake>,
    out: Option<mpsc::UnboundedSender<Vec<u8>>>,
    policy: RetentionPolicy,
    last_maintenance: Instant,
    once: bool,
    /// Set in `once` mode when something failed that a retry loop would otherwise hide.
    fatal: Option<String>,
    // Attachments (SPEC §6.3, §7)
    transfers: Option<mpsc::UnboundedSender<TransferJob>>,
    in_flight: HashSet<String>,
    pending_uploads: HashMap<String, String>,
    upload_retry_after: Option<Instant>,
    /// Attachment paths touched on disk, awaiting the debounce.
    pending_attachment_fs: HashMap<String, Instant>,
    /// Cache of blake3 hashes of local attachment files; invalidated by watcher events.
    local_hashes: HashMap<String, String>,
    /// Something changed that may have orphaned an attachment entry; checked once idle.
    orphan_check_due: bool,
    // Local relay (SPEC §3.2)
    peers: HashMap<u64, Peer>,
    /// Note docs a local UI created that have no vault entry yet (the UI writes text first).
    pending_docs: HashMap<NoteId, NoteDoc>,
}

impl Engine {
    pub fn open(opts: &SyncOptions) -> Result<Self> {
        let proj = Projection::new(&opts.vault_dir);
        std::fs::create_dir_all(proj.sidecar_dir())?;
        let mut store = Store::open(proj.sidecar_dir().join("local.db"))?;

        let stored: Option<VaultId> = store.meta_get("vault_id")?.map(|s| s.parse()).transpose()?;
        let vault_id = match (stored, opts.vault_id) {
            (Some(a), Some(b)) if a != b => {
                return Err(Error::Sync(format!("this directory already belongs to vault {a}, not {b}")));
            }
            (Some(a), _) => a,
            (None, Some(b)) => b,
            (None, None) => VaultId::new(),
        };
        store.meta_set("vault_id", &vault_id.to_string())?;
        let vault = store.load_vault_doc(vault_id)?;

        let mut notes = HashMap::new();
        let mut by_path = HashMap::new();
        for row in store.list_notes(vault_id)? {
            let doc = store.load_doc(DocId::Note(row.id))?;
            by_path.insert(row.path.clone(), row.id);
            notes.insert(row.id, NoteState { doc, path: row.path });
        }
        info!(%vault_id, notes = notes.len(), dir = %proj.root().display(), "vault opened");
        Ok(Self {
            store,
            proj,
            vault_id,
            vault,
            notes,
            by_path,
            dirty: HashMap::new(),
            pending_fs: HashMap::new(),
            pending_removals: Vec::new(),
            handshakes: HashMap::new(),
            out: None,
            policy: RetentionPolicy::default(),
            last_maintenance: Instant::now(),
            once: opts.once,
            fatal: None,
            transfers: None,
            in_flight: HashSet::new(),
            pending_uploads: HashMap::new(),
            upload_retry_after: None,
            pending_attachment_fs: HashMap::new(),
            local_hashes: HashMap::new(),
            orphan_check_due: true,
            peers: HashMap::new(),
            pending_docs: HashMap::new(),
        })
    }

    pub fn report(&self) -> SyncReport {
        SyncReport { vault_id: self.vault_id, notes: self.notes.len() }
    }

    fn is_idle(&self) -> bool {
        self.out.is_some()
            && self.handshakes.values().all(|h| *h == Handshake::Done)
            && self.dirty.is_empty()
            && self.pending_fs.is_empty()
            && self.pending_removals.is_empty()
            && self.pending_attachment_fs.is_empty()
            && self.in_flight.is_empty()
            && self.pending_uploads.is_empty()
    }

    // ---- Startup reconciliation -------------------------------------------------------------

    /// Bring the store in line with the directory: files changed/added/removed while we were
    /// not running are handled exactly like live watcher events.
    pub fn reconcile_disk(&mut self) -> Result<()> {
        let on_disk: HashSet<String> = self.proj.walk_notes()?.into_iter().collect();
        // Removals first so that renames can be matched by the creates that follow.
        let known: Vec<(NoteId, String)> = self.notes.iter().map(|(id, s)| (*id, s.path.clone())).collect();
        for (id, path) in known {
            if !on_disk.contains(&path) {
                match self.store.projected_text(DocId::Note(id))? {
                    Some(_) => self.local_remove(&path)?,
                    // Never projected (e.g. crashed before writing): write it now instead.
                    None => {
                        self.dirty.insert(id, Instant::now() - PROJECT_DEBOUNCE);
                    }
                }
            }
        }
        for path in on_disk {
            self.process_path(&path)?;
        }
        self.finalize_removals(true)?;
        // Notes that predate `id:` in front matter gain one on first sync.
        let ids: Vec<NoteId> = self.notes.keys().copied().collect();
        for id in ids {
            self.normalize_note(id)?;
        }
        // Referenced attachments whose upload never completed, or tracked attachments edited
        // while we were not running: the local file wins.
        for path in self.store.referenced_attachment_paths()? {
            self.want_upload(&path)?;
        }
        for (path, hash) in self.vault.attachment_entries() {
            if let Some(local) = self.local_hash(&path)?
                && local != hash
            {
                self.pending_uploads.insert(path, local);
            }
        }
        Ok(())
    }

    // ---- Filesystem side --------------------------------------------------------------------

    fn on_fs_event(&mut self, ev: FsEvent) {
        let abs = match ev {
            FsEvent::Created(p) | FsEvent::Modified(p) | FsEvent::Removed(p) => p,
        };
        if let Ok(rel) = abs.strip_prefix(self.proj.root()) {
            let rel = rel.to_string_lossy().replace('\\', "/");
            if Projection::is_note_path(&abs) {
                self.pending_fs.insert(rel, Instant::now());
            } else {
                self.local_hashes.remove(&rel);
                self.pending_attachment_fs.insert(rel, Instant::now());
            }
        }
    }

    /// Decide what a touched path means from the current disk state, not from the event kind:
    /// renames arrive as two events, editors write via temp files, and events coalesce anyway.
    fn process_path(&mut self, rel: &str) -> Result<()> {
        let exists = self.proj.resolve(rel)?.is_file();
        let known = self.by_path.get(rel).copied();
        match (exists, known) {
            (true, Some(id)) => self.local_edit(id, rel),
            (true, None) => self.local_create(rel),
            (false, Some(_)) => self.local_remove(rel),
            (false, None) => Ok(()),
        }
    }

    fn local_create(&mut self, rel: &str) -> Result<()> {
        let mut text = self.proj.read(rel)?;
        let fm_id = frontmatter::id_of(&text).and_then(|s| s.parse::<NoteId>().ok());
        // A known id whose old file is gone is a move — even if the content changed too.
        if let Some(fid) = fm_id
            && let Some(state) = self.notes.get(&fid)
            && !self.proj.resolve(&state.path)?.is_file()
        {
            info!(from = %state.path, to = %rel, "rename detected by id");
            self.pending_removals.retain(|r| r.id != fid);
            return self.apply_local_rename(fid, rel);
        }
        let hash = content_hash(&text);
        if let Some(pos) = self.pending_removals.iter().position(|r| r.content_hash == hash) {
            let removed = self.pending_removals.remove(pos);
            info!(from = %removed.path, to = %rel, "rename detected by content");
            return self.apply_local_rename(removed.id, rel);
        }
        // Adopt an unknown id from the file (e.g. moved in from another vault); a copy of an
        // existing note gets a fresh one.
        let id = match fm_id {
            Some(fid) if !self.notes.contains_key(&fid) => fid,
            _ => NoteId::new(),
        };
        let wanted = id.to_string();
        let rewrite = match fm_id {
            Some(fid) if fid != id => frontmatter::normalize(&strip_id_line(&text), &wanted),
            _ => frontmatter::normalize(&text, &wanted),
        };
        if let Some(fixed) = rewrite {
            text = fixed;
            self.proj.write(rel, &text)?;
        }
        let doc = NoteDoc::new();
        let update = doc.set_text(&text);
        self.store.append_update(DocId::Note(id), &update, None)?;
        self.store.set_projected_text(DocId::Note(id), rel, &text)?;
        self.by_path.insert(rel.to_owned(), id);
        self.notes.insert(id, NoteState { doc, path: rel.to_owned() });
        self.index(id, rel, &text)?;
        let vu = self.vault.set_path(id, rel);
        self.persist_and_send(DocId::Vault(self.vault_id), vu)?;
        self.handshake(DocId::Note(id));
        self.send_update(DocId::Note(id), update);
        info!(path = %rel, %id, "new note");
        Ok(())
    }

    fn local_edit(&mut self, id: NoteId, rel: &str) -> Result<()> {
        let on_disk = self.proj.read(rel)?;
        let last = self.store.projected_text(DocId::Note(id))?.map(|(_, t)| t).unwrap_or_default();
        if on_disk == last {
            return Ok(());
        }
        let update = ingest_external_edit(&self.notes[&id].doc, &last, &on_disk);
        self.store.set_projected_text(DocId::Note(id), rel, &on_disk)?;
        if !update.is_empty() {
            self.store.append_update(DocId::Note(id), &update, None)?;
            self.send_update(DocId::Note(id), update);
            self.index(id, rel, &on_disk)?;
            debug!(path = %rel, "local edit ingested");
        }
        // If the doc had diverged, the merged text differs from the file: write it back.
        if self.notes[&id].doc.text() != on_disk {
            self.dirty.insert(id, Instant::now());
        }
        self.normalize_note(id)?;
        Ok(())
    }

    /// Ensure the note text carries its id exactly once (SPEC §6.3); applies the fix as a CRDT
    /// edit and rewrites the file. Empty docs are skipped: their content has not arrived yet.
    fn normalize_note(&mut self, id: NoteId) -> Result<bool> {
        let Some(state) = self.notes.get(&id) else { return Ok(false) };
        let text = state.doc.text();
        if text.is_empty() {
            return Ok(false);
        }
        let Some(fixed) = frontmatter::normalize(&text, &id.to_string()) else { return Ok(false) };
        let path = state.path.clone();
        let update = state.doc.set_text(&fixed);
        self.store.append_update(DocId::Note(id), &update, None)?;
        self.send_update(DocId::Note(id), update);
        self.store.set_projected_text(DocId::Note(id), &path, &fixed)?;
        self.proj.write(&path, &fixed)?;
        self.index(id, &path, &fixed)?;
        debug!(path = %path, "front matter id normalised");
        Ok(true)
    }

    fn local_remove(&mut self, rel: &str) -> Result<()> {
        let Some(id) = self.by_path.get(rel).copied() else { return Ok(()) };
        let text = self.store.projected_text(DocId::Note(id))?.map(|(_, t)| t).unwrap_or_default();
        self.pending_removals.push(PendingRemoval {
            path: rel.to_owned(),
            id,
            content_hash: content_hash(&text),
            since: Instant::now(),
        });
        Ok(())
    }

    fn finalize_removals(&mut self, all: bool) -> Result<()> {
        let due: Vec<PendingRemoval> = {
            let (due, keep): (Vec<_>, Vec<_>) =
                self.pending_removals.drain(..).partition(|r| all || r.since.elapsed() >= RENAME_WINDOW);
            self.pending_removals = keep;
            due
        };
        for r in due {
            if self.proj.resolve(&r.path)?.is_file() {
                continue; // reappeared (editor swap-file dance); a later event handles it
            }
            info!(path = %r.path, id = %r.id, "note removed locally → trash");
            self.forget(r.id, &r.path)?;
            let vu = self.vault.remove(r.id);
            self.persist_and_send(DocId::Vault(self.vault_id), vu)?;
        }
        Ok(())
    }

    fn apply_local_rename(&mut self, id: NoteId, new_rel: &str) -> Result<()> {
        let old = self.notes[&id].path.clone();
        self.by_path.remove(&old);
        self.by_path.insert(new_rel.to_owned(), id);
        self.notes.get_mut(&id).unwrap().path = new_rel.to_owned();
        let text = self.notes[&id].doc.text();
        self.store.set_projected_text(DocId::Note(id), new_rel, &text)?;
        self.index(id, new_rel, &text)?;
        let vu = self.vault.set_path(id, new_rel);
        self.persist_and_send(DocId::Vault(self.vault_id), vu)?;
        // The file at the new path may also carry an edit relative to what we last projected.
        self.local_edit(id, new_rel)
    }

    /// Write a note's current text to disk (remote change or post-merge write-back).
    fn project(&mut self, id: NoteId) -> Result<()> {
        let Some(state) = self.notes.get(&id) else { return Ok(()) };
        let (path, text) = (state.path.clone(), state.doc.text());
        let unchanged =
            self.store.projected_text(DocId::Note(id))?.is_some_and(|(p, t)| p == path && t == text);
        if unchanged && self.proj.resolve(&path)?.is_file() {
            return Ok(());
        }
        self.store.set_projected_text(DocId::Note(id), &path, &text)?;
        self.proj.write(&path, &text)?;
        self.index(id, &path, &text)?;
        debug!(path = %path, "projected");
        self.normalize_note(id)?;
        Ok(())
    }

    fn index(&mut self, id: NoteId, rel: &str, text: &str) -> Result<()> {
        let ix = markdown::index(text)?;
        let title = ix.title.clone().or_else(|| file_stem(rel));
        self.store.upsert_note(id, self.vault_id, rel, title.as_deref())?;
        self.store.index_note(id, &ix)?;
        self.discover_attachments(rel, &ix)
    }

    // ---- Attachments ------------------------------------------------------------------------

    /// Every local file a note references becomes an attachment: hashed, uploaded when the
    /// server lacks it, and recorded in the vault doc so other replicas fetch it.
    fn discover_attachments(&mut self, note_rel: &str, ix: &NoteIndex) -> Result<()> {
        let mut targets: Vec<(String, bool)> =
            ix.wikilinks.iter().filter(|w| w.embed).map(|w| (w.target.clone(), true)).collect();
        targets.extend(ix.links.iter().map(|l| (l.clone(), false)));
        let mut paths: Vec<String> = Vec::new();
        for (target, wiki) in targets {
            if let Some(path) = self.resolve_attachment(note_rel, &target, wiki)?
                && !paths.contains(&path)
            {
                paths.push(path);
            }
        }
        let id = self.by_path.get(note_rel).copied();
        if let Some(id) = id {
            self.store.set_note_attachments(id, &paths)?;
        }
        self.orphan_check_due = true;
        for path in paths {
            self.want_upload(&path)?;
        }
        Ok(())
    }

    /// Map a link target to an existing non-note file in the vault (see [`resolve_reference`]).
    fn resolve_attachment(&self, note_rel: &str, target: &str, wiki: bool) -> Result<Option<String>> {
        let exists = |p: &str| self.is_attachment_file(p).unwrap_or(false);
        let all = || self.proj.walk_files().unwrap_or_default();
        Ok(resolve_reference(note_rel, target, wiki, exists, all))
    }

    fn is_attachment_file(&self, rel: &str) -> Result<bool> {
        let Ok(abs) = self.proj.resolve(rel) else { return Ok(false) };
        Ok(abs.is_file() && !Projection::is_note_path(&abs) && !self.proj.is_ignored(&abs))
    }

    fn local_hash(&mut self, rel: &str) -> Result<Option<String>> {
        if let Some(h) = self.local_hashes.get(rel) {
            return Ok(Some(h.clone()));
        }
        if !self.is_attachment_file(rel)? {
            return Ok(None);
        }
        let h = hash_bytes(&self.proj.read_bytes(rel)?);
        self.local_hashes.insert(rel.to_owned(), h.clone());
        Ok(Some(h))
    }

    fn want_upload(&mut self, rel: &str) -> Result<()> {
        let Some(hash) = self.local_hash(rel)? else { return Ok(()) };
        if self.vault.attachment_hash(rel).as_deref() == Some(hash.as_str()) || self.in_flight.contains(rel) {
            return Ok(());
        }
        self.pending_uploads.insert(rel.to_owned(), hash);
        Ok(())
    }

    fn flush_uploads(&mut self) {
        if self.out.is_none() || self.upload_retry_after.is_some_and(|t| Instant::now() < t) {
            return;
        }
        let Some(tx) = &self.transfers else { return };
        for (path, _) in self.pending_uploads.drain() {
            self.in_flight.insert(path.clone());
            let _ = tx.send(TransferJob::Upload { path });
        }
    }

    /// Fetch every vault-doc attachment whose local file is missing or has different content.
    fn reconcile_attachments(&mut self) -> Result<()> {
        if self.out.is_none() {
            return Ok(());
        }
        for (path, hash) in self.vault.attachment_entries() {
            if self.in_flight.contains(&path) || self.pending_uploads.contains_key(&path) {
                continue;
            }
            if self.local_hash(&path)?.as_deref() == Some(hash.as_str()) {
                continue;
            }
            if let Some(tx) = &self.transfers {
                self.in_flight.insert(path.clone());
                let _ = tx.send(TransferJob::Download { path, hash });
            }
        }
        Ok(())
    }

    /// Drop vault-doc entries no live note references any more (SPEC §9). Only runs when the
    /// replica is fully caught up, so a note whose content has not arrived yet cannot make its
    /// attachments look unreferenced. Local files are left alone; the server purges blobs.
    fn cleanup_orphans(&mut self) -> Result<()> {
        self.orphan_check_due = false;
        let referenced = self.store.referenced_attachment_paths()?;
        for (path, _) in self.vault.attachment_entries() {
            if !referenced.contains(&path) {
                info!(path = %path, "attachment no longer referenced; dropping from vault");
                let u = self.vault.remove_attachment(&path);
                self.persist_and_send(DocId::Vault(self.vault_id), u)?;
            }
        }
        Ok(())
    }

    /// Write uploaded bytes under `attachments/` (SPEC §6.3), reusing an identical existing
    /// file and suffixing the name on a content clash. Returns the vault-relative path.
    fn store_attachment(&mut self, name: &str, bytes: &[u8]) -> Result<String> {
        let safe: String = name
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or("file")
            .chars()
            .filter(|c| {
                !c.is_control()
                    && *c != ':'
                    && *c != '?'
                    && *c != '*'
                    && *c != '"'
                    && *c != '<'
                    && *c != '>'
                    && *c != '|'
            })
            .collect();
        let safe = if safe.trim().is_empty() { "file".to_owned() } else { safe };
        let hash = hash_bytes(bytes);
        let (stem, ext) = match safe.rsplit_once('.') {
            Some((s, e)) if !s.is_empty() => (s.to_owned(), format!(".{e}")),
            _ => (safe.clone(), String::new()),
        };
        let mut candidate = format!("attachments/{safe}");
        let mut n = 1;
        while self.is_attachment_file(&candidate)? {
            if self.local_hash(&candidate)?.as_deref() == Some(hash.as_str()) {
                return Ok(candidate); // same bytes already there
            }
            n += 1;
            candidate = format!("attachments/{stem}-{}{ext}", &hash[..6.min(hash.len())]);
            if n > 2 {
                candidate = format!("attachments/{stem}-{n}{ext}");
            }
        }
        self.proj.write_bytes(&candidate, bytes)?;
        self.local_hashes.insert(candidate.clone(), hash);
        Ok(candidate)
    }

    /// A tracked attachment file changed or vanished on disk.
    fn process_attachment_path(&mut self, rel: &str) -> Result<()> {
        self.local_hashes.remove(rel);
        if self.vault.attachment_hash(rel).is_none() {
            return Ok(()); // unreferenced file; picked up when a note references it
        }
        if self.is_attachment_file(rel)? {
            self.want_upload(rel)
        } else {
            // Deleted locally while still referenced: content-addressed refs must resolve, so
            // it is restored from the server (removing the reference is the way to drop it).
            self.reconcile_attachments()
        }
    }

    fn on_transfer_done(&mut self, done: TransferDone) -> Result<()> {
        match done {
            TransferDone::Uploaded { path, hash } => {
                self.in_flight.remove(&path);
                self.local_hashes.insert(path.clone(), hash.clone());
                let u = self.vault.set_attachment(&path, &hash);
                self.persist_and_send(DocId::Vault(self.vault_id), u)?;
                info!(path = %path, "attachment uploaded");
            }
            TransferDone::Downloaded { path, hash, bytes } => {
                self.in_flight.remove(&path);
                self.proj.write_bytes(&path, &bytes)?;
                self.local_hashes.insert(path.clone(), hash);
                info!(path = %path, size = bytes.len(), "attachment downloaded");
            }
            TransferDone::Failed { path, upload, error } => {
                self.in_flight.remove(&path);
                warn!(path = %path, upload, %error, "attachment transfer failed");
                if upload && let Some(h) = self.local_hashes.get(&path).cloned() {
                    self.pending_uploads.insert(path.clone(), h);
                    self.upload_retry_after = Some(Instant::now() + UPLOAD_RETRY);
                }
                if self.once {
                    self.fatal = Some(format!("attachment transfer failed for {path}: {error}"));
                }
            }
        }
        Ok(())
    }

    // ---- Vault doc reconciliation -----------------------------------------------------------

    /// Make the local note set match the vault doc: adopt new notes, move renamed files,
    /// remove deleted ones, and resolve two notes claiming one path (SPEC §4.3).
    fn reconcile_vault(&mut self) -> Result<()> {
        let entries = self.vault.entries();

        // Path collisions: the lowest id keeps the path; others get a numbered suffix. Every
        // replica applies the same deterministic rule, so they converge without coordination.
        let mut seen: HashMap<String, NoteId> = HashMap::new();
        for (id, path) in &entries {
            if let Some(&winner) = seen.get(path)
                && winner != *id
            {
                let mut n = 2;
                let mut candidate = suffixed(path, n);
                while seen.contains_key(&candidate) || self.by_path.get(&candidate).is_some_and(|x| x != id) {
                    n += 1;
                    candidate = suffixed(path, n);
                }
                warn!(path = %path, %id, renamed_to = %candidate, "path collision resolved");
                let vu = self.vault.set_path(*id, &candidate);
                self.persist_and_send(DocId::Vault(self.vault_id), vu)?;
                seen.insert(candidate, *id);
                continue;
            }
            seen.insert(path.clone(), *id);
        }
        let entries = self.vault.entries();

        let remote_ids: HashSet<NoteId> = entries.iter().map(|(id, _)| *id).collect();
        for (id, path) in entries {
            match self.notes.get(&id) {
                None => {
                    let doc = match self.pending_docs.remove(&id) {
                        Some(doc) => doc,
                        None => self.store.load_doc(DocId::Note(id))?,
                    };
                    self.store.upsert_note(id, self.vault_id, &path, file_stem(&path).as_deref())?;
                    self.by_path.insert(path.clone(), id);
                    self.notes.insert(id, NoteState { doc, path: path.clone() });
                    self.dirty.insert(id, Instant::now());
                    self.handshake(DocId::Note(id));
                    info!(path = %path, %id, "adopted note from vault");
                }
                Some(state) if state.path != path => {
                    let old = state.path.clone();
                    let old_abs = self.proj.resolve(&old)?;
                    let new_abs = self.proj.resolve(&path)?;
                    if old_abs.is_file() {
                        if let Some(parent) = new_abs.parent() {
                            std::fs::create_dir_all(parent)?;
                        }
                        std::fs::rename(&old_abs, &new_abs)?;
                    }
                    self.by_path.remove(&old);
                    self.by_path.insert(path.clone(), id);
                    self.notes.get_mut(&id).unwrap().path = path.clone();
                    let text = self.notes[&id].doc.text();
                    self.store.set_projected_text(DocId::Note(id), &path, &text)?;
                    self.index(id, &path, &text)?;
                    self.dirty.insert(id, Instant::now());
                    info!(from = %old, to = %path, "moved by remote");
                }
                Some(_) => {}
            }
        }
        // Entries only leave the vault doc through an explicit remove (local creates merge into
        // whatever the server sends), so anything we hold that is absent has been deleted.
        let gone: Vec<NoteId> = self.notes.keys().filter(|id| !remote_ids.contains(id)).copied().collect();
        for id in gone {
            let path = self.notes[&id].path.clone();
            info!(path = %path, %id, "removed by remote → trash");
            self.proj.remove(&path)?;
            self.forget(id, &path)?;
        }
        self.reconcile_attachments()
    }

    /// Drop a trashed note from memory and bookkeeping (its update log stays in the store).
    fn forget(&mut self, id: NoteId, path: &str) -> Result<()> {
        self.store.trash_note(id)?;
        self.store.clear_note_attachments(id)?;
        self.orphan_check_due = true;
        self.store.delete_projection(DocId::Note(id))?;
        self.by_path.remove(path);
        self.notes.remove(&id);
        self.dirty.remove(&id);
        self.handshakes.remove(&DocId::Note(id).to_string());
        Ok(())
    }

    // ---- Network side -----------------------------------------------------------------------

    fn on_connect(&mut self, out: mpsc::UnboundedSender<Vec<u8>>) {
        self.out = Some(out);
        self.handshakes.clear();
        self.handshake(DocId::Vault(self.vault_id));
        let ids: Vec<NoteId> = self.notes.keys().chain(self.pending_docs.keys()).copied().collect();
        for id in ids {
            self.handshake(DocId::Note(id));
        }
        if let Err(e) = self.reconcile_attachments() {
            warn!(%e, "reconciling attachments");
        }
    }

    fn on_disconnect(&mut self) {
        self.out = None;
        self.handshakes.clear();
    }

    fn doc_for(&self, id: NoteId) -> Option<&NoteDoc> {
        self.notes.get(&id).map(|s| &s.doc).or_else(|| self.pending_docs.get(&id))
    }

    fn state_vector_of(&self, doc: DocId) -> Option<yrs::StateVector> {
        match doc {
            DocId::Vault(_) => Some(self.vault.state_vector()),
            DocId::Note(id) => self.doc_for(id).map(|d| d.state_vector()),
        }
    }

    fn diff_of(&self, doc: DocId, sv: &yrs::StateVector) -> Option<Vec<u8>> {
        match doc {
            DocId::Vault(_) => Some(self.vault.diff_since(sv)),
            DocId::Note(id) => self.doc_for(id).map(|d| d.diff_since(sv)),
        }
    }

    fn handshake(&mut self, doc: DocId) {
        if self.out.is_none() {
            return;
        }
        let Some(sv) = self.state_vector_of(doc) else { return };
        self.send(doc, Message::Sync(SyncMessage::SyncStep1(sv)));
        self.handshakes.insert(doc.to_string(), Handshake::Sent);
    }

    /// To the real server only.
    fn send(&self, doc: DocId, msg: Message) {
        if let Some(out) = &self.out {
            let _ = out.send(Frame::new(doc.to_string(), &msg).encode());
        }
    }

    /// To local peers subscribed to `doc_id`, except `except`.
    fn broadcast_local(&self, doc_id: &str, bytes: &[u8], except: Option<u64>) {
        for (id, peer) in &self.peers {
            if Some(*id) != except && peer.subs.contains(doc_id) {
                let _ = peer.tx.send(bytes.to_vec());
            }
        }
    }

    /// An update produced here (disk edit, create, normalisation): everyone needs it.
    fn send_update(&self, doc: DocId, update: Vec<u8>) {
        if update.is_empty() {
            return;
        }
        let frame = Frame::new(doc.to_string(), &Message::Sync(SyncMessage::Update(update))).encode();
        if let Some(out) = &self.out {
            let _ = out.send(frame.clone());
        }
        self.broadcast_local(&doc.to_string(), &frame, None);
    }

    fn persist_and_send(&mut self, doc: DocId, update: Vec<u8>) -> Result<()> {
        if update.is_empty() {
            return Ok(());
        }
        self.store.append_update(doc, &update, None)?;
        self.send_update(doc, update);
        Ok(())
    }

    fn on_local_event(&mut self, ev: LocalEvent) {
        match ev {
            LocalEvent::PeerConnected { id, tx } => {
                self.peers.insert(id, Peer { tx, subs: HashSet::new() });
            }
            LocalEvent::PeerFrame { id, bytes } => self.handle_frame(Origin::Peer(id), &bytes),
            LocalEvent::PeerGone { id } => {
                self.peers.remove(&id);
            }
            LocalEvent::Query { query, reply } => {
                let _ = reply.send(self.answer(query));
            }
        }
    }

    fn answer(&mut self, q: LocalQuery) -> LocalReply {
        let r: Result<LocalReply> = (|| {
            Ok(match q {
                LocalQuery::Vaults => LocalReply::Vaults(vec![(self.vault_id, self.notes.len() as u32)]),
                LocalQuery::Notes => LocalReply::Notes(self.store.list_notes(self.vault_id)?),
                LocalQuery::Note(id) => {
                    LocalReply::Note(match (self.store.note_by_id(id)?, self.doc_for(id)) {
                        (Some(row), Some(doc)) => Some((row, doc.text())),
                        _ => None,
                    })
                }
                LocalQuery::Search { q, limit } => {
                    LocalReply::Search(self.store.search_in_vault(self.vault_id, &q, limit)?)
                }
                LocalQuery::Backlinks(id) => LocalReply::Backlinks(match self.store.note_by_id(id)? {
                    Some(row) => self.store.backlinks_to(&row)?,
                    None => Vec::new(),
                }),
                LocalQuery::Tags => LocalReply::Tags(self.store.tags_in_vault(self.vault_id)?),
                LocalQuery::Tagged(tag) => {
                    LocalReply::Tagged(self.store.notes_with_tag(self.vault_id, &tag)?)
                }
                LocalQuery::StoreAttachment { name, bytes } => {
                    let path = self.store_attachment(&name, &bytes)?;
                    LocalReply::Stored { path, hash: hash_bytes(&bytes) }
                }
                LocalQuery::Versions(id) => LocalReply::Versions(self.store.versions(DocId::Note(id))?),
                LocalQuery::VersionAt(id, seq) => LocalReply::VersionAt(
                    self.notes
                        .contains_key(&id)
                        .then(|| self.store.load_doc_at(DocId::Note(id), seq))
                        .transpose()?
                        .map(|d| d.text()),
                ),
                LocalQuery::SaveVersion(id, label) => {
                    let Some(doc) = self.doc_for(id) else {
                        return Ok(LocalReply::Error("unknown note".into()));
                    };
                    let now = now_ms();
                    let seq = self.store.snapshot_labeled_at(
                        DocId::Note(id),
                        &doc.encode_full(),
                        now,
                        Some(&label),
                        None,
                    )?;
                    LocalReply::SavedVersion(crate::store::VersionRow {
                        seq,
                        created_ms: now,
                        label: Some(label),
                        author: None,
                    })
                }
                LocalQuery::Attachment(hash) => {
                    let path =
                        self.vault.attachment_entries().into_iter().find(|(_, h)| *h == hash).map(|(p, _)| p);
                    LocalReply::Attachment(match path {
                        Some(p) if self.is_attachment_file(&p)? => {
                            Some((self.proj.read_bytes(&p)?, mime_for_path(&p)))
                        }
                        _ => None,
                    })
                }
            })
        })();
        r.unwrap_or_else(err_reply)
    }

    fn handle_frame(&mut self, origin: Origin, bytes: &[u8]) {
        if let Err(e) = self.try_handle_frame(origin, bytes) {
            warn!(%e, ?origin, "dropping frame");
        }
    }

    fn try_handle_frame(&mut self, origin: Origin, bytes: &[u8]) -> Result<()> {
        let frame = Frame::decode(bytes)?;
        let doc: DocId = frame.doc_id.parse()?;
        let msg = frame.message()?;
        let is_vault = doc == DocId::Vault(self.vault_id);
        match doc {
            DocId::Vault(v) if v != self.vault_id => return Ok(()),
            DocId::Note(id) if self.doc_for(id).is_none() => match origin {
                // A local UI creating a note: hold the doc until its vault entry arrives.
                Origin::Peer(_) => {
                    self.pending_docs.insert(id, NoteDoc::new());
                    self.handshake(DocId::Note(id));
                }
                Origin::Server => return Ok(()),
            },
            _ => {}
        }
        let peer_id = match origin {
            Origin::Peer(p) => Some(p),
            Origin::Server => None,
        };
        if let Some(p) = peer_id
            && let Some(peer) = self.peers.get_mut(&p)
        {
            peer.subs.insert(frame.doc_id.clone());
        }
        match msg {
            Message::Sync(SyncMessage::SyncStep1(sv)) => {
                let Some(diff) = self.diff_of(doc, &sv) else { return Ok(()) };
                let step2 = Frame::new(&frame.doc_id, &Message::Sync(SyncMessage::SyncStep2(diff))).encode();
                match origin {
                    Origin::Server => {
                        if let Some(out) = &self.out {
                            let _ = out.send(step2);
                        }
                        self.handshakes.insert(frame.doc_id, Handshake::Done);
                        if is_vault {
                            self.reconcile_vault()?;
                        }
                    }
                    Origin::Peer(p) => {
                        // Reply with our state and ask for theirs, like the server does.
                        let sv = self.state_vector_of(doc).expect("doc exists");
                        let step1 =
                            Frame::new(&frame.doc_id, &Message::Sync(SyncMessage::SyncStep1(sv))).encode();
                        if let Some(peer) = self.peers.get(&p) {
                            let _ = peer.tx.send(step2);
                            let _ = peer.tx.send(step1);
                        }
                    }
                }
            }
            Message::Sync(SyncMessage::SyncStep2(update)) | Message::Sync(SyncMessage::Update(update)) => {
                let changed = match doc {
                    DocId::Vault(_) => self.vault.apply_update(&update)?,
                    DocId::Note(id) => self.doc_for(id).expect("checked above").apply_update(&update)?,
                };
                if changed {
                    self.store.append_update(doc, &update, None)?;
                    let fanout =
                        Frame::new(&frame.doc_id, &Message::Sync(SyncMessage::Update(update))).encode();
                    if origin != Origin::Server
                        && let Some(out) = &self.out
                    {
                        let _ = out.send(fanout.clone());
                    }
                    self.broadcast_local(&frame.doc_id, &fanout, peer_id);
                    match doc {
                        DocId::Vault(_) => {
                            self.reconcile_vault()?;
                            self.orphan_check_due = true;
                        }
                        DocId::Note(id) => {
                            self.dirty.insert(id, Instant::now());
                        }
                    }
                }
                if origin == Origin::Server
                    && let Some(h) = self.handshakes.get_mut(&frame.doc_id)
                    && *h == Handshake::Sent
                {
                    *h = Handshake::Step2Received;
                }
            }
            Message::Awareness(_) => {
                // Presence is relayed verbatim in both directions.
                if origin != Origin::Server
                    && let Some(out) = &self.out
                {
                    let _ = out.send(bytes.to_vec());
                }
                self.broadcast_local(&frame.doc_id, bytes, peer_id);
            }
            Message::AwarenessQuery | Message::Auth(_) | Message::Custom(..) => {}
        }
        Ok(())
    }

    // ---- Periodic work ----------------------------------------------------------------------

    /// Apply the snapshot/pruning policy to every doc in the sidecar store.
    pub fn maintain_all(&mut self) -> Result<()> {
        self.last_maintenance = Instant::now();
        let now = now_ms();
        let mut snapshots = 0;
        let mut pruned = 0;
        let m = self
            .store
            .maintain(DocId::Vault(self.vault_id), &self.policy, now, || self.vault.encode_full())?;
        snapshots += m.snapshotted as usize;
        pruned += m.pruned_updates;
        for (id, state) in &self.notes {
            let m = self.store.maintain(DocId::Note(*id), &self.policy, now, || state.doc.encode_full())?;
            snapshots += m.snapshotted as usize;
            pruned += m.pruned_updates;
        }
        if snapshots > 0 || pruned > 0 {
            info!(snapshots, pruned, "maintenance");
        }
        Ok(())
    }

    fn tick(&mut self) -> Result<()> {
        let now = Instant::now();
        if now.duration_since(self.last_maintenance) >= MAINTENANCE_INTERVAL {
            self.maintain_all()?;
        }
        let due: Vec<String> = self
            .pending_fs
            .iter()
            .filter(|(_, t)| now.duration_since(**t) >= FS_DEBOUNCE)
            .map(|(p, _)| p.clone())
            .collect();
        if !due.is_empty() {
            // Missing paths first so a subsequent create can be matched as a rename.
            let mut due = due;
            due.sort_by_key(|p| self.proj.resolve(p).map(|a| a.is_file()).unwrap_or(false));
            for rel in due {
                self.pending_fs.remove(&rel);
                if let Err(e) = self.process_path(&rel) {
                    warn!(path = %rel, %e, "processing path");
                }
            }
        }
        self.finalize_removals(false)?;

        let due: Vec<String> = self
            .pending_attachment_fs
            .iter()
            .filter(|(_, t)| now.duration_since(**t) >= FS_DEBOUNCE)
            .map(|(p, _)| p.clone())
            .collect();
        for rel in due {
            self.pending_attachment_fs.remove(&rel);
            if let Err(e) = self.process_attachment_path(&rel) {
                warn!(path = %rel, %e, "processing attachment");
            }
        }
        self.flush_uploads();

        let ready: Vec<NoteId> = self
            .dirty
            .iter()
            .filter(|(_, t)| now.duration_since(**t) >= PROJECT_DEBOUNCE)
            .map(|(id, _)| *id)
            .collect();
        for id in ready {
            self.dirty.remove(&id);
            self.project(id)?;
        }
        if self.orphan_check_due && self.is_idle() {
            self.cleanup_orphans()?;
        }
        Ok(())
    }
}

/// Remove `id:` lines from the front matter so a fresh id can be written.
fn strip_id_line(text: &str) -> String {
    match frontmatter::block(text) {
        Some((range, _)) => {
            let body: String = text[range.clone()]
                .split_inclusive('\n')
                .filter(|l| !l.trim_start().starts_with("id:"))
                .collect();
            let mut out = text.to_owned();
            out.replace_range(range, &body);
            out
        }
        None => text.to_owned(),
    }
}

fn content_hash(text: &str) -> String {
    blake3::hash(text.as_bytes()).to_hex().to_string()
}

fn file_stem(rel: &str) -> Option<String> {
    std::path::Path::new(rel).file_stem().map(|s| s.to_string_lossy().into_owned())
}

/// `Projects/a.md` + 2 → `Projects/a (2).md`
fn suffixed(path: &str, n: u32) -> String {
    match path.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && !stem.ends_with('/') => format!("{stem} ({n}).{ext}"),
        _ => format!("{path} ({n})"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ws_urls() {
        assert_eq!(ws_url("http://h:1").unwrap(), "ws://h:1/ws");
        assert_eq!(ws_url("https://h/").unwrap(), "wss://h/ws");
        assert_eq!(ws_url("ws://h/ws").unwrap(), "ws://h/ws");
        assert!(ws_url("h:1").is_err());
    }

    #[test]
    fn http_urls() {
        assert_eq!(http_url("http://h:1/").unwrap(), "http://h:1");
        assert_eq!(http_url("ws://h:1/ws").unwrap(), "http://h:1");
        assert_eq!(http_url("wss://h").unwrap(), "https://h");
        assert!(http_url("h").is_err());
    }

    #[test]
    fn suffixes() {
        assert_eq!(suffixed("a.md", 2), "a (2).md");
        assert_eq!(suffixed("dir/a.b.md", 3), "dir/a.b (3).md");
        assert_eq!(suffixed("noext", 2), "noext (2)");
    }

    #[test]
    fn open_assigns_and_pins_vault_id() {
        let dir = tempfile::tempdir().unwrap();
        let base = SyncOptions {
            vault_dir: dir.path().into(),
            server_url: "http://x".into(),
            vault_id: None,
            once: true,
            ca_cert: None,
            token: None,
        };
        let e = Engine::open(&base).unwrap();
        let id = e.vault_id;
        drop(e);
        let again = Engine::open(&base).unwrap();
        assert_eq!(again.vault_id, id);
        drop(again);
        let other = SyncOptions { vault_id: Some(VaultId::new()), ..base.clone() };
        assert!(Engine::open(&other).is_err());
    }

    #[test]
    fn ids_are_written_adopted_and_used_for_moves() {
        let dir = tempfile::tempdir().unwrap();
        let opts = SyncOptions {
            vault_dir: dir.path().into(),
            server_url: "http://x".into(),
            vault_id: None,
            once: true,
            ca_cert: None,
            token: None,
        };
        let proj = Projection::new(dir.path());
        proj.write("plain.md", "# Plain\n").unwrap();
        proj.write("fm.md", "---\ntitle: FM\n---\nbody\n").unwrap();
        let mut e = Engine::open(&opts).unwrap();
        e.reconcile_disk().unwrap();
        let plain = e.by_path["plain.md"];
        let fm = e.by_path["fm.md"];
        assert_eq!(proj.read("plain.md").unwrap(), format!("---\nid: {plain}\n---\n# Plain\n"));
        assert_eq!(proj.read("fm.md").unwrap(), format!("---\ntitle: FM\nid: {fm}\n---\nbody\n"));
        assert_eq!(e.notes[&plain].doc.text(), proj.read("plain.md").unwrap());
        drop(e);

        // Move + edit at once (content hash no longer matches) resolves by id; a copy gets a
        // fresh id; a file carrying an unknown id keeps it.
        let moved = proj.read("plain.md").unwrap().replace("# Plain", "# Plain (moved)");
        proj.remove("plain.md").unwrap();
        proj.write("archive/plain.md", &moved).unwrap();
        let copy = proj.read("fm.md").unwrap();
        proj.write("fm-copy.md", &copy).unwrap();
        let foreign = NoteId::new();
        proj.write("foreign.md", &format!("---\nid: {foreign}\n---\nhi\n")).unwrap();
        let mut e = Engine::open(&opts).unwrap();
        e.reconcile_disk().unwrap();
        assert_eq!(e.by_path["archive/plain.md"], plain);
        assert!(!e.by_path.contains_key("plain.md"));
        assert!(e.notes[&plain].doc.text().contains("# Plain (moved)"));
        let copy_id = e.by_path["fm-copy.md"];
        assert_ne!(copy_id, fm);
        assert_eq!(
            frontmatter::id_of(&proj.read("fm-copy.md").unwrap()).as_deref(),
            Some(copy_id.to_string().as_str())
        );
        assert_eq!(e.by_path["foreign.md"], foreign);
        assert_eq!(e.notes.len(), 4);
    }

    #[test]
    fn reconcile_disk_creates_edits_renames_and_removes() {
        let dir = tempfile::tempdir().unwrap();
        let opts = SyncOptions {
            vault_dir: dir.path().into(),
            server_url: "http://x".into(),
            vault_id: None,
            once: true,
            ca_cert: None,
            token: None,
        };
        let proj = Projection::new(dir.path());
        proj.write("a.md", "# A\n").unwrap();
        proj.write("sub/b.md", "# B\n").unwrap();

        let mut e = Engine::open(&opts).unwrap();
        e.reconcile_disk().unwrap();
        assert_eq!(e.notes.len(), 2);
        let a = e.by_path["a.md"];
        assert_eq!(e.vault.entries().len(), 2);
        drop(e);

        // Offline changes: edit a, rename b, add c.
        proj.write("a.md", "# A\n\nmore\n").unwrap();
        std::fs::rename(dir.path().join("sub/b.md"), dir.path().join("b-renamed.md")).unwrap();
        proj.write("c.md", "# C\n").unwrap();
        let mut e = Engine::open(&opts).unwrap();
        e.reconcile_disk().unwrap();
        assert!(e.notes[&a].doc.text().ends_with("# A\n\nmore\n"), "{}", e.notes[&a].doc.text());
        assert!(e.by_path.contains_key("b-renamed.md") && !e.by_path.contains_key("sub/b.md"));
        assert_eq!(e.notes.len(), 3, "rename must not create a new note");
        assert_eq!(e.vault.path_of(e.by_path["b-renamed.md"]).as_deref(), Some("b-renamed.md"));
        drop(e);

        // Delete c.
        proj.remove("c.md").unwrap();
        let mut e = Engine::open(&opts).unwrap();
        e.reconcile_disk().unwrap();
        assert_eq!(e.notes.len(), 2);
        assert_eq!(e.vault.entries().len(), 2);
        assert_eq!(e.store.list_notes(e.vault_id).unwrap().len(), 2);
    }
}

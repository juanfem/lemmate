//! The client sync engine (SPEC §6.3, §7): keeps a projected vault directory, the local sidecar
//! store, and the server in agreement.
//!
//! One engine per vault. It owns every note doc in memory, persists every update to
//! `<vault>/.lemmate/local.db`, writes remote changes to disk (debounced), ingests local file
//! changes as CRDT edits, and speaks the framed Yjs protocol over a WebSocket. Being offline is
//! not an error: everything is journaled locally and the handshake on reconnect sends what the
//! other side is missing. The same engine will back the Tauri shells.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use std::net::SocketAddr;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message as WsMsg;
use tokio_tungstenite::{Connector, connect_async_tls_with_config};
use tracing::{debug, info, warn};

use crate::attachments::{MAX_ATTACHMENT_BYTES, hash_bytes, mime_for_path};
use crate::doc::NoteDoc;
use crate::error::{Error, Result};
use crate::frontmatter;
use crate::ids::{DocId, NoteId, VaultId};
use crate::import::{Upload, UploadReport};
pub use crate::local::LocalOptions;
use crate::local::{LocalEvent, LocalQuery, LocalReply, Outbox, Routes, err_reply, outbox};
use crate::markdown::{self, NoteIndex};
use crate::projection::{Projection, check_path, ingest_external_edit};
use crate::store::{NoteRow, RetentionPolicy, Store, now_ms};
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
/// A connection must have lasted this long before the reconnect delay starts over: a server
/// that accepts the upgrade and drops it at once would otherwise be hammered every second.
const RECONNECT_HEALTHY: Duration = Duration::from_secs(10);
/// How much may wait to be written to the server before the connection is dropped and the
/// reconnect handshake catches it up instead (see `local::Outbox`).
const SERVER_QUEUE_BYTES: usize = 64 << 20;
/// Filesystem events waiting for the engine. When the engine is busy the watcher thread waits,
/// rather than the queue growing.
const FS_QUEUE: usize = 16 * 1024;
/// Sidecar marker: attachments were recorded while standalone, so their bytes have never been
/// offered to a server. Set when [`Engine::flush_uploads`] records one, cleared once a
/// connected run has uploaded them all (SPEC §3.2, "a server is optional").
const ATTACHMENTS_LOCAL_ONLY: &str = "attachments_local_only";

#[derive(Debug, Clone)]
pub struct SyncOptions {
    pub vault_dir: PathBuf,
    /// `http://host:port` or `https://host:port`; `/ws` is appended. `None` is a **standalone**
    /// vault (SPEC §3.2): the engine keeps the projection, the sidecar and the local relay, and
    /// nothing goes on the wire.
    pub server_url: Option<String>,
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

/// The running engines with their local relay (SPEC §3.2): the address to point a UI at, and
/// the tasks to await or abort.
pub struct LocalHandle {
    pub addr: SocketAddr,
    /// The relay's key for this launch: every request needs it (`local::Guard`). Hand it to a
    /// page as [`LocalHandle::page_url`] does, or to a program as `Authorization: Bearer`.
    pub key: String,
    /// The first vault opened — the only one for a single-vault relay, and what a caller that
    /// wants to open the UI on one vault should use.
    pub vault_id: VaultId,
    /// The vaults this relay served at startup, in the order they were opened. A vault created
    /// later by a UI is not listed here; ask the relay (`GET /api/v1/vaults`) for the live set.
    pub vaults: Vec<VaultId>,
    /// Requests from the UI to give this app a server (SPEC §3.2), for a shell that named a
    /// configuration file to write; `None` for one that cannot be reconfigured from the page.
    /// Taken by the shell, which owns both halves of that job — signing in, and the file.
    pub connect: Option<mpsc::UnboundedReceiver<crate::local::ConnectAsk>>,
    /// Requests from the UI to sign out of the server, for the same shell as `connect`.
    pub sign_out: Option<mpsc::UnboundedReceiver<crate::local::SignOutAsk>>,
    /// Behind a lock because the set grows: opening a vault a UI created adds an engine.
    tasks: Arc<Mutex<Vec<tokio::task::JoinHandle<Result<SyncReport>>>>>,
    supervisor: Option<tokio::task::JoinHandle<()>>,
    server: tokio::task::JoinHandle<()>,
}

impl LocalHandle {
    /// The address to open the UI at: `/` with the key, which the relay trades for a cookie and
    /// drops from the address. `fragment` is the client-side route, `""` or like `#/v/…`.
    pub fn page_url(&self, fragment: &str) -> String {
        format!("http://{}/?key={}{fragment}", self.addr, self.key)
    }

    /// Run until the relay's listener stops — which is never, short of a bind error, so this is
    /// how a shell says "serve until I am killed".
    ///
    /// Not [`LocalHandle::wait`]: engines come and go under a running relay. One is added when
    /// the UI creates a vault, and one *ends* when a vault is merged away (SPEC §3.2), and the
    /// first engine finishing is no reason to take the other vaults down with it.
    pub async fn serve_forever(self) -> Result<()> {
        self.server.await.map_err(|e| Error::Sync(e.to_string()))
    }

    /// Wait for the first engine to finish. Only meaningful in `once` mode, which is only ever
    /// used with a single vault.
    pub async fn wait(self) -> Result<SyncReport> {
        let first = self.tasks.lock().ok().and_then(|mut t| (!t.is_empty()).then(|| t.remove(0)));
        let Some(first) = first else { return Err(Error::Sync("the relay has no engine".into())) };
        let r = first.await.map_err(|e| Error::Sync(e.to_string()))?;
        self.abort();
        r
    }
    pub fn abort(&self) {
        if let Ok(tasks) = self.tasks.lock() {
            for task in tasks.iter() {
                task.abort();
            }
        }
        if let Some(supervisor) = &self.supervisor {
            supervisor.abort();
        }
        self.server.abort();
    }
}

/// Bind the local relay, start the engine in a background task, and return once the listener
/// is up. The relay keeps working while the real server is unreachable.
pub async fn start(opts: SyncOptions, local: LocalOptions) -> Result<LocalHandle> {
    start_many(vec![opts], local).await
}

/// The same, for every vault a client holds at once (SPEC §9): one engine — folder, sidecar,
/// watcher and connection — per vault, all behind one relay, so the UI sees the workspace it
/// sees against the server. Vaults are opened in order and the first failure is returned, since
/// a shell that silently drops a vault is worse than one that will not start.
pub async fn start_many(opts: Vec<SyncOptions>, local: LocalOptions) -> Result<LocalHandle> {
    crate::tls::install_crypto_provider();
    if opts.is_empty() {
        return Err(Error::Sync("the local relay needs at least one vault".into()));
    }
    let mut engines = Vec::with_capacity(opts.len());
    for o in &opts {
        engines.push(Engine::open(o)?);
    }
    let vaults: Vec<VaultId> = engines.iter().map(|e| e.vault_id).collect();
    let template = opts[0].clone();
    let upstream = opts.iter().find_map(|o| {
        let url = o.server_url.clone()?;
        Some(crate::local::Upstream { url, token: o.token.clone(), ca_cert: o.ca_cert.clone() })
    });
    let served = crate::local::serve(&local, &vaults, upstream).await?;
    info!(addr = %served.addr, vaults = vaults.len(), "local relay listening");
    let tasks: Vec<_> = engines
        .into_iter()
        .zip(opts)
        .zip(served.events)
        .map(|((mut engine, o), rx)| {
            engine.routes = Some(served.routes.clone());
            tokio::spawn(run_inner(engine, o, rx))
        })
        .collect();
    let tasks = Arc::new(Mutex::new(tasks));

    // A UI may create a vault (SPEC §9): it mints the id itself and speaks it over the socket,
    // and the relay holds those frames while this opens a folder and an engine for it. Without
    // a root to put the folder in there is nothing to open, and `serve` never asks.
    let supervisor = match (served.wanted, local.vault_root.clone()) {
        (Some(mut wanted), Some(root)) => {
            let (registrar, routes, tasks) = (served.registrar, served.routes.clone(), tasks.clone());
            Some(tokio::spawn(async move {
                while let Some(vault) = wanted.recv().await {
                    match open_new_vault(&root, vault, &template, &registrar, &routes) {
                        Ok(Some(task)) => {
                            if let Ok(mut t) = tasks.lock() {
                                t.push(task);
                            }
                        }
                        Ok(None) => {}
                        Err(e) => warn!(%vault, %e, "could not open the vault the UI created"),
                    }
                }
            }))
        }
        _ => None,
    };
    Ok(LocalHandle {
        addr: served.addr,
        key: served.key,
        vault_id: vaults[0],
        vaults,
        connect: served.connect,
        sign_out: served.sign_out,
        tasks,
        supervisor,
        server: served.task,
    })
}

/// Open a vault a local UI has just created: a folder under `root`, an engine on it, and a
/// place in the relay. `Ok(None)` when the relay already holds it, which is what makes a
/// repeated request — a UI resending its handshake — harmless.
///
/// The folder is named after the vault id; the name the user gave it lives in the vault doc,
/// arrives moments later, and renames the folder on the next launch (`vaults::rehome`), when
/// nothing is holding it open.
fn open_new_vault(
    root: &Path,
    vault: VaultId,
    template: &SyncOptions,
    registrar: &crate::local::Registrar,
    routes: &Arc<Routes>,
) -> Result<Option<tokio::task::JoinHandle<Result<SyncReport>>>> {
    let dir = root.join(crate::vaults::default_folder_name(vault));
    std::fs::create_dir_all(&dir)?;
    let opts = SyncOptions { vault_dir: dir, vault_id: Some(vault), once: false, ..template.clone() };
    // Opened before registering, so a vault that cannot be opened is not one the relay claims.
    let mut engine = Engine::open(&opts)?;
    let Some(rx) = registrar.add(vault) else { return Ok(None) };
    engine.routes = Some(routes.clone());
    info!(%vault, dir = %opts.vault_dir.display(), "opened a vault created by the UI");
    Ok(Some(tokio::spawn(run_inner(engine, opts, rx))))
}

/// The vault a directory already belongs to, or `None` if it holds no sidecar yet. Lets a shell
/// work out which vaults it has on disk before opening any of them.
pub fn vault_id_at(vault_dir: &Path) -> Result<Option<VaultId>> {
    let db = Projection::new(vault_dir).sidecar_dir().join("local.db");
    if !db.is_file() {
        return Ok(None);
    }
    Store::open(db)?.meta_get("vault_id")?.map(|s| s.parse()).transpose()
}

/// The name a vault has been given (`meta.name` in its vault doc), read from a directory's
/// sidecar without starting an engine — what a shell labels the vault's folder with.
pub fn vault_name_at(vault_dir: &Path) -> Result<Option<String>> {
    let db = Projection::new(vault_dir).sidecar_dir().join("local.db");
    if !db.is_file() {
        return Ok(None);
    }
    let store = Store::open(db)?;
    let Some(id) = store.meta_get("vault_id")?.map(|s| s.parse()).transpose()? else {
        return Ok(None);
    };
    Ok(store.load_vault_doc(id)?.name())
}

async fn run_inner(
    mut engine: Engine,
    opts: SyncOptions,
    mut local_rx: mpsc::UnboundedReceiver<LocalEvent>,
) -> Result<SyncReport> {
    engine.reindex_if_stale()?;
    engine.reconcile_disk()?;
    engine.adopt_imports()?;
    engine.maintain_all()?;

    let (fs_tx, mut fs_rx) = mpsc::channel::<FsEvent>(FS_QUEUE);
    let (std_tx, std_rx) = std::sync::mpsc::sync_channel::<FsEvent>(FS_QUEUE);
    let _watcher = VaultWatcher::start(engine.proj.clone(), std_tx)?;
    std::thread::spawn(move || {
        while let Ok(ev) = std_rx.recv() {
            if fs_tx.blocking_send(ev).is_err() {
                break;
            }
        }
    });

    let Some(server_url) = opts.server_url.clone() else {
        return run_standalone(engine, &opts, local_rx, fs_rx).await;
    };
    let ws_url = ws_url(&server_url)?;
    let ca = opts.ca_cert.as_deref();
    let tls = if ws_url.starts_with("wss://") { Some(crate::tls::client_config(ca)?) } else { None };
    // Attachment bodies (up to 100 MiB) get the long transfer timeout; the retirement path below
    // is one small API call and gets the short one.
    let agent = crate::tls::transfer_agent(ca)?;
    let retire_agent = crate::tls::http_agent(ca)?;
    let (job_tx, job_rx) = mpsc::unbounded_channel::<TransferJob>();
    let (done_tx, mut done_rx) = mpsc::unbounded_channel::<TransferDone>();
    tokio::spawn(transfer_worker(
        agent,
        http_url(&server_url)?,
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
                let connected_at = Instant::now();
                let (mut sink, mut stream) = ws.split();
                let (out_tx, mut out_rx) = outbox(SERVER_QUEUE_BYTES);
                let overflow = out_tx.clone();
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
                        _ = overflow.overflowed() => {
                            warn!("the server is not keeping up; reconnecting to catch it up");
                            break false;
                        }
                        ev = fs_rx.recv() => if let Some(ev) = ev { engine.on_fs_event(ev) },
                        done = done_rx.recv() => if let Some(done) = done { engine.on_transfer_done(done) },
                        ev = local_rx.recv(), if local_alive => match ev {
                            Some(ev) => {
                                engine.on_local_event(ev);
                                if engine.retiring {
                                    let report = engine.report();
                                    engine.on_disconnect();
                                    writer.abort();
                                    // Only now, with nothing of ours still able to arrive on
                                    // that socket and re-create what we are about to delete.
                                    let (base, token, vault) =
                                        (http_url(&server_url)?, opts.token.clone(), engine.vault_id);
                                    let agent = retire_agent.clone();
                                    let deleted = tokio::task::spawn_blocking(move || {
                                        delete_vault_upstream(&agent, &base, token.as_deref(), vault)
                                    })
                                    .await;
                                    match deleted {
                                        Ok(Ok(())) => info!(vault = %report.vault_id, "vault deleted on the server"),
                                        Ok(Err(e)) => warn!(vault = %report.vault_id, %e,
                                            "merged locally, but the server still holds this vault — delete it there \
                                             or it comes back on the next launch"),
                                        Err(e) => warn!(%e, "deleting the vault on the server"),
                                    }
                                    return Ok(report);
                                }
                            }
                            None => local_alive = false,
                        },
                        _ = ticker.tick() => {
                            engine.tick();
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
                backoff = retry_delay(backoff, connected_at.elapsed());
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
                done = done_rx.recv() => if let Some(done) = done { engine.on_transfer_done(done) },
                ev = local_rx.recv(), if local_alive => match ev {
                    Some(ev) => {
                        engine.on_local_event(ev);
                        if engine.retiring {
                            return Ok(engine.report());
                        }
                    }
                    None => local_alive = false,
                },
                _ = tokio::time::sleep(TICK) => engine.tick(),
            }
        }
        backoff = (backoff * 2).min(RECONNECT_MAX);
    }
}

/// The loop for a vault with no server (SPEC §3.2): the projection, the watcher, the local
/// relay and the journal, and nothing on the wire.
///
/// It is the offline arm of [`run_inner`] without the deadline that ends it — there is no
/// connection to come back and no transfer worker, since a standalone vault's attachments never
/// leave this disk. `--once` still means "settle and report", which here is the moment the
/// debounced filesystem work has drained.
async fn run_standalone(
    mut engine: Engine,
    opts: &SyncOptions,
    mut local_rx: mpsc::UnboundedReceiver<LocalEvent>,
    mut fs_rx: mpsc::Receiver<FsEvent>,
) -> Result<SyncReport> {
    info!(vault = %engine.vault_id, "standalone: no server configured");
    let mut local_alive = true;
    let mut idle_since: Option<Instant> = None;
    // An interval, not a `sleep` per iteration: the debounced work — projection, indexing,
    // attachment bookkeeping — happens on this tick, and a UI busy enough to have an event
    // ready every time round would otherwise keep pushing the deadline away and starve it.
    let mut ticker = tokio::time::interval(TICK);
    loop {
        tokio::select! {
            ev = fs_rx.recv() => if let Some(ev) = ev { engine.on_fs_event(ev) },
            ev = local_rx.recv(), if local_alive => match ev {
                Some(ev) => {
                    engine.on_local_event(ev);
                    if engine.retiring {
                        return Ok(engine.report());
                    }
                }
                None => local_alive = false,
            },
            _ = ticker.tick() => {
                engine.tick();
                if let Some(msg) = engine.fatal.take() {
                    return Err(Error::Sync(msg));
                }
                if opts.once {
                    if engine.is_idle() {
                        let since = *idle_since.get_or_insert_with(Instant::now);
                        if since.elapsed() >= FS_DEBOUNCE {
                            return Ok(engine.report());
                        }
                    } else {
                        idle_since = None;
                    }
                }
            }
        }
    }
}

/// The delay before reconnecting after a connection that lasted `lasted`. Only one that held up
/// starts the delay over; one dropped straight after the upgrade is a server in trouble, and
/// backs off like a refused connection does.
fn retry_delay(backoff: Duration, lasted: Duration) -> Duration {
    if lasted >= RECONNECT_HEALTHY { RECONNECT_MIN } else { backoff }
}

/// Delete a vault on the server, which is how a merged-away vault stops existing for every
/// other client (SPEC §3.2). Called once the engine's socket is closed, so nothing it sent can
/// arrive afterwards and re-create the doc.
fn delete_vault_upstream(agent: &ureq::Agent, base: &str, token: Option<&str>, vault: VaultId) -> Result<()> {
    let mut req = agent.delete(format!("{}/api/v1/vaults/{vault}", base.trim_end_matches('/')));
    if let Some(t) = token {
        req = req.header("authorization", format!("Bearer {t}"));
    }
    req.call().map_err(|e| Error::Sync(e.to_string()))?;
    Ok(())
}

/// Remove empty directories under `root`, deepest first, so a retired vault does not leave a
/// skeleton of the tree it used to have. `root` itself is left to the caller.
fn prune_empty_dirs(root: &Path) {
    let Ok(entries) = std::fs::read_dir(root) else { return };
    for dir in entries.flatten().map(|e| e.path()).filter(|p| p.is_dir()) {
        prune_empty_dirs(&dir);
        let _ = std::fs::remove_dir(&dir);
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
    Uploaded {
        path: String,
        hash: String,
    },
    Downloaded {
        path: String,
        hash: String,
        bytes: Vec<u8>,
    },
    Failed {
        path: String,
        upload: bool,
        error: String,
    },
    /// The server refused an upload as too large (413). Sending it again cannot help.
    TooLarge {
        path: String,
    },
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
            let read = Projection::new(root).read_bytes(&path);
            let bytes = match read {
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
                Err(ureq::Error::StatusCode(413)) => TransferDone::TooLarge { path },
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MsgKind {
    Step2,
    Other,
}

/// Where a frame came from: the real server, or a local UI connected to the relay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Origin {
    Server,
    Peer(u64),
}

struct Peer {
    tx: Outbox,
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
    out: Option<Outbox>,
    policy: RetentionPolicy,
    last_maintenance: Instant,
    once: bool,
    /// No server to sync with: nothing waits on a connection (see [`SyncOptions::server_url`]).
    standalone: bool,
    /// Set by [`Engine::retire`]: this vault has been merged into another and its run loop is
    /// to end. The relay stops routing to it at the same moment.
    retiring: bool,
    /// Set in `once` mode when something failed that a retry loop would otherwise hide.
    fatal: Option<String>,
    // Attachments (SPEC §6.3, §7)
    transfers: Option<mpsc::UnboundedSender<TransferJob>>,
    in_flight: HashSet<String>,
    pending_uploads: HashMap<String, String>,
    upload_retry_after: Option<Instant>,
    /// Files the server will not take, with the hash that was refused: too big for
    /// [`MAX_ATTACHMENT_BYTES`], or answered 413 by a server with a lower limit. Left alone
    /// until the file changes, instead of being re-sent every few seconds forever.
    too_large: HashMap<String, String>,
    /// Attachment paths touched on disk, awaiting the debounce.
    pending_attachment_fs: HashMap<String, Instant>,
    /// Cache of blake3 hashes of local attachment files; invalidated by watcher events.
    local_hashes: HashMap<String, String>,
    /// Something changed that may have orphaned an attachment entry; checked once idle.
    orphan_check_due: bool,
    /// The kept files (`VaultDoc::kept_files`) and their hashes as of the last reconcile: one
    /// that has left the vault doc since was deleted or moved on purpose, somewhere.
    known_kept: HashMap<String, String>,
    /// Files kept for the vault rather than for a note (`attachments::vault_resources`):
    /// `_quarto.yml`, `_metadata.yml`, `export/`, and what they name.
    vault_resources: Vec<String>,
    /// Non-note files that appeared or changed on disk since the last `refresh_dependencies`:
    /// one may be what a note was waiting for, or a stylesheet that now imports more.
    touched_files: HashSet<String>,
    // Local relay (SPEC §3.2)
    peers: HashMap<u64, Peer>,
    /// Docs the server has refused, and the frame it refused them with, so a window opened
    /// after the refusal is told too — see the `Message::Auth` arm of `try_handle_frame`.
    denied: HashMap<String, Vec<u8>>,
    /// Note docs a local UI created that have no vault entry yet (the UI writes text first).
    pending_docs: HashMap<NoteId, NoteDoc>,
    /// Where the relay looks up which vault owns a note. `None` for a bare `run` with no relay.
    routes: Option<Arc<Routes>>,
    /// Set whenever the set of notes this engine holds changes, so `sync_routes` republishes it.
    routes_dirty: bool,
    /// Notes taken in from a file that names an id this replica has no history for, while the
    /// server may well have one (a folder re-joined with a fresh sidecar). Their doc waits for
    /// the server's state before the file is applied to it as an *edit* — inserting it into an
    /// empty doc would merge with the server's copy into the text twice. Never projected while
    /// waiting: the file is the truth until then.
    awaiting: HashSet<NoteId>,
    /// The vault's folder ignores case (macOS and Windows by default): `a.md` and `A.md` are
    /// one file, so two notes so named are a path clash.
    case_insensitive: bool,
    /// Notes whose vault path cannot be written on this disk (`projection::check_path`), with
    /// that path — reported once, not on every reconcile.
    unplaced: HashMap<NoteId, String>,
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
        let mut awaiting = HashSet::new();
        for row in store.list_notes(vault_id)? {
            let doc = store.load_doc(DocId::Note(row.id))?;
            // Taken in from a file and still waiting for the server when we last stopped: no
            // history yet, but a projected text — which is the file, not an empty note.
            if doc.state_vector() == yrs::StateVector::default()
                && store.projected_text(DocId::Note(row.id))?.is_some_and(|(_, t)| !t.is_empty())
            {
                awaiting.insert(row.id);
            }
            by_path.insert(row.path.clone(), row.id);
            notes.insert(row.id, NoteState { doc, path: row.path });
        }
        let case_insensitive = folder_ignores_case(&proj.sidecar_dir());
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
            standalone: opts.server_url.is_none(),
            retiring: false,
            fatal: None,
            transfers: None,
            in_flight: HashSet::new(),
            pending_uploads: HashMap::new(),
            upload_retry_after: None,
            too_large: HashMap::new(),
            pending_attachment_fs: HashMap::new(),
            local_hashes: HashMap::new(),
            orphan_check_due: true,
            known_kept: HashMap::new(),
            vault_resources: Vec::new(),
            touched_files: HashSet::new(),
            peers: HashMap::new(),
            denied: HashMap::new(),
            pending_docs: HashMap::new(),
            routes: None,
            routes_dirty: true,
            awaiting,
            case_insensitive,
            unplaced: HashMap::new(),
        })
    }

    pub fn report(&self) -> SyncReport {
        SyncReport { vault_id: self.vault_id, notes: self.notes.len() }
    }

    fn is_idle(&self) -> bool {
        (self.standalone || self.out.is_some())
            && self.awaiting.is_empty()
            && self.handshakes.values().all(|h| *h == Handshake::Done)
            && self.dirty.is_empty()
            && self.pending_fs.is_empty()
            && self.pending_removals.is_empty()
            && self.pending_attachment_fs.is_empty()
            && self.in_flight.is_empty()
            && self.pending_uploads.is_empty()
    }

    // ---- Startup reconciliation -------------------------------------------------------------

    /// Re-derive every note's tags, links and search text if they were written by an older
    /// indexer ([`markdown::INDEX_VERSION`]). The text is unchanged, so nothing is stamped as
    /// edited; only what the indexer reads out of it is new.
    pub fn reindex_if_stale(&mut self) -> Result<()> {
        if self.store.index_is_current()? {
            return Ok(());
        }
        let notes: Vec<(NoteId, String, String)> =
            self.notes.iter().map(|(id, s)| (*id, s.path.clone(), s.doc.text())).collect();
        for (id, path, text) in &notes {
            let ix = markdown::index(text)?;
            self.store.reindex_note(*id, &ix)?;
            self.discover_attachments(path, text, &ix)?;
        }
        self.store.mark_index_current()?;
        info!(vault_id = %self.vault_id, notes = notes.len(), "re-indexed notes for a newer indexer");
        Ok(())
    }

    /// Move what an offline `lemmate import obsidian` left in the sidecar — bookmarks and
    /// daily-note settings (`import::BOOKMARKS_FILE`, `import::DAILY_FILE`) — into the vault doc,
    /// where every replica sees them, and delete the files. A file that does not parse is left
    /// where it is, so nothing the user imported disappears unexplained.
    pub fn adopt_imports(&mut self) -> Result<()> {
        let dir = self.proj.sidecar_dir();
        let vault = DocId::Vault(self.vault_id);
        let marks = dir.join(crate::import::BOOKMARKS_FILE);
        if let Ok(raw) = std::fs::read_to_string(&marks) {
            match serde_json::from_str::<Vec<crate::vault_doc::Bookmark>>(&raw) {
                Ok(list) => {
                    let update = self.vault.add_bookmarks(&list);
                    self.persist_and_send(vault, update)?;
                    std::fs::remove_file(&marks)?;
                    info!(vault = %self.vault_id, bookmarks = list.len(), "adopted imported bookmarks");
                }
                Err(e) => warn!(%e, path = %marks.display(), "imported bookmarks do not parse"),
            }
        }
        let daily = dir.join(crate::import::DAILY_FILE);
        if let Ok(raw) = std::fs::read_to_string(&daily) {
            match serde_json::from_str::<crate::daily::DailySettings>(&raw) {
                Ok(settings) => {
                    let update = self.vault.set_daily(&settings);
                    self.persist_and_send(vault, update)?;
                    std::fs::remove_file(&daily)?;
                    info!(vault = %self.vault_id, "adopted imported daily-note settings");
                }
                Err(e) => warn!(%e, path = %daily.display(), "imported daily-note settings do not parse"),
            }
        }
        Ok(())
    }

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
        // One file that cannot be read — or named, on this disk — is that file's problem, not
        // the vault's: it is reported, and every other one is taken in.
        for path in on_disk {
            if let Err(e) = self.process_path(&path) {
                warn!(%path, %e, "skipping a note file");
            }
        }
        self.finalize_removals(true)?;
        // With no server there is nothing to wait for (see `awaiting`).
        if self.standalone {
            for id in self.awaiting.clone() {
                self.finish_adoption(id)?;
            }
        }
        // Notes that predate `id:` in front matter gain one on first sync.
        let ids: Vec<NoteId> = self.notes.keys().copied().collect();
        for id in ids {
            if let Err(e) = self.normalize_note(id) {
                warn!(%id, %e, "adding the id to a note");
            }
        }
        self.known_kept = self.kept_now();
        // Files may have arrived while we were not running — an image a note already linked to,
        // a theme its front matter already named, a partial a theme now imports — and nothing
        // else would notice, since the notes themselves did not change.
        self.refresh_dependencies(None)?;
        // Referenced attachments whose upload never completed, or tracked attachments edited
        // while we were not running: the local file wins.
        for path in self.store.referenced_attachment_paths()? {
            self.want_upload(&path)?;
        }
        self.backfill_standalone_attachments()?;
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
        if let Some(rel) = self.proj.relative(&abs) {
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
        // An adopted id may already have a history. Here: a note that was trashed, or one a
        // merge carried across (`LocalQuery::AdoptState`). Then the file is an *edit* of that
        // note, made by diffing — inserting the text into a fresh doc would, once the histories
        // meet, put it in twice. Unknown here but possibly known to the server (a folder joined
        // with a fresh sidecar): wait for the server's copy (`awaiting`) and diff against that.
        let adopted = fm_id == Some(id);
        let pending = self.pending_docs.remove(&id);
        let doc = match pending {
            Some(doc) => doc,
            None if adopted => self.store.load_doc(DocId::Note(id))?,
            None => NoteDoc::new(),
        };
        let await_server = adopted && !self.standalone && doc.state_vector() == yrs::StateVector::default();
        let update = if await_server { Vec::new() } else { doc.set_text(&text) };
        if !update.is_empty() {
            self.store.append_update(DocId::Note(id), &update, None)?;
        }
        self.store.set_projected_text(DocId::Note(id), rel, &text)?;
        self.by_path.insert(rel.to_owned(), id);
        self.notes.insert(id, NoteState { doc, path: rel.to_owned() });
        self.routes_dirty = true;
        if await_server {
            self.awaiting.insert(id);
        }
        self.index(id, rel, &text)?;
        let vu = self.vault.set_path(id, rel);
        self.persist_and_send(DocId::Vault(self.vault_id), vu)?;
        self.handshake(DocId::Note(id));
        self.send_update(DocId::Note(id), update);
        if await_server {
            info!(path = %rel, %id, "note taken in from disk; waiting for the server's copy of it");
        } else {
            info!(path = %rel, %id, "new note");
        }
        Ok(())
    }

    /// The server has answered for a note in `awaiting`: its doc now holds whatever history the
    /// server had (possibly none), and the file is applied to that as an edit.
    fn finish_adoption(&mut self, id: NoteId) -> Result<()> {
        if !self.awaiting.remove(&id) {
            return Ok(());
        }
        let Some(state) = self.notes.get(&id) else { return Ok(()) };
        let path = state.path.clone();
        let Ok(on_disk) = self.proj.read(&path) else {
            // The file went meanwhile; the server's copy is all there is.
            self.dirty.insert(id, Instant::now());
            return Ok(());
        };
        let update = state.doc.set_text(&on_disk);
        if !update.is_empty() {
            self.store.append_update(DocId::Note(id), &update, None)?;
            self.send_update(DocId::Note(id), update);
        }
        self.store.set_projected_text(DocId::Note(id), &path, &on_disk)?;
        self.index(id, &path, &on_disk)?;
        debug!(path = %path, "note taken in from disk, against the server's copy");
        self.normalize_note(id)?;
        Ok(())
    }

    fn local_edit(&mut self, id: NoteId, rel: &str) -> Result<()> {
        if self.awaiting.contains(&id) {
            return Ok(()); // the file is read when the server answers
        }
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
            if self.proj.resolve(&r.path).is_ok_and(|p| p.is_file()) {
                continue; // reappeared (editor swap-file dance); a later event handles it
            }
            info!(path = %r.path, id = %r.id, "note removed locally → trash");
            if let Err(e) = self.forget(r.id, &r.path) {
                warn!(path = %r.path, %e, "trashing a removed note");
            }
            let vu = self.vault.remove(r.id);
            self.persist_and_send(DocId::Vault(self.vault_id), vu)?;
        }
        Ok(())
    }

    /// After a rename, fix `[[links]]` in every note that pointed at the old path (SPEC §4.4).
    /// Done by the replica that performed or first observed the rename; the edits are ordinary
    /// CRDT changes, so replicas that do it concurrently converge on the same text.
    fn rewrite_links(&mut self, old: &str, new: &str) -> Result<()> {
        if old == new {
            return Ok(());
        }
        let referrers: Vec<NoteId> = self
            .store
            .note_by_path(self.vault_id, new)?
            .map(|row| self.store.backlinks_to(&NoteRow { path: old.to_owned(), ..row }))
            .transpose()?
            .unwrap_or_default()
            .into_iter()
            .map(|r| r.id)
            .collect();
        let notes = self.store.list_notes(self.vault_id)?;
        let paths: Vec<&str> = notes.iter().map(|n| n.path.as_str()).collect();
        for rid in referrers {
            let Some(state) = self.notes.get(&rid) else { continue };
            let text = state.doc.text();
            let bare = markdown::BareName::of(&state.path, old, new, &paths);
            if let Some(fixed) = markdown::rewrite_wikilinks(&text, old, new, bare) {
                let path = state.path.clone();
                let update = state.doc.set_text(&fixed);
                self.store.append_update(DocId::Note(rid), &update, None)?;
                self.send_update(DocId::Note(rid), update);
                self.store.set_projected_text(DocId::Note(rid), &path, &fixed)?;
                self.proj.write(&path, &fixed)?;
                self.index(rid, &path, &fixed)?;
                info!(note = %path, %old, %new, "links rewritten after rename");
            }
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
        self.local_edit(id, new_rel)?;
        self.rewrite_links(&old, new_rel)
    }

    /// Take in a save to a note's file that the watcher has not delivered yet — the file differs
    /// from what this replica last wrote or read there. Called before anything overwrites,
    /// moves or deletes the file, which would otherwise lose that save.
    fn ingest_unsaved(&mut self, id: NoteId) {
        let Some(path) = self.notes.get(&id).map(|s| s.path.clone()) else { return };
        let differs = match (self.store.projected_text(DocId::Note(id)), self.proj.read(&path)) {
            (Ok(Some((p, last))), Ok(on_disk)) => p == path && on_disk != last,
            _ => false,
        };
        if differs {
            self.pending_fs.remove(&path);
            if let Err(e) = self.local_edit(id, &path) {
                warn!(%path, %e, "taking in a save before writing over it");
            }
        }
    }

    /// Write a note's current text to disk (remote change or post-merge write-back).
    fn project(&mut self, id: NoteId) -> Result<()> {
        if self.awaiting.contains(&id) {
            return Ok(());
        }
        self.ingest_unsaved(id);
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
        self.discover_attachments(rel, text, &ix)
    }

    // ---- Writes through the local API (SPEC §13.1) ------------------------------------------
    //
    // Performed on the projected files: the same code path as an editor saving the file, so
    // ids, uploads, indexing, and the vault entry all follow.

    fn api_note(&self, id: NoteId) -> Result<LocalReply> {
        Ok(LocalReply::Written(match (self.store.note_by_id(id)?, self.doc_for(id)) {
            (Some(row), Some(doc)) => Some((row, doc.text())),
            _ => None,
        }))
    }

    /// A note path from the API, or `None` if no replica could write it (`check_path`). Only a
    /// `..` *segment* climbs: `v1..2.md` is an ordinary name.
    fn api_path(path: &str) -> Option<String> {
        let p = path.trim().trim_start_matches('/');
        if p.is_empty() {
            return None;
        }
        let p = if p.ends_with(".md") || p.ends_with(".qmd") { p.to_owned() } else { format!("{p}.md") };
        check_path(&p).is_ok().then_some(p)
    }

    fn api_create(&mut self, path: &str, content: &str) -> Result<LocalReply> {
        let Some(rel) = Self::api_path(path) else { return Ok(LocalReply::Conflict("bad path".into())) };
        if self.by_path.contains_key(&rel) || self.proj.resolve(&rel)?.exists() {
            return Ok(LocalReply::Conflict(rel));
        }
        self.proj.write(&rel, content)?;
        self.process_path(&rel)?;
        match self.by_path.get(&rel).copied() {
            Some(id) => self.api_note(id),
            None => Ok(LocalReply::Written(None)),
        }
    }

    /// Import one batch of an uploaded Obsidian vault (SPEC §11.4). Notes are created through
    /// the same path as any other write, so the projection, the index and the sync engine all
    /// see them; attachments keep the relative layout their notes reference them by, and are
    /// uploaded once a note that references them is ingested.
    fn api_import(&mut self, files: Vec<(String, Vec<u8>)>) -> Result<UploadReport> {
        let mut report = UploadReport::default();
        for (rel, bytes) in files {
            let Some(upload) = crate::import::import_upload(&rel, bytes) else {
                if crate::import::upload_rejected(&rel) {
                    report.skipped += 1;
                }
                continue;
            };
            match upload {
                Upload::Note { path, text, callouts, embeds } => match self.api_create(&path, &text)? {
                    LocalReply::Conflict(_) => report.skipped += 1,
                    _ => {
                        report.notes += 1;
                        report.callouts += callouts;
                        report.embeds += embeds;
                    }
                },
                Upload::Attachment { path, bytes } => {
                    // A name no replica could hold (`check_path`) is skipped, not the whole batch.
                    if bytes.len() as u64 > MAX_ATTACHMENT_BYTES
                        || self.proj.resolve(&path).is_ok_and(|p| p.exists())
                        || check_path(&path).is_err()
                    {
                        report.skipped += 1;
                        continue;
                    }
                    self.proj.write_bytes(&path, &bytes)?;
                    report.attachments += 1;
                }
                Upload::Bookmarks(marks) => {
                    let before = self.vault.bookmarks().len();
                    let update = self.vault.add_bookmarks(&marks);
                    report.bookmarks += self.vault.bookmarks().len() - before;
                    self.persist_and_send(DocId::Vault(self.vault_id), update)?;
                }
                Upload::Daily(settings) => {
                    let update = self.vault.set_daily(&settings);
                    self.persist_and_send(DocId::Vault(self.vault_id), update)?;
                    report.daily_notes = true;
                }
            }
        }
        Ok(report)
    }

    fn api_replace(&mut self, id: NoteId, content: &str) -> Result<LocalReply> {
        let Some(path) = self.notes.get(&id).map(|s| s.path.clone()) else {
            return Ok(LocalReply::Written(None));
        };
        let text = frontmatter::normalize(content, &id.to_string()).unwrap_or_else(|| content.to_owned());
        self.proj.write(&path, &text)?;
        self.local_edit(id, &path)?;
        self.api_note(id)
    }

    fn api_rename(&mut self, id: NoteId, path: &str) -> Result<LocalReply> {
        let Some(old) = self.notes.get(&id).map(|s| s.path.clone()) else {
            return Ok(LocalReply::Written(None));
        };
        let Some(new) = Self::api_path(path) else { return Ok(LocalReply::Conflict("bad path".into())) };
        if new == old {
            return Ok(LocalReply::Done);
        }
        if self.by_path.contains_key(&new) || self.proj.resolve(&new)?.exists() {
            return Ok(LocalReply::Conflict(new));
        }
        let (from, to) = (self.proj.resolve(&old)?, self.proj.resolve(&new)?);
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::rename(&from, &to)?;
        self.apply_local_rename(id, &new)?;
        Ok(LocalReply::Done)
    }

    fn api_delete(&mut self, id: NoteId) -> Result<LocalReply> {
        let Some(path) = self.notes.get(&id).map(|s| s.path.clone()) else {
            return Ok(LocalReply::Written(None));
        };
        self.proj.remove(&path)?;
        self.local_remove(&path)?;
        self.finalize_removals(true)?;
        Ok(LocalReply::Done)
    }

    // ---- Merging one vault into another (SPEC §3.2) -----------------------------------------

    /// Every attachment a merge has to carry: what the vault doc records, plus what the notes
    /// reference and this disk actually holds.
    ///
    /// The two differ for a moment after an image is added — standalone the entry is written on
    /// the next tick, and with a server when the upload completes — and a merge that trusted the
    /// doc alone would leave that file behind and move the note that points at it.
    fn merge_attachments(&mut self) -> Result<Vec<(String, String)>> {
        let mut entries: std::collections::BTreeMap<String, String> =
            self.vault.attachment_entries().into_iter().collect();
        for path in self.store.referenced_attachment_paths()? {
            if !entries.contains_key(&path)
                && let Some(hash) = self.local_hash(&path)?
            {
                entries.insert(path, hash);
            }
        }
        Ok(entries.into_iter().collect())
    }

    /// Take one file from a vault being merged in.
    ///
    /// A note is processed straight away, so its front-matter id is adopted before anything else
    /// arrives and the note is *the same note*, in a new place, rather than a copy. An
    /// attachment is only written: it becomes an attachment of this vault when a note that
    /// references it is indexed, which is exactly what happens a moment later.
    fn merge_write(&mut self, path: &str, bytes: &[u8]) -> Result<LocalReply> {
        if check_path(path).is_err() {
            return Ok(LocalReply::Conflict(format!("bad path {path}")));
        }
        if self.by_path.contains_key(path) {
            return Ok(LocalReply::Conflict(path.to_owned()));
        }
        self.proj.write_bytes(path, bytes)?;
        if Projection::is_note_path(&self.proj.resolve(path)?) {
            self.process_path(path)?;
        }
        Ok(LocalReply::Done)
    }

    /// Delete everything this vault owns and stop.
    ///
    /// Only what the vault knows about: its notes, and the attachments its doc records. Anything
    /// else the folder happens to hold is somebody's, not ours, so it stays — and is reported,
    /// along with whether the folder went with it.
    fn retire(&mut self) -> Result<LocalReply> {
        // Everything that can fail comes first, while nothing is gone yet. The same attachment
        // list the survey used, so one recorded a moment ago is not left behind.
        let attachments = self.merge_attachments()?;
        let paths: Vec<String> = self.notes.values().map(|s| s.path.clone()).collect();
        // The sidecar is what makes the folder a vault (SPEC §6.2): with it gone, nothing on
        // the next launch will open this directory as one. Its database is closed first —
        // Windows will not move or delete a file that is open — and the folder is renamed out
        // of the way in one step, so a failure leaves the vault exactly as it was.
        let sidecar = self.proj.sidecar_dir();
        if sidecar.is_dir() {
            let db = sidecar.join("local.db");
            self.store = Store::open_in_memory()?;
            let tomb = self.proj.root().join(format!(".lemmate-retired-{}", self.vault_id));
            if let Err(e) = std::fs::rename(&sidecar, &tomb) {
                self.store = Store::open(&db)?;
                return Err(e.into());
            }
            if let Err(e) = std::fs::remove_dir_all(&tomb) {
                warn!(path = %tomb.display(), %e, "could not remove a retired vault's sidecar");
            }
        }
        for path in paths {
            if let Err(e) = self.proj.remove(&path) {
                warn!(%path, %e, "removing a merged note");
            }
        }
        for (path, _) in attachments {
            let _ = self.proj.remove(&path);
        }
        self.retiring = true;
        let left = self.proj.walk_files().unwrap_or_default();
        prune_empty_dirs(self.proj.root());
        let folder_removed = std::fs::remove_dir(self.proj.root()).is_ok();
        info!(vault = %self.vault_id, left = left.len(), folder_removed, "vault retired after a merge");
        Ok(LocalReply::Retired { left, folder_removed })
    }

    // ---- Attachments ------------------------------------------------------------------------

    /// Every local file a note depends on becomes an attachment: hashed, uploaded when the
    /// server lacks it, and recorded in the vault doc so other replicas fetch it. "Depends on"
    /// is `attachments::dependencies`: its links and embeds, the files its front matter names
    /// (a Quarto theme, a filter), and what those import in turn.
    fn discover_attachments(&mut self, note_rel: &str, text: &str, ix: &NoteIndex) -> Result<()> {
        let paths = crate::attachments::dependencies(
            note_rel,
            text,
            ix,
            |p| self.is_attachment_file(p).unwrap_or(false),
            || self.proj.walk_files().unwrap_or_default(),
            |p| self.proj.read_bytes(p).ok(),
        );
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

    /// Re-derive dependencies that can change without any note changing, after `touched` files
    /// appeared or changed on disk (`None`: at startup, everything that may have while we were
    /// not running).
    ///
    /// - A file no one has recorded may be what a note named before it existed — `![](pic.png)`
    ///   written first, the image copied in after. The notes naming it are re-derived.
    /// - A stylesheet or YAML file may now name more: a new `@import` in `custom.scss` is a new
    ///   dependency of every note using it. Those notes are re-derived, and the vault's own
    ///   resources (`_quarto.yml`, `_metadata.yml`, `export/`) are worked out again.
    fn refresh_dependencies(&mut self, touched: Option<&HashSet<String>>) -> Result<()> {
        let mut affected: HashSet<NoteId> = HashSet::new();
        if touched.is_none_or(|t| t.iter().any(|p| crate::attachments::names_files(p))) {
            self.vault_resources = crate::attachments::vault_resources(
                |p| self.is_attachment_file(p).unwrap_or(false),
                || self.proj.walk_files().unwrap_or_default(),
                |p| self.proj.read_bytes(p).ok(),
            );
            for path in self.vault_resources.clone() {
                self.want_upload(&path)?;
            }
            affected.extend(
                self.store
                    .note_attachment_paths()?
                    .into_iter()
                    .filter(|(_, p)| crate::attachments::names_files(p))
                    .map(|(id, _)| id),
            );
        }
        let recorded: HashSet<String> = self.vault.attachment_entries().into_iter().map(|(p, _)| p).collect();
        let new: Vec<String> = match touched {
            Some(t) => t
                .iter()
                .filter(|p| !recorded.contains(*p) && self.is_attachment_file(p).unwrap_or(false))
                .cloned()
                .collect(),
            None => self.proj.walk_files()?.into_iter().filter(|p| !recorded.contains(p)).collect(),
        };
        if !new.is_empty() {
            let names: HashSet<String> =
                new.iter().filter_map(|p| p.rsplit('/').next()).map(str::to_owned).collect();
            // A few names are cheaper to look for in the text than to index every note for; a
            // note that does not contain the name cannot be naming the file.
            let prefilter = names.len() <= 32;
            for (id, state) in &self.notes {
                let text = state.doc.text();
                if prefilter
                    && !names
                        .iter()
                        .any(|n| text.contains(n.as_str()) || text.contains(&n.replace(' ', "%20")))
                {
                    continue;
                }
                let ix = markdown::index(&text)?;
                if crate::attachments::referred_names(&text, &ix).iter().any(|n| names.contains(n)) {
                    affected.insert(*id);
                }
            }
        }
        for id in affected {
            let Some(state) = self.notes.get(&id) else { continue };
            let (path, text) = (state.path.clone(), state.doc.text());
            let ix = markdown::index(&text)?;
            self.discover_attachments(&path, &text, &ix)?;
        }
        self.orphan_check_due = true;
        Ok(())
    }

    // ---- The file manager (SPEC §9) ------------------------------------------------------------

    /// What is at `path` now, as a hash: the file on disk, else the vault doc's entry (a file
    /// another replica has and this one has not fetched yet is still there).
    fn current_file_hash(&mut self, path: &str) -> Result<Option<String>> {
        if self.is_attachment_file(path)? {
            return self.local_hash(path);
        }
        Ok(self.vault.attachment_hash(path))
    }

    /// Write a file at a path the user chose, and keep it. The mark goes into the vault doc at
    /// once; the entry follows the way every attachment's does — at the next flush standalone,
    /// once the server has the bytes otherwise — so no replica is told of bytes it cannot get.
    fn put_file(
        &mut self,
        path: &str,
        bytes: &[u8],
        replace: bool,
        base: Option<String>,
    ) -> Result<LocalReply> {
        let current = self.current_file_hash(path)?;
        if let Some(current) = &current
            && (!replace || base.as_ref().is_some_and(|b| b != current))
        {
            return Ok(LocalReply::FileConflict(current.clone()));
        }
        let hash = hash_bytes(bytes);
        self.proj.write_bytes(path, bytes)?;
        self.local_hashes.insert(path.to_owned(), hash.clone());
        let u = self.vault.mark_kept(path);
        self.persist_and_send(DocId::Vault(self.vault_id), u)?;
        self.pending_uploads.insert(path.to_owned(), hash.clone());
        self.flush_uploads()?;
        self.touched_files.insert(path.to_owned());
        Ok(LocalReply::FileWritten { path: path.to_owned(), hash, created: current.is_none() })
    }

    /// Delete a file from the vault and from this disk.
    fn delete_file(&mut self, path: &str) -> Result<LocalReply> {
        let on_disk = self.is_attachment_file(path)?;
        let u = self.vault.delete_file(path);
        if !on_disk && u.is_empty() {
            return Ok(LocalReply::Written(None));
        }
        if on_disk {
            self.proj.remove(path)?;
        }
        self.local_hashes.remove(path);
        self.known_kept.remove(path);
        self.pending_uploads.remove(path);
        self.persist_and_send(DocId::Vault(self.vault_id), u)?;
        self.orphan_check_due = true;
        Ok(LocalReply::Done)
    }

    /// Move or rename a file, on disk and in the vault doc, and point the notes that use it at
    /// the new path (`files::rewrite_references`).
    fn move_file(&mut self, from: &str, to: &str) -> Result<LocalReply> {
        if from == to {
            return Ok(LocalReply::FileMoved { path: to.to_owned(), rewritten: 0 });
        }
        let on_disk = self.is_attachment_file(from)?;
        if !on_disk && self.vault.attachment_hash(from).is_none() {
            return Ok(LocalReply::Written(None));
        }
        if let Some(there) = self.current_file_hash(to)? {
            return Ok(LocalReply::FileConflict(there));
        }
        // The vault as it was, for working out what each reference meant.
        let mut files = self.proj.walk_files()?;
        files.extend(self.vault.attachment_entries().into_iter().map(|(p, _)| p));
        files.sort();
        files.dedup();
        let users: Vec<NoteId> = self
            .store
            .note_attachment_paths()?
            .into_iter()
            .filter(|(_, p)| p == from)
            .map(|(id, _)| id)
            .collect();
        if on_disk {
            let (a, b) = (self.proj.resolve(from)?, self.proj.resolve(to)?);
            if let Some(parent) = b.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::rename(a, b)?;
            if let Some(h) = self.local_hashes.remove(from) {
                self.local_hashes.insert(to.to_owned(), h);
            }
        }
        if let Some(u) = self.vault.move_file(from, to) {
            self.persist_and_send(DocId::Vault(self.vault_id), u)?;
        }
        if let Some(h) = self.known_kept.remove(from) {
            self.known_kept.insert(to.to_owned(), h);
        }
        let mut rewritten = 0;
        for id in users {
            let Some(state) = self.notes.get(&id) else { continue };
            let (path, text) = (state.path.clone(), state.doc.text());
            if let Some(new) = crate::files::rewrite_references(&path, &text, from, to, &files) {
                self.api_replace(id, &new)?;
                rewritten += 1;
            }
        }
        self.orphan_check_due = true;
        Ok(LocalReply::FileMoved { path: to.to_owned(), rewritten })
    }

    /// Whether a vault-doc attachment entry names a place an attachment may be written.
    fn attachment_path_ok(rel: &str) -> bool {
        check_path(rel).is_ok() && !Projection::is_note_path(Path::new(rel))
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

    /// A vault that ran standalone has attachment entries whose bytes no server has ever seen:
    /// `want_upload` skips them, because the vault doc already names the hash it would upload.
    /// Now that there is somewhere to send them, queue every one — `PUT` is content-addressed
    /// and idempotent, so re-sending a blob the server already holds costs one request.
    ///
    /// The marker survives a quit halfway through, and clearing it waits for the last upload
    /// ([`Engine::on_transfer_done`]), so an interrupted backfill simply happens again.
    fn backfill_standalone_attachments(&mut self) -> Result<()> {
        if self.standalone || self.store.meta_get(ATTACHMENTS_LOCAL_ONLY)?.is_none() {
            return Ok(());
        }
        let mut queued = 0;
        for (path, hash) in self.vault.attachment_entries() {
            if self.is_attachment_file(&path)? {
                self.pending_uploads.insert(path, hash);
                queued += 1;
            }
        }
        info!(queued, "uploading attachments recorded while standalone");
        if queued == 0 {
            self.store.meta_clear(ATTACHMENTS_LOCAL_ONLY)?;
        }
        Ok(())
    }

    fn flush_uploads(&mut self) -> Result<()> {
        // Standalone: there is nowhere to upload to, so the vault-doc entry an upload would
        // have written on completion is written here instead, from the file already on disk.
        // Everything downstream reads that map — the relay's `GET …/attachments/{hash}`, the
        // reconciliation above, orphan cleanup — so without this an image inserted into a note
        // would be on disk and invisible to the app.
        if self.standalone {
            for (path, hash) in std::mem::take(&mut self.pending_uploads) {
                self.local_hashes.insert(path.clone(), hash.clone());
                if self.vault.attachment_hash(&path).as_deref() != Some(hash.as_str()) {
                    let u = self.vault.set_attachment(&path, &hash);
                    self.persist_and_send(DocId::Vault(self.vault_id), u)?;
                    // The entry now names a hash no server has the bytes for; remember that,
                    // because only a later connected run can put that right.
                    self.store.meta_set(ATTACHMENTS_LOCAL_ONLY, "1")?;
                    info!(path = %path, "attachment recorded");
                }
            }
            return Ok(());
        }
        if self.out.is_none() || self.upload_retry_after.is_some_and(|t| Instant::now() < t) {
            return Ok(());
        }
        if self.transfers.is_none() {
            return Ok(());
        }
        let pending: Vec<(String, String)> = self.pending_uploads.drain().collect();
        for (path, hash) in pending {
            if self.too_large.get(&path) == Some(&hash) {
                continue;
            }
            let Ok(abs) = self.proj.resolve(&path) else { continue };
            let size = abs.metadata().map(|m| m.len()).unwrap_or(0);
            if size > MAX_ATTACHMENT_BYTES {
                warn!(
                    path = %path,
                    size,
                    limit = MAX_ATTACHMENT_BYTES,
                    "attachment too large to upload; it stays on this machine only"
                );
                self.too_large.insert(path, hash);
                continue;
            }
            self.in_flight.insert(path.clone());
            if let Some(tx) = &self.transfers {
                let _ = tx.send(TransferJob::Upload { path });
            }
        }
        Ok(())
    }

    /// Fetch every vault-doc attachment whose local file is missing or has different content.
    fn reconcile_attachments(&mut self) -> Result<()> {
        // Nothing to fetch from: a standalone vault's attachments only ever exist on this disk.
        if self.standalone || self.out.is_none() {
            return Ok(());
        }
        for (path, hash) in self.vault.attachment_entries() {
            if self.in_flight.contains(&path) || self.pending_uploads.contains_key(&path) {
                continue;
            }
            // An entry is another replica's word for where a file goes. Somewhere no replica
            // may write (`check_path`), or over a note, it does not go.
            if !Self::attachment_path_ok(&path) {
                debug!(path = %path, "not fetching an attachment to a path that cannot hold one");
                continue;
            }
            if self.local_hash(&path).ok().flatten().as_deref() == Some(hash.as_str()) {
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
            if !referenced.contains(&path)
                && !self.vault_resources.contains(&path)
                && !self.vault.is_kept(&path)
            {
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
        // Before the early return below: a file nothing records yet — a new `_vars.scss` that
        // an existing theme imports, a first `_quarto.yml` — is exactly the case to look at.
        if crate::attachments::names_files(rel) || self.vault.attachment_hash(rel).is_none() {
            self.touched_files.insert(rel.to_owned());
        }
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

    /// One failure here is one attachment's: reported, and the engine carries on.
    fn on_transfer_done(&mut self, done: TransferDone) {
        if let Err(e) = self.try_transfer_done(done) {
            warn!(%e, "finishing an attachment transfer");
        }
    }

    fn try_transfer_done(&mut self, done: TransferDone) -> Result<()> {
        match done {
            TransferDone::Uploaded { path, hash } => {
                self.in_flight.remove(&path);
                self.local_hashes.insert(path.clone(), hash.clone());
                let u = self.vault.set_attachment(&path, &hash);
                self.persist_and_send(DocId::Vault(self.vault_id), u)?;
                info!(path = %path, "attachment uploaded");
                // The last of a standalone backfill: the server now has every blob this
                // vault's entries name, so the marker has nothing left to say.
                if self.in_flight.is_empty()
                    && self.pending_uploads.is_empty()
                    && self.store.meta_get(ATTACHMENTS_LOCAL_ONLY)?.is_some()
                {
                    self.store.meta_clear(ATTACHMENTS_LOCAL_ONLY)?;
                }
            }
            TransferDone::Downloaded { path, hash, bytes } => {
                self.in_flight.remove(&path);
                if !Self::attachment_path_ok(&path) {
                    return Err(Error::PathEscape(path));
                }
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
            TransferDone::TooLarge { path } => {
                self.in_flight.remove(&path);
                warn!(path = %path, "the server refused the attachment as too large; it stays on this machine only");
                if let Some(h) = self.local_hashes.get(&path).cloned() {
                    self.too_large.insert(path.clone(), h);
                }
                if self.once {
                    self.fatal = Some(format!("attachment too large for the server: {path}"));
                }
            }
        }
        Ok(())
    }

    // ---- Vault doc reconciliation -----------------------------------------------------------

    /// Make the local note set match the vault doc: adopt new notes, move renamed files,
    /// remove deleted ones, and resolve two notes claiming one path (SPEC §4.3).
    ///
    /// Done in an order that cannot trip over itself. Whatever is on disk and not yet taken in
    /// is taken in first, so nothing below overwrites or deletes a save. Then every removal and
    /// every move gives up its old path — in the store and on disk — before any note takes a
    /// new one, so a swap, or a note moving onto a path another has just left, or a remote
    /// note landing where a local one was renamed away from, all simply work. A failure is one
    /// note's: it is logged and the rest goes ahead, with memory and store kept in step.
    fn reconcile_vault(&mut self) -> Result<()> {
        self.take_in_occupants();
        self.resolve_collisions()?;
        let wanted: HashMap<NoteId, String> = self.vault.entries().into_iter().collect();

        let mut gone: Vec<NoteId> = Vec::new();
        let mut moves: Vec<(NoteId, String, String)> = Vec::new();
        let mut unplaceable: Vec<(NoteId, String)> = Vec::new();
        for (id, state) in &self.notes {
            match wanted.get(id) {
                None => gone.push(*id),
                Some(to) if *to != state.path => {
                    if check_path(to).is_ok() {
                        moves.push((*id, state.path.clone(), to.clone()));
                    } else {
                        unplaceable.push((*id, to.clone()));
                    }
                }
                Some(_) => {}
            }
        }
        let mut new: Vec<(NoteId, String)> = Vec::new();
        for (id, path) in &wanted {
            if self.notes.contains_key(id) {
                continue;
            }
            if check_path(path).is_ok() {
                new.push((*id, path.clone()));
            } else {
                unplaceable.push((*id, path.clone()));
            }
        }
        for (id, path) in unplaceable {
            if self.unplaced.get(&id) != Some(&path) {
                warn!(%id, %path, "this note's path cannot be a file on any replica; it stays in the vault without one here");
                self.unplaced.insert(id, path);
            }
        }
        // A target still held by a note that is not leaving it — one whose own new path cannot
        // be written here — is not free. The note waits; nothing is overwritten.
        let leaving: HashSet<String> = gone
            .iter()
            .filter_map(|id| self.notes.get(id).map(|s| self.path_key(&s.path)))
            .chain(moves.iter().map(|(_, from, _)| self.path_key(from)))
            .collect();
        let held: HashMap<String, NoteId> =
            self.by_path.iter().map(|(p, id)| (self.path_key(p), *id)).collect();
        let blocked = |id: &NoteId, to: &str| {
            let key = self.path_key(to);
            held.get(&key).is_some_and(|holder| holder != id) && !leaving.contains(&key)
        };
        moves.retain(|(id, _, to)| !blocked(id, to));
        new.retain(|(id, to)| !blocked(id, to));

        // 1. Saves not yet taken in, for every note about to be moved or removed.
        for id in gone.iter().chain(moves.iter().map(|(id, _, _)| id)) {
            if self.awaiting.contains(id)
                && let Err(e) = self.finish_adoption(*id)
            {
                warn!(%id, %e, "taking in a note before it moves");
            }
            self.ingest_unsaved(*id);
        }
        // A note whose file could not be taken in keeps its file where it is.
        moves.retain(|(id, _, _)| !self.awaiting.contains(id));

        // 2. Removals. Only a file this replica wrote or read is deleted: anything else at that
        //    path is not the note's.
        for id in gone {
            let path = self.notes[&id].path.clone();
            info!(path = %path, %id, "removed by remote → trash");
            if self.projected_at(id, &path)
                && !self.awaiting.remove(&id)
                && let Err(e) = self.proj.remove(&path)
            {
                warn!(%path, %e, "removing a note's file");
            }
            if let Err(e) = self.forget(id, &path) {
                warn!(%path, %e, "trashing a note removed by remote");
            }
        }

        // 3. Moves: every old path is given up before any new one is taken. The store row is
        //    parked in the trash meanwhile, which frees its place in the (vault, path) index.
        for (id, from, _) in &moves {
            if let Err(e) = self.store.trash_note(*id) {
                warn!(%from, %e, "freeing a moved note's path");
            }
            if self.projected_at(*id, from)
                && let Err(e) = self.proj.remove(from)
            {
                warn!(%from, %e, "removing a moved note's old file");
            }
            if self.by_path.get(from) == Some(id) {
                self.by_path.remove(from);
            }
        }
        for (id, from, to) in &moves {
            self.by_path.insert(to.clone(), *id);
            if let Some(state) = self.notes.get_mut(id) {
                state.path = to.clone();
            }
            self.dirty.remove(id);
            if let Err(e) = self.place_moved(*id, to) {
                warn!(%from, %to, %e, "writing a moved note");
            }
            info!(%from, %to, "moved by remote");
        }
        for (_, from, to) in &moves {
            if let Err(e) = self.rewrite_links(from, to) {
                warn!(%from, %to, %e, "rewriting links after a move");
            }
        }

        // 4. Notes new to this replica.
        for (id, path) in new {
            let pending = self.pending_docs.remove(&id);
            let doc = match pending {
                Some(doc) => doc,
                None => match self.store.load_doc(DocId::Note(id)) {
                    Ok(doc) => doc,
                    Err(e) => {
                        warn!(%path, %e, "loading a note from the vault");
                        continue;
                    }
                },
            };
            if let Err(e) = self.store.upsert_note(id, self.vault_id, &path, file_stem(&path).as_deref()) {
                warn!(%path, %e, "adopting a note from the vault");
                self.pending_docs.insert(id, doc);
                continue;
            }
            self.unplaced.remove(&id);
            self.by_path.insert(path.clone(), id);
            self.notes.insert(id, NoteState { doc, path: path.clone() });
            self.dirty.insert(id, Instant::now());
            self.routes_dirty = true;
            self.handshake(DocId::Note(id));
            info!(path = %path, %id, "adopted note from vault");
        }
        if let Err(e) = self.drop_removed_kept_files() {
            warn!(%e, "removing kept files deleted elsewhere");
        }
        if let Err(e) = self.reconcile_attachments() {
            warn!(%e, "fetching attachments");
        }
        Ok(())
    }

    /// Two notes may not share a path; nor, on a folder that ignores case, a spelling.
    fn path_key(&self, path: &str) -> String {
        if self.case_insensitive { path.to_lowercase() } else { path.to_owned() }
    }

    /// Whether the file at `path` is one this replica last wrote or read for `id`.
    fn projected_at(&self, id: NoteId, path: &str) -> bool {
        self.store.projected_text(DocId::Note(id)).ok().flatten().is_some_and(|(p, _)| p == path)
    }

    /// A moved note, written at its new path with the text it has *now* — which includes any
    /// edit that arrived just before the move — and indexed there, which also takes it back
    /// out of the trash it was parked in.
    fn place_moved(&mut self, id: NoteId, to: &str) -> Result<()> {
        let text = self.notes[&id].doc.text();
        self.index(id, to, &text)?;
        match self.proj.write(to, &text) {
            Ok(()) => self.store.set_projected_text(DocId::Note(id), to, &text),
            Err(e) => {
                // Nothing of this note is on disk now: say so, or the next start would read the
                // missing file as a deletion. It is written again on its next change.
                self.store.delete_projection(DocId::Note(id))?;
                Err(e)
            }
        }
    }

    /// Note files saved where the vault doc now puts a note, that this engine has not taken in
    /// yet — the watcher's event is still waiting out its debounce. They are taken in now, as
    /// the notes they are, so the collision rule sees two notes where the write that follows
    /// would otherwise have erased one.
    fn take_in_occupants(&mut self) {
        let held: HashSet<String> = self.by_path.keys().map(|p| self.path_key(p)).collect();
        let occupied: Vec<String> = self
            .vault
            .entries()
            .into_iter()
            .filter(|(id, p)| self.notes.get(id).is_none_or(|s| s.path != *p))
            .map(|(_, p)| p)
            .filter(|p| !held.contains(&self.path_key(p)))
            .filter(|p| Projection::is_note_path(Path::new(p)))
            .filter(|p| self.proj.resolve(p).is_ok_and(|a| a.is_file()))
            .collect();
        for path in occupied {
            self.pending_fs.remove(&path);
            if let Err(e) = self.local_create(&path) {
                warn!(%path, %e, "taking in a note file before a remote one lands on it");
            }
        }
    }

    /// Two notes claiming one path: the lowest id keeps it, the others get a numbered suffix.
    /// Every replica applies the same rule to the same entries, so they converge without
    /// coordination. A folder that ignores case compares spellings, so `A.md` and `a.md` clash.
    fn resolve_collisions(&mut self) -> Result<()> {
        let entries = self.vault.entries();
        let mut taken: HashSet<String> = entries.iter().map(|(_, p)| self.path_key(p)).collect();
        taken.extend(self.by_path.keys().map(|p| self.path_key(p)));
        let mut seen: HashMap<String, NoteId> = HashMap::new();
        for (id, path) in &entries {
            let key = self.path_key(path);
            match seen.get(&key) {
                Some(winner) if winner != id => {
                    let mut n = 2;
                    let mut candidate = suffixed(path, n);
                    while taken.contains(&self.path_key(&candidate)) {
                        n += 1;
                        candidate = suffixed(path, n);
                    }
                    warn!(path = %path, %id, renamed_to = %candidate, "path collision resolved");
                    let vu = self.vault.set_path(*id, &candidate);
                    self.persist_and_send(DocId::Vault(self.vault_id), vu)?;
                    taken.insert(self.path_key(&candidate));
                    seen.insert(self.path_key(&candidate), *id);
                }
                _ => {
                    seen.insert(key, *id);
                }
            }
        }
        Ok(())
    }

    /// The kept files and their hashes, as the vault doc has them now.
    fn kept_now(&self) -> HashMap<String, String> {
        self.vault
            .kept_files()
            .into_iter()
            .filter_map(|p| self.vault.attachment_hash(&p).map(|h| (p, h)))
            .collect()
    }

    /// A kept file that left the vault doc was deleted or moved on purpose, on some replica: the
    /// copy here goes too — but only while it is still the copy that was synced. One edited here
    /// since is somebody's work, and stays. (Files a note merely used are left alone, as ever:
    /// nobody chose to remove those.)
    fn drop_removed_kept_files(&mut self) -> Result<()> {
        let now = self.kept_now();
        let before = std::mem::replace(&mut self.known_kept, now);
        for (path, hash) in before {
            if self.vault.attachment_hash(&path).is_some() {
                continue;
            }
            if self.is_attachment_file(&path)?
                && self.local_hash(&path).ok().flatten().as_deref() == Some(hash.as_str())
            {
                info!(path = %path, "kept file removed elsewhere; removing the copy here");
                if let Err(e) = self.proj.remove(&path) {
                    warn!(%path, %e, "removing a kept file");
                }
                self.local_hashes.remove(&path);
            }
        }
        Ok(())
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
        self.routes_dirty = true;
        self.handshakes.remove(&DocId::Note(id).to_string());
        Ok(())
    }

    // ---- Network side -----------------------------------------------------------------------

    fn on_connect(&mut self, out: Outbox) {
        self.out = Some(out);
        self.handshakes.clear();
        // Whatever was refused before is about to be asked again; the answer may differ, since
        // a permission is exactly the kind of thing that changes between two connections.
        self.denied.clear();
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
        // A kept file this replica recorded is one it knows, for `drop_removed_kept_files`: when
        // it later leaves the vault doc, that is a delete from elsewhere, not news.
        if doc == DocId::Vault(self.vault_id) {
            self.known_kept = self.kept_now();
        }
        Ok(())
    }

    /// Tell the relay which notes this vault owns, and take back the frames it was holding for
    /// notes we have just adopted. A note created by a local UI is written before its vault
    /// entry exists, so those frames — the note's first content — arrive before anyone can say
    /// where they belong; this is where they land.
    fn sync_routes(&mut self) {
        if !self.routes_dirty {
            return;
        }
        self.routes_dirty = false;
        let Some(routes) = self.routes.clone() else { return };
        let mut ids: HashSet<NoteId> = self.notes.keys().copied().collect();
        // Bounded: `claim` only releases frames for notes that were not ours a moment ago, and
        // each release empties that note's queue.
        for _ in 0..8 {
            let released = routes.claim(self.vault_id, &ids);
            if released.is_empty() {
                break;
            }
            for (peer, bytes) in released {
                self.handle_frame(Origin::Peer(peer), &bytes);
            }
            ids = self.notes.keys().copied().collect();
        }
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
        self.sync_routes();
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
                LocalQuery::Versions(id) => LocalReply::Versions(
                    self.notes
                        .contains_key(&id)
                        .then(|| crate::history::entries(&self.store.journal(DocId::Note(id))?))
                        .transpose()?,
                ),
                LocalQuery::LabelVersion(id, seq, label) => LocalReply::Labelled(
                    self.notes
                        .contains_key(&id)
                        .then(|| self.store.set_version_label(DocId::Note(id), seq, label.as_deref()))
                        .transpose()?
                        .flatten(),
                ),
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
                    LocalReply::SavedVersion(self.store.save_version(
                        DocId::Note(id),
                        &doc.encode_full(),
                        now_ms(),
                        &label,
                        None,
                    )?)
                }
                LocalQuery::CreateNote { path, content } => self.api_create(&path, &content)?,
                LocalQuery::Import { files } => LocalReply::Imported(self.api_import(files)?),
                LocalQuery::Survey => LocalReply::Survey {
                    name: self.vault.name(),
                    notes: self.notes.iter().map(|(id, s)| (*id, s.path.clone())).collect(),
                    attachments: self.merge_attachments()?,
                    // Merging a synced vault away ends with deleting it on the server, and a
                    // merge that could not finish that would leave the empty shell to be pulled
                    // back down on the next launch. Better to say so before anything moves.
                    blocked: (!self.standalone && self.out.is_none())
                        .then(|| "this vault syncs with a server it cannot reach right now".to_owned()),
                },
                LocalQuery::ReadFile(path) => LocalReply::File(self.proj.read_bytes(&path).ok()),
                LocalQuery::WriteFile { path, bytes } => self.merge_write(&path, &bytes)?,
                LocalQuery::Retire => self.retire()?,
                LocalQuery::NoteState(id) => {
                    LocalReply::NoteState(self.doc_for(id).map(NoteDoc::encode_full))
                }
                LocalQuery::AdoptState { id, state } => {
                    if self.notes.contains_key(&id) {
                        LocalReply::Conflict(id.to_string())
                    } else {
                        // Checked before it is journaled: a state that does not decode would
                        // make the note unloadable from then on.
                        NoteDoc::from_updates([state.as_slice()])?;
                        self.store.append_update(DocId::Note(id), &state, None)?;
                        LocalReply::Done
                    }
                }
                LocalQuery::ReplaceNote { id, content } => self.api_replace(id, &content)?,
                LocalQuery::RenameNote { id, path } => self.api_rename(id, &path)?,
                LocalQuery::DeleteNote(id) => self.api_delete(id)?,
                LocalQuery::Files => {
                    let users = self.store.note_attachment_paths()?;
                    let size = |p: &str, _: &str| self.proj.resolve(p).ok()?.metadata().ok().map(|m| m.len());
                    LocalReply::Files(crate::files::listing(
                        self.vault.attachment_entries(),
                        &self.vault.kept_files(),
                        &users,
                        size,
                    ))
                }
                LocalQuery::PutFile { path, bytes, replace, base } => {
                    self.put_file(&path, &bytes, replace, base)?
                }
                LocalQuery::DeleteFile(path) => self.delete_file(&path)?,
                LocalQuery::MoveFile { from, to } => self.move_file(&from, &to)?,
                LocalQuery::Export { id, format } => {
                    let Some(doc) = self.doc_for(id) else { return Ok(LocalReply::Written(None)) };
                    let text = doc.text();
                    let root = self.proj.root().to_path_buf();
                    let note_path = self.notes.get(&id).map(|n| n.path.clone()).unwrap_or_default();
                    let cites = crate::pandoc::citation_files(&note_path, &text, |p| {
                        self.proj.resolve(p).is_ok_and(|f| f.is_file())
                    });
                    let opts = crate::pandoc::ExportOptions {
                        resource_dir: Some(root.clone()),
                        bibliography: cites.bibliography.iter().map(|p| root.join(p)).collect(),
                        csl: cites.csl.map(|p| root.join(p)),
                        ..Default::default()
                    };
                    match crate::pandoc::render(&text, format, &opts) {
                        Ok((bytes, mime)) => LocalReply::Exported(bytes, mime),
                        Err(e) => LocalReply::Error(e.to_string()),
                    }
                }
                LocalQuery::RenderSource(id) => match (self.notes.get(&id), self.doc_for(id)) {
                    (Some(n), Some(doc)) => LocalReply::RenderSource {
                        path: n.path.clone(),
                        text: doc.text(),
                        attachments: self.vault.attachment_entries().into_iter().map(|(p, _)| p).collect(),
                        root: self.proj.root().to_path_buf(),
                    },
                    _ => LocalReply::Written(None),
                },
                LocalQuery::Trash => LocalReply::Trash(self.store.trashed_notes(self.vault_id)?),
                LocalQuery::Restore(id) => match self.store.restore_note(id)? {
                    Some(row) => {
                        let doc = self.store.load_doc(DocId::Note(id))?;
                        self.by_path.insert(row.path.clone(), id);
                        self.notes.insert(id, NoteState { doc, path: row.path.clone() });
                        self.routes_dirty = true;
                        let vu = self.vault.set_path(id, &row.path);
                        self.persist_and_send(DocId::Vault(self.vault_id), vu)?;
                        self.handshake(DocId::Note(id));
                        self.dirty.insert(id, Instant::now() - PROJECT_DEBOUNCE);
                        LocalReply::Written(Some((row, self.notes[&id].doc.text())))
                    }
                    None => LocalReply::Written(None),
                },
                LocalQuery::Daily(date) => {
                    let Some(day) = crate::daily::Date::parse(&date) else {
                        return Ok(LocalReply::Written(None)); // `local::daily` validated it already
                    };
                    let path = self.vault.daily().path_for(day);
                    match self.by_path.get(&path).copied() {
                        Some(id) => self.api_note(id)?,
                        None => self.api_create(&path, &format!("# {date}\n\n"))?,
                    }
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
        let msg_kind = match &msg {
            Message::Sync(SyncMessage::SyncStep2(_)) => MsgKind::Step2,
            _ => MsgKind::Other,
        };
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
            // Subscribing to something the server has refused: say so now rather than serving
            // the local copy as if it were in sync with a server that will not take it.
            if let Some(refusal) = self.denied.get(&frame.doc_id) {
                let _ = peer.tx.send(refusal.clone());
            }
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
                // The server's answer to our state vector: for a note taken in from a file,
                // the moment its history is here and the file can be applied as an edit.
                let answered = matches!(msg_kind, MsgKind::Step2) && origin == Origin::Server;
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
                if answered && let DocId::Note(id) = doc {
                    self.finish_adoption(id)?;
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
            Message::Auth(reason) => {
                // The server refusing a doc is the one failure a relay could hide completely:
                // the window stays green because the socket it talks to is the local one. Pass
                // the frame on — it is exactly what the web client gets, and the UI already
                // knows how to show it — and keep it, so a window opened later is told as well.
                if origin == Origin::Server
                    && let Some(reason) = reason
                {
                    warn!(doc = %frame.doc_id, %reason, "the server refused this doc");
                    self.denied.insert(frame.doc_id.clone(), bytes.to_vec());
                    self.broadcast_local(&frame.doc_id, bytes, None);
                    // No history is coming for it: the file is all there is.
                    if let DocId::Note(id) = doc {
                        self.finish_adoption(id)?;
                    }
                }
            }
            Message::AwarenessQuery | Message::Custom(..) => {}
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

    /// The debounced work. Nothing in here ends the engine: a note that cannot be written, or
    /// a store hiccup, is that note's or that moment's problem — logged, and retried when it
    /// next comes round — while every other vault file keeps syncing.
    fn tick(&mut self) {
        self.sync_routes();
        let now = Instant::now();
        if now.duration_since(self.last_maintenance) >= MAINTENANCE_INTERVAL
            && let Err(e) = self.maintain_all()
        {
            warn!(%e, "store maintenance");
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
        if let Err(e) = self.finalize_removals(false) {
            warn!(%e, "finishing removals");
        }

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
        if !self.touched_files.is_empty() {
            let touched = std::mem::take(&mut self.touched_files);
            if let Err(e) = self.refresh_dependencies(Some(&touched)) {
                warn!(%e, "refreshing attachment dependencies");
            }
        }
        if let Err(e) = self.flush_uploads() {
            warn!(%e, "queueing uploads");
        }

        let ready: Vec<NoteId> = self
            .dirty
            .iter()
            .filter(|(_, t)| now.duration_since(**t) >= PROJECT_DEBOUNCE)
            .map(|(id, _)| *id)
            .collect();
        for id in ready {
            self.dirty.remove(&id);
            if let Err(e) = self.project(id) {
                let path = self.notes.get(&id).map(|s| s.path.clone()).unwrap_or_default();
                warn!(%id, %path, %e, "could not write a note to disk; it keeps syncing");
            }
        }
        if self.orphan_check_due
            && self.is_idle()
            && let Err(e) = self.cleanup_orphans()
        {
            warn!(%e, "dropping unreferenced attachments");
        }
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

/// Whether names in `dir` are matched without regard to case, found by asking the filesystem
/// rather than guessing from the platform: a case-sensitive APFS volume, or a Linux folder on a
/// FAT stick, are both real.
fn folder_ignores_case(dir: &Path) -> bool {
    let probe = dir.join(format!("case-probe-{}", NoteId::new()));
    let upper = dir.join(probe.file_name().map(|n| n.to_string_lossy().to_uppercase()).unwrap_or_default());
    if std::fs::write(&probe, b"").is_err() {
        return cfg!(any(windows, target_os = "macos"));
    }
    let ignores = upper.exists();
    let _ = std::fs::remove_file(&probe);
    ignores
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
    fn reconnecting_backs_off_unless_the_connection_held() {
        let long = Duration::from_secs(16);
        assert_eq!(retry_delay(long, Duration::from_millis(200)), long, "dropped at once: keep backing off");
        assert_eq!(retry_delay(long, RECONNECT_HEALTHY), RECONNECT_MIN);
    }

    #[test]
    fn api_paths_refuse_only_what_climbs_or_hides() {
        assert_eq!(Engine::api_path("v1..2").as_deref(), Some("v1..2.md"));
        assert_eq!(Engine::api_path("/Projects/plan.qmd").as_deref(), Some("Projects/plan.qmd"));
        for bad in ["../up", "a/../../up.md", ".lemmate/x", ".git/config", "a\\b", "", "What?", "CON"] {
            assert_eq!(Engine::api_path(bad), None, "{bad:?}");
        }
    }

    // ---- The engine against a scripted server ----------------------------------------------
    //
    // A replica of the vault doc stands in for the server: changes made on it are fed to the
    // engine as the frames the server would send, and nothing runs on a timer unless asked.

    fn engine_at(dir: &Path) -> Engine {
        Engine::open(&SyncOptions {
            vault_dir: dir.into(),
            server_url: Some("http://x".into()),
            vault_id: None,
            once: true,
            ca_cert: None,
            token: None,
        })
        .unwrap()
    }

    fn from_server(e: &mut Engine, doc: DocId, msg: SyncMessage) {
        let frame = Frame::new(doc.to_string(), &Message::Sync(msg)).encode();
        e.try_handle_frame(Origin::Server, &frame).unwrap();
    }

    fn to_vault(e: &mut Engine, msg: SyncMessage) {
        let vault = DocId::Vault(e.vault_id);
        from_server(e, vault, msg);
    }

    fn remote_vault(e: &Engine) -> VaultDoc {
        VaultDoc::from_updates([e.vault.encode_full().as_slice()]).unwrap()
    }

    /// Several vault changes as the one update the server would relay after a reconnect.
    fn at_once(updates: &[Vec<u8>]) -> SyncMessage {
        SyncMessage::Update(
            yrs::merge_updates_v1(updates.iter().map(Vec::as_slice).collect::<Vec<_>>()).unwrap(),
        )
    }

    /// Write every note with a pending remote change, as the tick does once the debounce is out.
    fn settle(e: &mut Engine) {
        for t in e.dirty.values_mut() {
            *t -= PROJECT_DEBOUNCE;
        }
        e.tick();
    }

    /// A text for a note made elsewhere, front matter and all.
    fn remote_text(id: NoteId, body: &str) -> (NoteDoc, Vec<u8>) {
        let doc = NoteDoc::new();
        let u = doc.set_text(&format!("---\nid: {id}\n---\n{body}"));
        (doc, u)
    }

    /// The audit's wedge: offline, this device made `D.md`; meanwhile another made `D.md` too,
    /// with a lower id. The lower id keeps the path everywhere, so *ours* moves aside — and it
    /// has to be out of the store's (vault, path) slot before theirs is adopted into it.
    #[test]
    fn a_remote_note_landing_on_a_local_ones_path_moves_the_local_one_aside() {
        let dir = tempfile::tempdir().unwrap();
        let proj = Projection::new(dir.path());
        let theirs: NoteId = "00000000000000000000000001".parse().unwrap();
        proj.write("D.md", "# from this device\n").unwrap();
        let mut e = engine_at(dir.path());
        e.reconcile_disk().unwrap();
        let ours = e.by_path["D.md"];
        let remote = remote_vault(&e);
        let (_, text) = remote_text(theirs, "# from the server\n");
        to_vault(&mut e, SyncMessage::Update(remote.set_path(theirs, "D.md")));
        from_server(&mut e, DocId::Note(theirs), SyncMessage::Update(text));
        settle(&mut e);
        assert_eq!(e.by_path.get("D.md"), Some(&theirs));
        assert_eq!(e.by_path.get("D (2).md"), Some(&ours));
        assert_eq!(e.vault.path_of(ours).as_deref(), Some("D (2).md"));
        assert!(proj.read("D.md").unwrap().contains("# from the server"));
        assert!(proj.read("D (2).md").unwrap().contains("# from this device"));
        assert_eq!(e.store.note_by_path(e.vault_id, "D.md").unwrap().map(|r| r.id), Some(theirs));
        assert_eq!(e.store.note_by_path(e.vault_id, "D (2).md").unwrap().map(|r| r.id), Some(ours));
    }

    /// Two notes trading paths, and a note moving onto a path another has just left, in one
    /// update: every old path is given up before any new one is taken.
    #[test]
    fn swaps_and_moves_into_freed_paths_land_with_the_right_text() {
        let dir = tempfile::tempdir().unwrap();
        let proj = Projection::new(dir.path());
        for (p, t) in [("a.md", "# A\n"), ("b.md", "# B\n"), ("c.md", "# C\n")] {
            proj.write(p, t).unwrap();
        }
        let mut e = engine_at(dir.path());
        e.reconcile_disk().unwrap();
        let (a, b, c) = (e.by_path["a.md"], e.by_path["b.md"], e.by_path["c.md"]);
        let remote = remote_vault(&e);
        let swap = [remote.set_path(a, "b.md"), remote.set_path(b, "a.md")];
        to_vault(&mut e, at_once(&swap));
        assert!(proj.read("a.md").unwrap().contains("# B") && proj.read("b.md").unwrap().contains("# A"));
        assert_eq!((e.by_path["a.md"], e.by_path["b.md"]), (b, a));
        // c takes a's path, and a moves on to a new one.
        let chain = [remote.set_path(a, "d.md"), remote.set_path(c, "b.md")];
        to_vault(&mut e, at_once(&chain));
        settle(&mut e);
        assert!(proj.read("d.md").unwrap().contains("# A"));
        assert!(proj.read("b.md").unwrap().contains("# C"));
        assert!(!dir.path().join("c.md").exists());
        assert_eq!(e.vault.entries().len(), 3);
        assert_eq!(e.store.list_notes(e.vault_id).unwrap().len(), 3);
    }

    /// A remote edit and, before it reached the disk, a remote rename: the file lands at the new
    /// path with the edit in it, and the watcher's view of that move changes nothing.
    #[test]
    fn a_remote_edit_survives_a_remote_rename_right_after_it() {
        let dir = tempfile::tempdir().unwrap();
        let proj = Projection::new(dir.path());
        proj.write("T.md", "# T\n\nbody\n").unwrap();
        let mut e = engine_at(dir.path());
        e.reconcile_disk().unwrap();
        let id = e.by_path["T.md"];
        let there = NoteDoc::from_updates([e.notes[&id].doc.encode_full().as_slice()]).unwrap();
        let edit = there.set_text(&there.text().replace("body", "body EDITED REMOTELY"));
        let remote = remote_vault(&e);
        from_server(&mut e, DocId::Note(id), SyncMessage::Update(edit));
        to_vault(&mut e, SyncMessage::Update(remote.set_path(id, "moved.md")));
        assert!(proj.read("moved.md").unwrap().contains("EDITED REMOTELY"));
        assert!(!dir.path().join("T.md").exists());
        e.on_fs_event(FsEvent::Removed(dir.path().join("T.md")));
        e.on_fs_event(FsEvent::Created(dir.path().join("moved.md")));
        for t in e.pending_fs.values_mut() {
            *t -= FS_DEBOUNCE;
        }
        e.tick();
        settle(&mut e);
        assert!(e.notes[&id].doc.text().contains("EDITED REMOTELY"), "{}", e.notes[&id].doc.text());
        assert!(proj.read("moved.md").unwrap().contains("EDITED REMOTELY"));
    }

    /// An editor saved the file and the watcher has not said so yet when a remote change comes
    /// to be written, or a remote delete: the save is taken in first, not written over.
    #[test]
    fn a_save_not_yet_seen_is_taken_in_before_a_remote_write_or_delete() {
        let dir = tempfile::tempdir().unwrap();
        let proj = Projection::new(dir.path());
        proj.write("n.md", "# N\n\nfirst\n").unwrap();
        let mut e = engine_at(dir.path());
        e.reconcile_disk().unwrap();
        let id = e.by_path["n.md"];
        let on_disk = proj.read("n.md").unwrap();
        std::fs::write(dir.path().join("n.md"), format!("{on_disk}saved in vim\n")).unwrap();
        let there = NoteDoc::from_updates([e.notes[&id].doc.encode_full().as_slice()]).unwrap();
        let edit = there.set_text(&there.text().replace("first", "first (remote)"));
        from_server(&mut e, DocId::Note(id), SyncMessage::Update(edit));
        settle(&mut e);
        let now = proj.read("n.md").unwrap();
        assert!(now.contains("first (remote)") && now.contains("saved in vim"), "{now}");
        assert_eq!(e.notes[&id].doc.text(), now);

        std::fs::write(dir.path().join("n.md"), format!("{now}unsaved before the delete\n")).unwrap();
        let remote = remote_vault(&e);
        to_vault(&mut e, SyncMessage::Update(remote.remove(id)));
        assert!(!dir.path().join("n.md").exists());
        let kept = e.store.load_doc(DocId::Note(id)).unwrap().text();
        assert!(kept.contains("unsaved before the delete"), "the save is in the trashed note: {kept}");
    }

    /// On a folder that ignores case, `Plan.md` and `plan.md` are one file: a path clash.
    #[test]
    fn notes_differing_only_in_case_clash_on_a_folder_that_ignores_it() {
        let dir = tempfile::tempdir().unwrap();
        let mut e = engine_at(dir.path());
        e.case_insensitive = true;
        let (one, two): (NoteId, NoteId) =
            ("00000000000000000000000001".parse().unwrap(), "00000000000000000000000002".parse().unwrap());
        let remote = remote_vault(&e);
        let both = [remote.set_path(one, "Plan.md"), remote.set_path(two, "plan.md")];
        to_vault(&mut e, at_once(&both));
        assert_eq!(e.vault.path_of(one).as_deref(), Some("Plan.md"));
        assert_eq!(e.vault.path_of(two).as_deref(), Some("plan (2).md"));
    }

    /// One note at a path this disk cannot hold — under a file — is that note's problem: the
    /// engine keeps going, and the next note is written.
    #[test]
    fn a_note_that_cannot_be_written_does_not_stop_the_others() {
        let dir = tempfile::tempdir().unwrap();
        let proj = Projection::new(dir.path());
        proj.write("a.md", "# a\n").unwrap();
        let mut e = engine_at(dir.path());
        e.reconcile_disk().unwrap();
        let (blocked, later) = (NoteId::new(), NoteId::new());
        let remote = remote_vault(&e);
        let both = [remote.set_path(blocked, "a.md/b.md"), remote.set_path(later, "later.md")];
        to_vault(&mut e, at_once(&both));
        from_server(&mut e, DocId::Note(blocked), SyncMessage::Update(remote_text(blocked, "# b\n").1));
        from_server(&mut e, DocId::Note(later), SyncMessage::Update(remote_text(later, "# later\n").1));
        settle(&mut e);
        assert!(proj.read("later.md").unwrap().contains("# later"));
        assert!(e.notes.contains_key(&blocked), "still held, and still syncing");
    }

    /// What another replica can name is not what may be written: a note in the sidecar, files
    /// in `.git`, an attachment over a note.
    #[test]
    fn hostile_paths_from_the_vault_doc_write_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let proj = Projection::new(dir.path());
        proj.write("T.md", "# T\n").unwrap();
        let mut e = engine_at(dir.path());
        e.reconcile_disk().unwrap();
        let evil = NoteId::new();
        let remote = remote_vault(&e);
        let all = [
            remote.set_path(evil, ".lemmate/evil.md"),
            remote.set_attachment(".git/config", &"a".repeat(64)),
            remote.set_attachment("T.md", &"b".repeat(64)),
        ];
        to_vault(&mut e, at_once(&all));
        settle(&mut e);
        assert!(!dir.path().join(".lemmate/evil.md").exists() && !dir.path().join(".git").exists());
        assert!(!e.notes.contains_key(&evil) && e.unplaced.contains_key(&evil));
        assert_eq!(e.vault.path_of(evil).as_deref(), Some(".lemmate/evil.md"), "kept in the vault doc");
        assert!(!Engine::attachment_path_ok(".git/config") && !Engine::attachment_path_ok("T.md"));
        assert!(Engine::attachment_path_ok("attachments/x.png"));
        let err = e.try_transfer_done(TransferDone::Downloaded {
            path: "T.md".into(),
            hash: "b".repeat(64),
            bytes: b"overwritten".to_vec(),
        });
        assert!(err.is_err());
        assert!(proj.read("T.md").unwrap().contains("# T"));
    }

    /// A folder joined with a fresh sidecar holds files whose ids the server already has, with
    /// their history. The file waits for the server's copy and is applied to it as an edit —
    /// inserting it into a new doc would merge with the server's into the text twice.
    #[test]
    fn a_file_whose_id_the_server_knows_is_not_inserted_twice() {
        let first = tempfile::tempdir().unwrap();
        Projection::new(first.path()).write("Plan.md", "# Plan\n\nunique body line\n").unwrap();
        let mut e = engine_at(first.path());
        e.reconcile_disk().unwrap();
        let id = e.by_path["Plan.md"];
        let server_copy = e.notes[&id].doc.encode_full();
        let text = e.notes[&id].doc.text();
        let vault = e.vault_id;
        drop(e);

        let again = tempfile::tempdir().unwrap();
        Projection::new(again.path()).write("Plan.md", &text).unwrap();
        let mut e = Engine::open(&SyncOptions {
            vault_dir: again.path().into(),
            server_url: Some("http://x".into()),
            vault_id: Some(vault),
            once: true,
            ca_cert: None,
            token: None,
        })
        .unwrap();
        e.reconcile_disk().unwrap();
        assert!(e.awaiting.contains(&id));
        settle(&mut e);
        assert_eq!(Projection::new(again.path()).read("Plan.md").unwrap(), text, "not written while waiting");
        from_server(&mut e, DocId::Note(id), SyncMessage::SyncStep2(server_copy));
        assert!(!e.awaiting.contains(&id));
        assert_eq!(e.notes[&id].doc.text(), text);
        assert_eq!(e.notes[&id].doc.text().matches("unique body line").count(), 1);
    }

    /// Retiring renames the sidecar away in one step; when that fails, nothing is gone and the
    /// vault carries on.
    #[cfg(unix)]
    #[test]
    fn a_retire_that_cannot_remove_the_sidecar_removes_nothing() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("v");
        Projection::new(&root).write("n.md", "# n\n").unwrap();
        let mut e = engine_at(&root);
        e.reconcile_disk().unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o555)).unwrap();
        if std::fs::write(root.join("probe"), b"").is_ok() {
            return; // running as root: permissions do not stop us, so nothing to test
        }
        let result = e.retire();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(result.is_err());
        assert!(root.join("n.md").is_file() && root.join(".lemmate/local.db").is_file());
        assert!(!e.retiring);
        assert_eq!(e.store.list_notes(e.vault_id).unwrap().len(), 1, "the real store is back");
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
            server_url: Some("http://x".into()),
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

    /// What an offline `lemmate import obsidian` leaves in the sidecar ends up in the vault doc —
    /// persisted, so it survives a restart and reaches the server — and the files go.
    #[test]
    fn imported_settings_are_adopted_into_the_vault_doc() {
        let dir = tempfile::tempdir().unwrap();
        let src = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(src.path().join(".obsidian")).unwrap();
        std::fs::write(src.path().join("N.md"), "n\n").unwrap();
        std::fs::write(
            src.path().join(".obsidian/bookmarks.json"),
            r#"{"items":[{"type":"file","path":"N.md"}]}"#,
        )
        .unwrap();
        std::fs::write(
            src.path().join(".obsidian/daily-notes.json"),
            r#"{"folder":"Journal","format":"DD.MM.YYYY"}"#,
        )
        .unwrap();
        crate::import::import_obsidian(src.path(), dir.path(), &Default::default()).unwrap();
        let sidecar = dir.path().join(crate::SIDECAR_DIR);
        assert!(sidecar.join(crate::import::DAILY_FILE).is_file());

        let opts = SyncOptions {
            vault_dir: dir.path().into(),
            server_url: None,
            vault_id: None,
            once: true,
            ca_cert: None,
            token: None,
        };
        let mut e = Engine::open(&opts).unwrap();
        e.adopt_imports().unwrap();
        assert!(!sidecar.join(crate::import::DAILY_FILE).exists());
        assert!(!sidecar.join(crate::import::BOOKMARKS_FILE).exists());
        drop(e);
        let e = Engine::open(&opts).unwrap();
        assert_eq!(e.vault.bookmarks().len(), 1);
        let day = crate::daily::Date::parse("2026-09-25").unwrap();
        assert_eq!(e.vault.daily().path_for(day), "Journal/25.09.2026.md");
    }

    /// A sidecar indexed by an older indexer is re-derived on start, once.
    #[test]
    fn stale_index_is_rederived_on_start() {
        let dir = tempfile::tempdir().unwrap();
        let opts = SyncOptions {
            vault_dir: dir.path().into(),
            server_url: None,
            vault_id: None,
            once: true,
            ca_cert: None,
            token: None,
        };
        Projection::new(dir.path()).write("t.md", "| a |\n|---|\n| #in-table |\n").unwrap();
        let mut e = Engine::open(&opts).unwrap();
        e.reconcile_disk().unwrap();
        let id = e.by_path["t.md"];
        // What an older indexer left behind: nothing read out of the table, no version recorded.
        e.store.reindex_note(id, &NoteIndex::default()).unwrap();
        e.store.meta_clear("index_version").unwrap();
        drop(e);

        let mut e = Engine::open(&opts).unwrap();
        e.reindex_if_stale().unwrap();
        assert_eq!(e.store.tags_in_vault(e.vault_id).unwrap(), vec![("in-table".to_owned(), 1)]);
        assert!(e.store.index_is_current().unwrap());
    }

    #[test]
    fn ids_are_written_adopted_and_used_for_moves() {
        let dir = tempfile::tempdir().unwrap();
        let opts = SyncOptions {
            vault_dir: dir.path().into(),
            server_url: Some("http://x".into()),
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
            server_url: Some("http://x".into()),
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

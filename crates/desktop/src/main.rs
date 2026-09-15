//! `lemmate-desktop` — the Tauri 2 desktop shell (SPEC §3.1, §14).
//!
//! The shell is deliberately thin: it starts the engine's local relay for every vault the
//! account can read — one folder, sidecar and connection each, under the configured root — and
//! opens one window on the URL that relay serves. With no server configured it does the same
//! thing standalone (SPEC §3.2): the vaults are whatever folders are under the root, and there
//! is no connection to make. All of the application lives in
//! `lemmate-core` (sync, projection, search) and in the shared TypeScript UI; nothing is
//! exposed to the webview over Tauri IPC yet. The one thing the page asks of the shell — a note
//! moved out into a window of its own — it asks with a plain `window.open`, which
//! [`relay_window`] answers with another window on the same relay.
#![cfg_attr(all(not(debug_assertions), windows), windows_subsystem = "windows")]

mod config;

use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, bail};
use clap::Parser;
use lemmate_core::client::{self, LocalHandle, LocalOptions, SyncOptions};
use lemmate_core::local::ConnectRequest;
use lemmate_core::vaults;
use tauri::webview::NewWindowResponse;
use tauri::{Manager, RunEvent, Url, WebviewUrl, WebviewWindowBuilder, Wry};

const WINDOW_LABEL: &str = "main";
const WINDOW_SIZE: (f64, f64) = (1280.0, 840.0);
/// A note on its own has no sidebar to make room for.
const NOTE_WINDOW_SIZE: (f64, f64) = (960.0, 900.0);
/// A path the relay never serves: a note window navigating to it is asking to be closed.
const CLOSE_PATH: &str = "/.lemmate-shell/close-window";
/// How long the "connected" answer gets to reach the page before the restart takes the process.
const RESTART_GRACE: Duration = Duration::from_millis(500);

/// The running relay, kept in Tauri managed state so it outlives `setup` and can be
/// stopped on exit. `Option` because [`LocalHandle::abort`] is called exactly once.
struct Relay(Mutex<Option<LocalHandle>>);

impl Relay {
    /// Whether `url` is a page this relay serves: the only thing a window may be opened on.
    fn serves(&self, url: &Url) -> bool {
        self.0.lock().is_ok_and(|guard| guard.as_ref().is_some_and(|h| same_origin(h.addr, url)))
    }

    fn abort(&self) {
        if let Ok(mut guard) = self.0.lock()
            && let Some(handle) = guard.take()
        {
            handle.abort();
        }
    }
}

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()),
        )
        .init();

    let cli = config::Cli::parse();
    if let Some(ctx) = config::Config::needs_setup(&cli) {
        return match run_setup(ctx) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("error: {e:#}");
                ExitCode::FAILURE
            }
        };
    }
    let cfg = match config::Config::resolve(cli) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("{e:#}");
            return ExitCode::FAILURE;
        }
    };

    match run(cfg) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cfg: config::Config) -> anyhow::Result<()> {
    let app = tauri::Builder::default()
        .setup(move |app| {
            let relay = start_relay(app, &cfg)?;
            app.manage(relay);
            Ok(())
        })
        .build(tauri::generate_context!())
        .context("building the Tauri application")?;

    app.run(|app, event| {
        if let RunEvent::Exit = event
            && let Some(relay) = app.try_state::<Relay>()
        {
            relay.abort();
        }
    });
    Ok(())
}

/// First run: serve the UI in setup mode, wait for the form, write the config, sign in if
/// asked, then start the real relay and point the same window at it.
fn run_setup(ctx: config::SetupContext) -> anyhow::Result<()> {
    let app = tauri::Builder::default()
        .setup(move |app| {
            let web_dir = resolve_web_dir(app, ctx.web_dir.as_deref())?;
            let (addr, rx, setup_task) = tauri::async_runtime::block_on(lemmate_core::local::serve_setup(
                SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
                Some(web_dir.clone()),
                ctx.config_path.clone(),
                ctx.suggested_root_dir.clone(),
            ))
            .context("starting the setup server")?;
            let url: tauri::Url = format!("http://{addr}/").parse()?;
            tracing::info!(%url, "opening setup window");
            relay_window(app, WINDOW_LABEL, url)
                .title("Lemmate — setup")
                .inner_size(WINDOW_SIZE.0, WINDOW_SIZE.1)
                .build()
                .context("creating the setup window")?;
            app.manage(Relay(Mutex::new(None)));

            let handle = app.handle().clone();
            let config_path = ctx.config_path.clone();
            tauri::async_runtime::spawn(async move {
                let Ok(req) = rx.await else { return };
                let result: anyhow::Result<()> = async {
                    // Standalone setups name no server, so there is nothing to sign in to.
                    if let (Some(server), Some(email), Some(password)) =
                        (req.server_url.as_deref(), req.email.as_deref(), req.password.as_deref())
                        && !email.is_empty()
                    {
                        let ca = req.ca_cert.as_deref().filter(|c| !c.is_empty()).map(Path::new);
                        let device = std::fs::read_to_string("/etc/hostname")
                            .map(|s| s.trim().to_owned())
                            .unwrap_or_else(|_| "desktop".into());
                        lemmate_core::credentials::login(
                            server,
                            email,
                            password,
                            req.register,
                            req.invite.as_deref(),
                            ca,
                            &device,
                        )
                        .context("signing in")?;
                    }
                    config::Config::write_setup(&config_path, &req)?;
                    let cfg = config::Config::resolve(config::Cli::parse())
                        .context("re-reading the new configuration")?;
                    let mut relay = start_relay_for(&cfg, web_dir).await?;
                    watch_for_connect(handle.clone(), &mut relay, cfg.config_path.clone());
                    let url: tauri::Url = window_url(&relay).parse()?;
                    if let Some(w) = handle.get_webview_window(WINDOW_LABEL) {
                        w.navigate(url).context("navigating to the relay")?;
                        let _ = w.set_title("Lemmate");
                    }
                    if let Some(state) = handle.try_state::<Relay>()
                        && let Ok(mut guard) = state.0.lock()
                    {
                        *guard = Some(relay);
                    }
                    setup_task.abort();
                    Ok(())
                }
                .await;
                if let Err(e) = result {
                    tracing::error!(error = %format!("{e:#}"), "setup failed");
                    eprintln!("setup failed: {e:#}");
                }
            });
            Ok(())
        })
        .build(tauri::generate_context!())
        .context("building the Tauri application")?;
    app.run(|app, event| {
        if let RunEvent::Exit = event
            && let Some(relay) = app.try_state::<Relay>()
        {
            relay.abort();
        }
    });
    Ok(())
}

/// Start the local relay and open the window on it.
fn start_relay(app: &tauri::App, cfg: &config::Config) -> anyhow::Result<Relay> {
    let web_dir = resolve_web_dir(app, cfg.web_dir.as_deref())?;
    // `start_relay_for` binds the listener and returns once it is up, so the URL below is
    // serveable by the time the webview asks for it.
    let mut handle = tauri::async_runtime::block_on(start_relay_for(cfg, web_dir))?;
    watch_for_connect(app.handle().clone(), &mut handle, cfg.config_path.clone());

    let url = window_url(&handle);
    tracing::info!(%url, "opening main window");
    let url = url.parse().with_context(|| format!("relay URL {url} is not a valid URL"))?;

    relay_window(app, WINDOW_LABEL, url)
        .title("Lemmate")
        .inner_size(WINDOW_SIZE.0, WINDOW_SIZE.1)
        .build()
        .context("creating the main window")?;

    Ok(Relay(Mutex::new(Some(handle))))
}

/// A window on the relay's page, which may open more of them.
///
/// *Move to new window* on a tab is a `window.open` of the same page on the note's `#/w/…`
/// route. A webview left to itself does something different with that on every platform —
/// nothing at all on Linux and macOS — so a request for a page this relay serves gets a window
/// built here instead, like this one, and the request itself is refused. Anything else is left
/// to the platform, as it was before this handler existed.
fn relay_window<M: Manager<Wry>>(
    app: &M,
    label: impl Into<String>,
    url: Url,
) -> WebviewWindowBuilder<'_, Wry, M> {
    let handle = app.app_handle().clone();
    WebviewWindowBuilder::new(app, label, WebviewUrl::External(url))
        // The page does its own drag and drop — notes and folders in the tree, tabs between
        // panes, files onto the editor — and on Windows Tauri's file-drop handler swallows
        // every HTML5 drag. Nothing listens to that handler's events: there is no IPC.
        .disable_drag_drop_handler()
        .on_new_window(move |url, _features| {
            if !handle.try_state::<Relay>().is_some_and(|relay| relay.serves(&url)) {
                return NewWindowResponse::Allow;
            }
            // Built once this callback has returned rather than inside it: the callback runs on
            // the event loop, and building a window waits for that loop to answer.
            let handle = handle.clone();
            tauri::async_runtime::spawn(async move {
                if let Err(e) = open_note_window(&handle, url) {
                    tracing::warn!(error = %format!("{e:#}"), "could not open a note window");
                }
            });
            NewWindowResponse::Deny
        })
}

/// A note moved out of the main window. Its page names the note in the document title, which
/// is the only thing a window list or a task switcher has to tell several of them apart by.
///
/// The page closes the window when its last tab goes. `window.close()` cannot do that here: a
/// page may only close a window a script opened, this one was opened by the shell, and on Linux
/// wry answers it by destroying the webview and leaving an empty frame behind. So the shell
/// hands the page a `lemmateShell.closeWindow()` that navigates to [`CLOSE_PATH`], and the
/// navigation handler cancels that navigation and closes the window instead. Nothing crosses
/// Tauri IPC, which this shell still exposes none of.
fn open_note_window(app: &tauri::AppHandle, url: Url) -> anyhow::Result<()> {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let label = format!("note-{}", NEXT.fetch_add(1, Ordering::Relaxed));
    tracing::info!(%url, label, "opening a note window");
    let (handle, me) = (app.clone(), label.clone());
    relay_window(app, label, url)
        .title("Lemmate")
        .inner_size(NOTE_WINDOW_SIZE.0, NOTE_WINDOW_SIZE.1)
        .on_document_title_changed(|window, title| {
            let _ = window.set_title(&title);
        })
        .initialization_script(format!(
            "window.lemmateShell = {{ closeWindow: () => location.assign({CLOSE_PATH:?}) }}"
        ))
        .on_navigation(move |url| {
            if !(url.path() == CLOSE_PATH && handle.try_state::<Relay>().is_some_and(|r| r.serves(url))) {
                return true;
            }
            // Closed once this callback has returned, for the same reason `relay_window` builds
            // windows outside its own.
            let (handle, me) = (handle.clone(), me.clone());
            tauri::async_runtime::spawn(async move {
                if let Some(window) = handle.get_webview_window(&me) {
                    let _ = window.close();
                }
            });
            false
        })
        .build()
        .context("creating the window")?;
    Ok(())
}

/// Same scheme, host and port as the relay listening on `addr`.
fn same_origin(addr: SocketAddr, url: &Url) -> bool {
    Url::parse(&format!("http://{addr}/")).is_ok_and(|relay| relay.origin() == url.origin())
}

/// Where to point the window: at the workspace, or — when this shell holds exactly one vault —
/// straight at it, which is what a single-vault configuration means to ask for.
fn window_url(handle: &LocalHandle) -> String {
    match handle.vaults.len() {
        1 => format!("http://{}/#/v/{}", handle.addr, handle.vault_id),
        _ => format!("http://{}/", handle.addr),
    }
}

/// Work out which vaults to open, then start one engine per vault behind one relay.
async fn start_relay_for(cfg: &config::Config, web_dir: PathBuf) -> anyhow::Result<LocalHandle> {
    let token =
        cfg.token.clone().or_else(|| cfg.server_url.as_deref().and_then(lemmate_core::credentials::load));
    let sync = |vault_dir: PathBuf, vault_id| SyncOptions {
        vault_dir,
        server_url: cfg.server_url.clone(),
        vault_id,
        once: false,
        ca_cert: cfg.ca_cert.clone(),
        // Saved by `lemmate login`; a server with accounts refuses the sync without it.
        token: token.clone(),
    };
    let opts = match &cfg.layout {
        config::Layout::Single { dir, id } => vec![sync(dir.clone(), *id)],
        config::Layout::Root(root) => {
            std::fs::create_dir_all(root)
                .with_context(|| format!("creating the notes folder {}", root.display()))?;
            for (from, to) in vaults::rehome(root) {
                tracing::info!(from = %from.display(), to = %to.display(), "vault folder renamed");
            }
            // Best-effort: with no answer from the server — or no server at all — whatever is
            // already on disk opens. A vault that only exists there yet arrives on the next
            // launch.
            let remote = match &cfg.server_url {
                None => Vec::new(),
                Some(url) => match vaults::remote_ids(url, token.as_deref(), cfg.ca_cert.as_deref()) {
                    Ok(ids) => ids,
                    Err(e) => {
                        tracing::warn!(error = %e, "could not ask the server which vaults exist");
                        Vec::new()
                    }
                },
            };
            vaults::plan(root, &remote).into_iter().map(|f| sync(f.dir, f.id)).collect()
        }
    };
    tracing::info!(vaults = opts.len(), web = %web_dir.display(), "starting local relay");
    // Only a workspace can grow: "New vault" in the tree puts a folder under the root. A shell
    // pointed at one vault folder stays pointed at it.
    let vault_root = match &cfg.layout {
        config::Layout::Root(root) => Some(root.clone()),
        config::Layout::Single { .. } => None,
    };
    start_on_stable_port(opts, web_dir, cfg.layout.anchor(), vault_root, cfg.config_path.clone()).await
}

/// Answer the UI's "connect a server" requests for as long as this relay runs (SPEC §3.2).
///
/// Signing in and rewriting `desktop.toml` are the shell's job, not the engine's, and so is what
/// follows: the engines were built from the old configuration and there is no way to give them a
/// server in place, so the app restarts onto the new one. It comes back up with the same
/// arguments, finds the file it just wrote, and syncs the vaults it already has.
fn watch_for_connect(app: tauri::AppHandle, relay: &mut LocalHandle, config_path: Option<PathBuf>) {
    let (Some(mut rx), Some(config_path)) = (relay.connect.take(), config_path) else {
        tracing::debug!("no configuration file to write: the UI will not offer to connect a server");
        return;
    };
    tauri::async_runtime::spawn(async move {
        while let Some(ask) = rx.recv().await {
            let (path, request) = (config_path.clone(), ask.request.clone());
            let result = tauri::async_runtime::spawn_blocking(move || connect_server(&path, &request))
                .await
                .map_err(|e| format!("the connection attempt did not finish: {e}"))
                .and_then(|r| r.map_err(|e| format!("{e:#}")));
            match &result {
                Ok(()) => tracing::info!(server = %ask.request.server_url, "connected; restarting"),
                Err(e) => tracing::warn!(error = %e, "could not connect to the server"),
            }
            let restart = result.is_ok();
            let _ = ask.reply.send(result);
            if restart {
                // Give the answer time off the wire before the process goes away, so the page
                // that asked sees "connected" rather than a dropped connection. On the blocking
                // pool, since this crate does not depend on tokio directly.
                let _ = tauri::async_runtime::spawn_blocking(|| std::thread::sleep(RESTART_GRACE)).await;
                app.restart();
            }
        }
    });
}

/// Sign in if asked, prove the server answers, then write it into the configuration file.
///
/// Validating before writing is the point: a typo, an unreachable host, a private CA that is not
/// trusted, or a server with accounts and no session are all easy to explain in the dialog that
/// asked, and nearly impossible to explain after a restart that silently syncs nothing.
fn connect_server(config_path: &Path, req: &ConnectRequest) -> anyhow::Result<()> {
    let url = req.server_url.trim().trim_end_matches('/');
    let ca = req.ca_cert.as_deref().map(str::trim).filter(|c| !c.is_empty());
    if let (Some(email), Some(password)) = (req.email.as_deref(), req.password.as_deref())
        && !email.is_empty()
    {
        let device = std::fs::read_to_string("/etc/hostname")
            .map(|s| s.trim().to_owned())
            .unwrap_or_else(|_| "desktop".into());
        lemmate_core::credentials::login(
            url,
            email,
            password,
            req.register,
            req.invite.as_deref(),
            ca.map(Path::new),
            &device,
        )
        .context("signing in")?;
    }
    let token = lemmate_core::credentials::load(url);
    vaults::remote_ids(url, token.as_deref(), ca.map(Path::new))
        .context("asking the server which vaults this account can read")?;
    config::Config::set_server(config_path, url, ca)?;
    Ok(())
}

/// Bind the relay on the notes folder's stable port (see [`lemmate_core::local::stable_port`]),
/// and fall back to an ephemeral one if something already holds it.
///
/// The port is what keeps the webview's origin — and with it every `localStorage` key the UI
/// writes: open tabs and panes, pinned tabs, sidebar width, the file browser's folds — the same
/// from one launch to the next. Losing it costs the layout, not the notes, so a taken port is a
/// warning rather than a failure to start.
async fn start_on_stable_port(
    sync: Vec<SyncOptions>,
    web_dir: PathBuf,
    anchor: &Path,
    vault_root: Option<PathBuf>,
    config_path: Option<PathBuf>,
) -> anyhow::Result<LocalHandle> {
    let port = lemmate_core::local::stable_port(anchor);
    let opts = |port| LocalOptions {
        bind: SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
        web_dir: Some(web_dir.clone()),
        vault_root: vault_root.clone(),
        config_path: config_path.clone(),
    };
    match client::start_many(sync.clone(), opts(port)).await {
        Ok(handle) => Ok(handle),
        Err(e) => {
            tracing::warn!(port, error = %e, "stable port unavailable; the saved layout will not be restored");
            client::start_many(sync, opts(0)).await.context("starting the local sync relay")
        }
    }
}

/// Where the relay reads the built web assets from, in order of precedence:
///
/// 1. `--web-dir` / `LEMMATE_WEB_DIR` (or `web_dir` in the config file);
/// 2. `<resource dir>/ui/dist`, i.e. the copy bundled by `bundle.resources`, when present;
/// 3. `<repo>/ui/dist` relative to `CARGO_MANIFEST_DIR` — the dev-mode source tree.
fn resolve_web_dir(app: &tauri::App, override_dir: Option<&Path>) -> anyhow::Result<PathBuf> {
    if let Some(dir) = override_dir {
        if !dir.is_dir() {
            bail!("web assets directory {} does not exist", dir.display());
        }
        return Ok(dir.to_path_buf());
    }

    if let Ok(resources) = app.path().resource_dir() {
        let bundled = resources.join("ui").join("dist");
        if bundled.is_dir() {
            return Ok(bundled);
        }
    }

    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../ui/dist");
    if source.is_dir() {
        return Ok(std::fs::canonicalize(&source).unwrap_or(source));
    }

    bail!(
        "no web assets found: pass --web-dir (or set LEMMATE_WEB_DIR), or build the UI with \
         `npm install && npm run build` in {}",
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../ui").display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_relays_own_pages_get_a_window() {
        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, 4242));
        let url = |s: &str| Url::parse(s).unwrap();
        assert!(same_origin(addr, &url("http://127.0.0.1:4242/#/w/01J/01K")));
        assert!(same_origin(addr, &url("http://127.0.0.1:4242/")));
        assert!(!same_origin(addr, &url("http://127.0.0.1:4243/")));
        assert!(!same_origin(addr, &url("https://127.0.0.1:4242/")));
        assert!(!same_origin(addr, &url("http://localhost:4242/")));
        assert!(!same_origin(addr, &url("https://example.com/")));
    }

    #[test]
    fn the_close_path_survives_url_normalisation() {
        // `on_navigation` compares the parsed path, so a path a URL parser rewrites (a `.` or
        // `..` segment, say) would never match and no window would ever close.
        let url = Url::parse(&format!("http://127.0.0.1:4242{CLOSE_PATH}")).unwrap();
        assert_eq!(url.path(), CLOSE_PATH);
    }
}

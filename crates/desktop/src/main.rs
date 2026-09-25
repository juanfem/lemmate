//! `lemmate-desktop` — the Tauri 2 desktop shell (SPEC §3.1, §14).
//!
//! The shell is deliberately thin: it starts the engine's local relay for every vault the
//! account can read — one folder, sidecar and connection each, under the configured root — and
//! opens one window on the URL that relay serves. With no server configured it does the same
//! thing standalone (SPEC §3.2): the vaults are whatever folders are under the root, and there
//! is no connection to make. All of the application lives in
//! `lemmate-core` (sync, projection, search) and in the shared TypeScript UI; nothing is
//! exposed to the webview over Tauri IPC yet. The two things the page asks of the shell — open a
//! note in a window of its own, close that window again — it asks by navigating to a path the
//! relay never serves, which [`relay_window`] cancels and acts on instead (see [`ShellRequest`]).
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
/// Paths the relay never serves. A window navigating under here is asking the shell for something.
const SHELL_PREFIX: &str = "/.lemmate-shell/";
const CLOSE_PATH: &str = "/.lemmate-shell/close-window";
const OPEN_PATH: &str = "/.lemmate-shell/open-window";
const EXTERNAL_PATH: &str = "/.lemmate-shell/open-external";
const SIGN_IN_PATH: &str = "/.lemmate-shell/sign-in";
/// Where the server sends the sign-in window back to. Never loaded: the window's navigation
/// handler catches it first (`BrowserSignIn::is_callback`), so no port need be listening.
const SIGN_IN_CALLBACK: &str = "http://127.0.0.1/lemmate-callback";
const SIGN_IN_LABEL: &str = "sign-in";
/// What every relay window's page gets as `window.lemmateShell`: the two requests, as navigations.
const SHELL_SCRIPT: &str = r#"window.lemmateShell = {
  closeWindow: () => location.assign("/.lemmate-shell/close-window"),
  openWindow: (route, x, y) => {
    const q = new URLSearchParams({ route })
    if (Number.isFinite(x) && Number.isFinite(y)) q.set('x', String(Math.round(x))), q.set('y', String(Math.round(y)))
    location.assign("/.lemmate-shell/open-window?" + q)
  },
  openExternal: (url) => location.assign("/.lemmate-shell/open-external?" + new URLSearchParams({ url: new URL(url, location.href).href })),
  signIn: (server, ca) => location.assign("/.lemmate-shell/sign-in?" + new URLSearchParams({ server, ca: ca || "" })),
}"#;
/// How long the "connected" answer gets to reach the page before the restart takes the process.
const RESTART_GRACE: Duration = Duration::from_millis(500);

/// The running relay, kept in Tauri managed state so it outlives `setup` and can be
/// stopped on exit. `Option` because [`LocalHandle::abort`] is called exactly once.
struct Relay(Mutex<Option<LocalHandle>>);

/// The server the relay syncs with, if any: the one place besides the relay's own pages that a
/// page may ask to open in the system browser — its account settings, a note shared with you.
struct ServerOrigin(Mutex<Option<Url>>);

impl ServerOrigin {
    fn set(app: &tauri::AppHandle, cfg: &config::Config) {
        let url = cfg.server_url.as_deref().and_then(|u| Url::parse(u).ok());
        if let Some(state) = app.try_state::<ServerOrigin>()
            && let Ok(mut guard) = state.0.lock()
        {
            *guard = url;
        }
    }

    fn allows(app: &tauri::AppHandle, url: &Url) -> bool {
        app.try_state::<ServerOrigin>().is_some_and(|s| {
            s.0.lock().is_ok_and(|g| g.as_ref().is_some_and(|server| server.origin() == url.origin()))
        })
    }
}

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
            app.manage(ServerOrigin(Mutex::new(None)));
            ServerOrigin::set(app.handle(), &cfg);
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
            relay_window(app, WINDOW_LABEL.into(), url)
                .title("Lemmate — setup")
                .inner_size(WINDOW_SIZE.0, WINDOW_SIZE.1)
                .build()
                .context("creating the setup window")?;
            app.manage(Relay(Mutex::new(None)));
            app.manage(ServerOrigin(Mutex::new(None)));

            let handle = app.handle().clone();
            let config_path = ctx.config_path.clone();
            tauri::async_runtime::spawn(async move {
                let Ok(req) = rx.await else { return };
                let result: anyhow::Result<()> = async {
                    // Standalone setups name no server, so there is nothing to sign in to.
                    if let Some(server) = req.server_url.as_deref().filter(|s| !s.is_empty()) {
                        let ca = req.ca_cert.as_deref().filter(|c| !c.is_empty()).map(Path::new);
                        sign_in(
                            server,
                            req.email.as_deref(),
                            req.password.as_deref(),
                            req.token.as_deref(),
                            req.register,
                            req.invite.as_deref(),
                            ca,
                        )?;
                    }
                    config::Config::write_setup(&config_path, &req)?;
                    let cfg = config::Config::resolve(config::Cli::parse())
                        .context("re-reading the new configuration")?;
                    let mut relay = start_relay_for(&cfg, web_dir).await?;
                    watch_for_connect(handle.clone(), &mut relay, cfg.config_path.clone());
                    watch_for_sign_out(handle.clone(), &mut relay, &cfg);
                    ServerOrigin::set(&handle, &cfg);
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
    watch_for_sign_out(app.handle().clone(), &mut handle, cfg);

    let url = window_url(&handle);
    tracing::info!(%url, "opening main window");
    let url = url.parse().with_context(|| format!("relay URL {url} is not a valid URL"))?;

    relay_window(app, WINDOW_LABEL.into(), url)
        .title("Lemmate")
        .inner_size(WINDOW_SIZE.0, WINDOW_SIZE.1)
        .build()
        .context("creating the main window")?;

    Ok(Relay(Mutex::new(Some(handle))))
}

/// A window on the relay's page, which may open more of them and close itself.
///
/// *Move to new window* on a tab, or a tab dragged out of every window, asks for a window on the
/// note's `#/w/…` route. `window.open` is not enough for that: WebKit only lets a page open one
/// in answer to a click or a key, and the end of a drag is neither. Closing is the same story —
/// a page may only close a window a script opened, and on Linux wry answers `window.close()` by
/// destroying the webview and leaving an empty frame behind. So the page gets
/// [`SHELL_SCRIPT`], which turns both requests into navigations under [`SHELL_PREFIX`], and the
/// navigation handler here cancels those and does what they ask. Nothing crosses Tauri IPC.
///
/// A plain `window.open` of a page this relay serves still gets a window built here too, since a
/// webview left to itself does something different with it on every platform — nothing at all on
/// Linux and macOS. Anything else is left to the platform, as it was before this handler existed.
fn relay_window<M: Manager<Wry>>(app: &M, label: String, url: Url) -> WebviewWindowBuilder<'_, Wry, M> {
    let (opener, navigator, me) = (app.app_handle().clone(), app.app_handle().clone(), label.clone());
    WebviewWindowBuilder::new(app, label, WebviewUrl::External(url))
        // The page does its own drag and drop — notes and folders in the tree, tabs between
        // panes, files onto the editor — and on Windows Tauri's file-drop handler swallows
        // every HTML5 drag. Nothing listens to that handler's events: there is no IPC.
        .disable_drag_drop_handler()
        .initialization_script(SHELL_SCRIPT)
        .on_navigation(move |url| {
            if !url.path().starts_with(SHELL_PREFIX) {
                return true;
            }
            // Signing in is asked for by the setup page too, which no relay serves yet: any page
            // of this machine's may ask, and the window it opens only ever shows the server.
            if let Some(ShellRequest::SignIn { server, ca_cert }) = ShellRequest::parse(url)
                && is_loopback(url)
            {
                let (handle, me) = (navigator.clone(), me.clone());
                tauri::async_runtime::spawn(async move { sign_in_window(&handle, &me, &server, ca_cert) });
                return false;
            }
            let served = || navigator.try_state::<Relay>().is_some_and(|r| r.serves(url));
            if !served() {
                return true;
            }
            // Acted on once this callback has returned rather than inside it: the callback runs
            // on the event loop, and building or closing a window waits for that loop to answer.
            let (handle, me, request) = (navigator.clone(), me.clone(), ShellRequest::parse(url));
            tauri::async_runtime::spawn(async move {
                match request {
                    // The main window is the app: it closes when the user says so.
                    Some(ShellRequest::Close) if me != WINDOW_LABEL => {
                        if let Some(window) = handle.get_webview_window(&me) {
                            let _ = window.close();
                        }
                    }
                    Some(ShellRequest::Open { url, position }) => open_note_window(&handle, url, position),
                    // Only a page this relay serves, or of the server it syncs with: the page may
                    // send the browser there, and nowhere else, whatever a script in it tried.
                    Some(ShellRequest::External(url))
                        if handle.try_state::<Relay>().is_some_and(|r| r.serves(&url))
                            || ServerOrigin::allows(&handle, &url) =>
                    {
                        open_in_browser(&url)
                    }
                    _ => tracing::debug!(window = me, "ignored a shell request"),
                }
            });
            false
        })
        .on_new_window(move |url, features| {
            if !opener.try_state::<Relay>().is_some_and(|relay| relay.serves(&url)) {
                return NewWindowResponse::Allow;
            }
            let (handle, position) = (opener.clone(), features.position().map(|p| (p.x, p.y)));
            tauri::async_runtime::spawn(async move { open_note_window(&handle, url, position) });
            NewWindowResponse::Deny
        })
}

/// What a navigation under [`SHELL_PREFIX`] asks for.
#[derive(Debug, PartialEq)]
enum ShellRequest {
    Close,
    /// `url` in the system's default browser — a render, to present it.
    External(Url),
    /// A note window on `url`, at a screen position (logical pixels, top left) if the page knew one.
    Open {
        url: Url,
        position: Option<(f64, f64)>,
    },
    /// Sign in to `server` through its web page (`sign_in_window`).
    SignIn {
        server: String,
        ca_cert: Option<PathBuf>,
    },
}

impl ShellRequest {
    /// `None` for anything but the two requests, including an open of a route that is not a note
    /// window's — the page is ours, but nothing it navigates to should become a window by accident.
    fn parse(url: &Url) -> Option<Self> {
        match url.path() {
            CLOSE_PATH => Some(Self::Close),
            OPEN_PATH => {
                let arg =
                    |name: &str| url.query_pairs().find(|(k, _)| k == name).map(|(_, v)| v.into_owned());
                let route = arg("route").filter(|r| r.starts_with("#/w/"))?;
                let mut target = url.clone();
                target.set_path("/");
                target.set_query(None);
                target.set_fragment(Some(&route[1..]));
                let coord = |name| arg(name).and_then(|v| v.parse::<f64>().ok()).filter(|v| v.is_finite());
                let position = coord("x").zip(coord("y"));
                Some(Self::Open { url: target, position })
            }
            SIGN_IN_PATH => {
                let arg =
                    |name: &str| url.query_pairs().find(|(k, _)| k == name).map(|(_, v)| v.into_owned());
                let server = arg("server").filter(|s| {
                    Url::parse(s).is_ok_and(|u| matches!(u.scheme(), "http" | "https") && u.host().is_some())
                })?;
                let ca_cert =
                    arg("ca").map(|c| c.trim().to_owned()).filter(|c| !c.is_empty()).map(PathBuf::from);
                Some(Self::SignIn { server, ca_cert })
            }
            EXTERNAL_PATH => {
                let target = url.query_pairs().find(|(k, _)| k == "url").map(|(_, v)| v.into_owned())?;
                let target = Url::parse(&target).ok().filter(|u| matches!(u.scheme(), "http" | "https"))?;
                Some(Self::External(target))
            }
            _ => None,
        }
    }
}

/// Hand `url` to the system's default browser, the way each platform opens a link from another
/// program. Nothing waits for it: the browser outlives the request, and may already be running.
fn open_in_browser(url: &Url) {
    #[cfg(target_os = "macos")]
    let mut command = std::process::Command::new("open");
    #[cfg(target_os = "windows")]
    let mut command = std::process::Command::new("explorer");
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let mut command = std::process::Command::new("xdg-open");
    match command.arg(url.as_str()).spawn() {
        Ok(_) => tracing::info!(%url, "opened in the default browser"),
        Err(e) => tracing::warn!(%url, error = %e, "could not open the default browser"),
    }
}

/// A note moved out of the main window. Its page names the note in the document title, which
/// is the only thing a window list or a task switcher has to tell several of them apart by; the
/// page also closes the window when its last tab goes (see [`relay_window`]).
fn open_note_window(app: &tauri::AppHandle, url: Url, position: Option<(f64, f64)>) {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let label = format!("note-{}", NEXT.fetch_add(1, Ordering::Relaxed));
    tracing::info!(%url, label, ?position, "opening a note window");
    let mut builder = relay_window(app, label, url)
        .title("Lemmate")
        .inner_size(NOTE_WINDOW_SIZE.0, NOTE_WINDOW_SIZE.1)
        .on_document_title_changed(|window, title| {
            let _ = window.set_title(&title);
        });
    if let Some((x, y)) = position {
        builder = builder.position(x, y);
    }
    if let Err(e) = builder.build() {
        tracing::warn!(error = %e, "could not open a note window");
    }
}

/// A page served from this machine: the relay, or the setup server before there is one.
fn is_loopback(url: &Url) -> bool {
    url.scheme() == "http" && matches!(url.host_str(), Some("127.0.0.1" | "[::1]" | "localhost"))
}

/// Sign in to `server` in a window of its own (the server's `apps.rs`): its web page signs in
/// however the server does — straight through the identity provider, if that is the only way —
/// and asks to allow this app; the answer is a redirect to [`SIGN_IN_CALLBACK`], caught here
/// before it loads, and traded for an access token that is saved like `lemmate login` saves one.
///
/// The page that asked hears how it went as a `lemmate-sign-in` event on its window, with
/// `detail` `{ ok: true, email }` or `{ ok: false, error }` — closing the window counts as a
/// cancel. Nothing crosses Tauri IPC: the answer is a script evaluated in that page.
fn sign_in_window(app: &tauri::AppHandle, asker: &str, server: &str, ca_cert: Option<PathBuf>) {
    let report = {
        let (app, asker) = (app.clone(), asker.to_owned());
        move |result: Result<String, String>| {
            let detail = match result {
                Ok(email) => serde_json::json!({ "ok": true, "email": email }),
                Err(error) => serde_json::json!({ "ok": false, "error": error }),
            };
            let js =
                format!("window.dispatchEvent(new CustomEvent('lemmate-sign-in', {{ detail: {detail} }}))");
            if let Some(w) = app.get_webview_window(&asker) {
                let _ = w.eval(&js);
            }
        }
    };
    let device = format!("Lemmate desktop on {}", lemmate_core::credentials::hostname());
    let flow = match lemmate_core::credentials::BrowserSignIn::start(server, SIGN_IN_CALLBACK, &device) {
        Ok(f) => f,
        Err(e) => return report(Err(e.to_string())),
    };
    let url: Url = match flow.url.parse() {
        Ok(u) => u,
        Err(e) => return report(Err(format!("{e}"))),
    };
    // One sign-in at a time: a second request replaces the first.
    if let Some(old) = app.get_webview_window(SIGN_IN_LABEL) {
        let _ = old.destroy();
    }
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (on_nav_done, on_close_done) = (done.clone(), done.clone());
    let (on_nav_report, on_close_report) = (report.clone(), report);
    let nav_app = app.clone();
    let built = WebviewWindowBuilder::new(app, SIGN_IN_LABEL, WebviewUrl::External(url))
        .title(format!("Sign in to {}", lemmate_core::credentials::key(server)))
        .inner_size(520.0, 720.0)
        .on_navigation(move |url| {
            if !flow.is_callback(url.as_str()) {
                return true;
            }
            on_nav_done.store(true, Ordering::SeqCst);
            let (flow, query, ca, report, app) = (
                flow.clone(),
                url.query().unwrap_or("").to_owned(),
                ca_cert.clone(),
                on_nav_report.clone(),
                nav_app.clone(),
            );
            tauri::async_runtime::spawn(async move {
                if let Some(w) = app.get_webview_window(SIGN_IN_LABEL) {
                    let _ = w.close();
                }
                let result = tauri::async_runtime::spawn_blocking(move || flow.finish(&query, ca.as_deref()))
                    .await
                    .map_err(|e| e.to_string())
                    .and_then(|r| r.map_err(|e| e.to_string()));
                report(result);
            });
            false
        })
        .build();
    match built {
        Ok(window) => window.on_window_event(move |event| {
            if matches!(event, tauri::WindowEvent::Destroyed) && !on_close_done.swap(true, Ordering::SeqCst) {
                on_close_report(Err("sign-in was cancelled".into()));
            }
        }),
        Err(e) => {
            done.store(true, Ordering::SeqCst);
            tracing::warn!(error = %e, "could not open the sign-in window");
        }
    }
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

/// Answer the UI's "sign out" for as long as this relay runs: revoke the token on the server,
/// forget it here — keychain, credentials file, and a `token` written into `desktop.toml` — and
/// restart. The app comes back signed out: the relay answers `/api/v1/auth/me` with 401 and the
/// UI offers to sign in again. The notes stay where they are; they are the user's files.
///
/// A token given by `--token` or `LEMMATE_TOKEN` is not the shell's to forget, and comes back
/// with the next start.
fn watch_for_sign_out(app: tauri::AppHandle, relay: &mut LocalHandle, cfg: &config::Config) {
    let (Some(mut rx), Some(server)) = (relay.sign_out.take(), cfg.server_url.clone()) else { return };
    let (ca, config_path) = (cfg.ca_cert.clone(), cfg.config_path.clone());
    tauri::async_runtime::spawn(async move {
        while let Some(ask) = rx.recv().await {
            let (server, ca, path) = (server.clone(), ca.clone(), config_path.clone());
            let result = tauri::async_runtime::spawn_blocking(move || -> anyhow::Result<bool> {
                let revoked = lemmate_core::credentials::sign_out(&server, ca.as_deref())?;
                if let Some(p) = &path {
                    config::Config::clear_token(p)?;
                }
                Ok(revoked)
            })
            .await
            .map_err(|e| format!("signing out did not finish: {e}"))
            .and_then(|r| r.map_err(|e| format!("{e:#}")));
            match &result {
                Ok(revoked) => tracing::info!(revoked, "signed out; restarting"),
                Err(e) => tracing::warn!(error = %e, "could not sign out"),
            }
            let restart = result.is_ok();
            let _ = ask.reply.send(result.map(|_| ()));
            if restart {
                let _ = tauri::async_runtime::spawn_blocking(|| std::thread::sleep(RESTART_GRACE)).await;
                app.restart();
            }
        }
    });
}

/// Save a session for `server` from whatever the dialog was given: a pasted access token, or an
/// email and password (signing in, or registering). Given neither, nothing happens — a server
/// with `--no-auth`, or a token saved earlier by `lemmate login`.
fn sign_in(
    server: &str,
    email: Option<&str>,
    password: Option<&str>,
    token: Option<&str>,
    register: bool,
    invite: Option<&str>,
    ca: Option<&Path>,
) -> anyhow::Result<()> {
    if let Some(token) = token.map(str::trim).filter(|t| !t.is_empty()) {
        lemmate_core::credentials::login_with_token(server, token, ca)
            .context("checking the access token")?;
        return Ok(());
    }
    if let (Some(email), Some(password)) = (email, password)
        && !email.is_empty()
    {
        let device = std::fs::read_to_string("/etc/hostname")
            .map(|s| s.trim().to_owned())
            .unwrap_or_else(|_| "desktop".into());
        lemmate_core::credentials::login(server, email, password, register, invite, ca, &device)
            .context("signing in")?;
    }
    Ok(())
}

/// Sign in if asked, prove the server answers, then write it into the configuration file.
///
/// Validating before writing is the point: a typo, an unreachable host, a private CA that is not
/// trusted, or a server with accounts and no session are all easy to explain in the dialog that
/// asked, and nearly impossible to explain after a restart that silently syncs nothing.
fn connect_server(config_path: &Path, req: &ConnectRequest) -> anyhow::Result<()> {
    let url = req.server_url.trim().trim_end_matches('/');
    let ca = req.ca_cert.as_deref().map(str::trim).filter(|c| !c.is_empty());
    sign_in(
        url,
        req.email.as_deref(),
        req.password.as_deref(),
        req.token.as_deref(),
        req.register,
        req.invite.as_deref(),
        ca.map(Path::new),
    )?;
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
    fn shell_requests_are_parsed_from_the_navigation() {
        let url = |s: &str| Url::parse(&format!("http://127.0.0.1:4242{s}")).unwrap();
        // The paths survive URL normalisation (no `.` or `..` segment gets rewritten away), or no
        // request would ever match.
        assert!(CLOSE_PATH.starts_with(SHELL_PREFIX) && OPEN_PATH.starts_with(SHELL_PREFIX));
        // The page's script spells the same paths out.
        assert!(SHELL_SCRIPT.contains(CLOSE_PATH) && SHELL_SCRIPT.contains(OPEN_PATH));
        assert!(EXTERNAL_PATH.starts_with(SHELL_PREFIX) && SHELL_SCRIPT.contains(EXTERNAL_PATH));
        let render = "http://127.0.0.1:4242/api/v1/vaults/01J/notes/01K/render/01M?format=revealjs";
        let mut ask = url(EXTERNAL_PATH);
        ask.query_pairs_mut().append_pair("url", render);
        assert_eq!(ShellRequest::parse(&ask), Some(ShellRequest::External(Url::parse(render).unwrap())));
        assert_eq!(
            ShellRequest::parse(&url(&format!("{EXTERNAL_PATH}?url=file%3A%2F%2F%2Fetc%2Fpasswd"))),
            None
        );
        assert_eq!(ShellRequest::parse(&url(EXTERNAL_PATH)), None);
        assert_eq!(ShellRequest::parse(&url(CLOSE_PATH)), Some(ShellRequest::Close));
        assert_eq!(
            ShellRequest::parse(&url(&format!("{OPEN_PATH}?route=%23%2Fw%2F01J%2F01K&x=120&y=-40"))),
            Some(ShellRequest::Open { url: url("/#/w/01J/01K"), position: Some((120.0, -40.0)) })
        );
        assert_eq!(
            ShellRequest::parse(&url(&format!("{OPEN_PATH}?route=%23%2Fw%2F01J%2F01K&x=NaN"))),
            Some(ShellRequest::Open { url: url("/#/w/01J/01K"), position: None })
        );
        // Only a note window's route, and nothing outside the two paths.
        assert!(SIGN_IN_PATH.starts_with(SHELL_PREFIX) && SHELL_SCRIPT.contains(SIGN_IN_PATH));
        assert_eq!(
            ShellRequest::parse(&url(&format!("{SIGN_IN_PATH}?server=https%3A%2F%2Fnotes.example.org&ca="))),
            Some(ShellRequest::SignIn { server: "https://notes.example.org".into(), ca_cert: None })
        );
        assert_eq!(ShellRequest::parse(&url(&format!("{SIGN_IN_PATH}?server=file%3A%2F%2F%2Fetc"))), None);
        assert_eq!(ShellRequest::parse(&url(SIGN_IN_PATH)), None);
        assert!(is_loopback(&url("/")) && !is_loopback(&Url::parse("https://notes.example.org/").unwrap()));
        assert_eq!(ShellRequest::parse(&url(&format!("{OPEN_PATH}?route=%23%2Fv%2F01J"))), None);
        assert_eq!(ShellRequest::parse(&url(OPEN_PATH)), None);
        assert_eq!(ShellRequest::parse(&url("/.lemmate-shell/other")), None);
        assert_eq!(ShellRequest::parse(&url("/")), None);
    }
}

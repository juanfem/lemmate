//! `lemmate-desktop` — the Tauri 2 desktop shell (SPEC §3.1, §14).
//!
//! The shell is deliberately thin: it starts the engine's local relay for every vault the
//! account can read — one folder, sidecar and connection each, under the configured root — and
//! opens one window on the URL that relay serves. All of the application lives in
//! `lemmate-core` (sync, projection, search) and in the shared TypeScript UI; nothing is
//! exposed to the webview over Tauri IPC yet.
#![cfg_attr(all(not(debug_assertions), windows), windows_subsystem = "windows")]

mod config;

use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Mutex;

use anyhow::{Context, bail};
use clap::Parser;
use lemmate_core::client::{self, LocalHandle, LocalOptions, SyncOptions};
use lemmate_core::vaults;
use tauri::{Manager, RunEvent, WebviewUrl, WebviewWindowBuilder};

const WINDOW_LABEL: &str = "main";
const WINDOW_SIZE: (f64, f64) = (1280.0, 840.0);

/// The running relay, kept in Tauri managed state so it outlives `setup` and can be
/// stopped on exit. `Option` because [`LocalHandle::abort`] is called exactly once.
struct Relay(Mutex<Option<LocalHandle>>);

impl Relay {
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
            WebviewWindowBuilder::new(app, WINDOW_LABEL, WebviewUrl::External(url))
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
                    if let (Some(email), Some(password)) = (req.email.as_deref(), req.password.as_deref())
                        && !email.is_empty()
                    {
                        let ca = req.ca_cert.as_deref().filter(|c| !c.is_empty()).map(Path::new);
                        let device = std::fs::read_to_string("/etc/hostname")
                            .map(|s| s.trim().to_owned())
                            .unwrap_or_else(|_| "desktop".into());
                        lemmate_core::credentials::login(
                            &req.server_url,
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
                    let relay = start_relay_for(&cfg, web_dir).await?;
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
    let handle = tauri::async_runtime::block_on(start_relay_for(cfg, web_dir))?;

    let url = window_url(&handle);
    tracing::info!(%url, "opening main window");
    let url = url.parse().with_context(|| format!("relay URL {url} is not a valid URL"))?;

    WebviewWindowBuilder::new(app, WINDOW_LABEL, WebviewUrl::External(url))
        .title("Lemmate")
        .inner_size(WINDOW_SIZE.0, WINDOW_SIZE.1)
        .build()
        .context("creating the main window")?;

    Ok(Relay(Mutex::new(Some(handle))))
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
    let token = cfg.token.clone().or_else(|| lemmate_core::credentials::load(&cfg.server_url));
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
            // Best-effort: with no answer from the server, whatever is already on disk opens.
            // A vault that only exists there yet arrives on the next launch.
            let remote = match vaults::remote_ids(&cfg.server_url, token.as_deref(), cfg.ca_cert.as_deref()) {
                Ok(ids) => ids,
                Err(e) => {
                    tracing::warn!(error = %e, "could not ask the server which vaults exist");
                    Vec::new()
                }
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
    start_on_stable_port(opts, web_dir, cfg.layout.anchor(), vault_root).await
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
) -> anyhow::Result<LocalHandle> {
    let port = lemmate_core::local::stable_port(anchor);
    let opts = |port| LocalOptions {
        bind: SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
        web_dir: Some(web_dir.clone()),
        vault_root: vault_root.clone(),
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

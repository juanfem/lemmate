//! `notes-desktop` — the Tauri 2 desktop shell (SPEC §3.1, §14).
//!
//! The shell is deliberately thin: it starts the engine's local relay for the configured
//! vault and opens one window on the URL that relay serves. All of the application lives
//! in `notes-core` (sync, projection, search) and in the shared TypeScript UI; nothing is
//! exposed to the webview over Tauri IPC yet.
#![cfg_attr(all(not(debug_assertions), windows), windows_subsystem = "windows")]

mod config;

use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Mutex;

use anyhow::{Context, bail};
use clap::Parser;
use notes_core::client::{self, LocalHandle, LocalOptions, SyncOptions};
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

    let cfg = match config::Config::resolve(config::Cli::parse()) {
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

/// Start the local relay and open the window on it.
fn start_relay(app: &tauri::App, cfg: &config::Config) -> anyhow::Result<Relay> {
    let web_dir = resolve_web_dir(app, cfg.web_dir.as_deref())?;
    tracing::info!(vault = %cfg.vault_dir.display(), web = %web_dir.display(), "starting local relay");

    let sync = SyncOptions {
        vault_dir: cfg.vault_dir.clone(),
        server_url: cfg.server_url.clone(),
        vault_id: cfg.vault_id,
        once: false,
        ca_cert: cfg.ca_cert.clone(),
    };
    let local = LocalOptions { bind: SocketAddr::from((Ipv4Addr::LOCALHOST, 0)), web_dir: Some(web_dir) };

    // `client::start` binds the listener and returns once it is up, so the URL below is
    // serveable by the time the webview asks for it.
    let handle = tauri::async_runtime::block_on(client::start(sync, local))
        .context("starting the local sync relay")?;

    let url = format!("http://{}/#/v/{}", handle.addr, handle.vault_id);
    tracing::info!(%url, "opening main window");
    let url = url.parse().with_context(|| format!("relay URL {url} is not a valid URL"))?;

    WebviewWindowBuilder::new(app, WINDOW_LABEL, WebviewUrl::External(url))
        .title("notes")
        .inner_size(WINDOW_SIZE.0, WINDOW_SIZE.1)
        .build()
        .context("creating the main window")?;

    Ok(Relay(Mutex::new(Some(handle))))
}

/// Where the relay reads the built web assets from, in order of precedence:
///
/// 1. `--web-dir` / `NOTES_WEB_DIR` (or `web_dir` in the config file);
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
        "no web assets found: pass --web-dir (or set NOTES_WEB_DIR), or build the UI with \
         `npm install && npm run build` in {}",
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../ui").display()
    )
}

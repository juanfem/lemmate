//! `lemmate-mobile` — the Tauri 2 shell for Android and iOS (SPEC §3.1, §14, milestone M3).
//!
//! The same shape as `lemmate-desktop`: the engine's local relay runs in-process and the
//! webview is pointed at `http://127.0.0.1:<port>`, so the shared TypeScript UI is the same
//! bundle the server serves and nothing crosses Tauri IPC. What differs is everything the
//! platform decides for you rather than the user:
//!
//! * **The vault path is not a choice.** It is `<app data>/vault`; the setup form's folder
//!   field is ignored here (see [`config`]). Exposing that folder to the rest of the phone —
//!   the Storage Access Framework on Android, the Files app on iOS (SPEC §6.3) — is separate
//!   work and not done yet.
//! * **The web assets are compiled in.** The desktop ships `ui/dist` through
//!   `bundle.resources` and lets the relay read it off disk, but an APK's resources are not
//!   plain files a `ServeDir` can open, so they are embedded and unpacked once into the app's
//!   data directory. See [`web::unpack`].
//! * **The port must be stable.** The webview's origin is the relay's address, and a browser
//!   partitions `localStorage` by origin, so an ephemeral port would throw the layout away on
//!   every launch — and a phone relaunches the app constantly. See
//!   [`lemmate_core::local::stable_port`].

mod config;
mod web;

use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Mutex;

use anyhow::{Context, Result};
use lemmate_core::client::{self, LocalHandle, LocalOptions, SyncOptions};
use tauri::{Manager, RunEvent, WebviewUrl, WebviewWindowBuilder};

use config::{Config, Paths};

const WINDOW_LABEL: &str = "main";

/// The running relay, kept in Tauri managed state so it outlives `setup` and can be stopped
/// when the app exits.
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

/// The entry point Android's `MainActivity` and iOS's `main` call into.
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let app = tauri::Builder::default()
        .setup(|app| {
            // A phone has nowhere to print a startup error to, so a failure here has to reach
            // the webview rather than stderr. Until there is a screen for it, the log is it.
            if let Err(e) = boot(app) {
                tracing::error!(error = %format!("{e:#}"), "could not start");
                return Err(e.into());
            }
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("building the Tauri application");

    app.run(|app, event| {
        if let RunEvent::Exit = event
            && let Some(relay) = app.try_state::<Relay>()
        {
            relay.abort();
        }
    });
}

/// Unpack the assets, then either open the setup screen or start the relay.
fn boot(app: &tauri::App) -> Result<()> {
    let data_dir = app.path().app_data_dir().context("no app data directory")?;
    std::fs::create_dir_all(&data_dir).with_context(|| format!("creating {}", data_dir.display()))?;
    let paths = Paths::under(&data_dir);
    let web_dir = web::unpack(&paths.web).context("unpacking the web client")?;

    match Config::load(&paths) {
        Some(cfg) => {
            let handle = start_relay(&cfg, web_dir)?;
            let url = format!("http://{}/#/v/{}", handle.addr, handle.vault_id);
            open_window(app, &url)?;
            app.manage(Relay(Mutex::new(Some(handle))));
        }
        None => start_setup(app, paths, web_dir)?,
    }
    Ok(())
}

/// Bind the relay on the vault's stable port, falling back to an ephemeral one when something
/// already holds it — a forgotten layout beats an app that will not open.
fn start_relay(cfg: &Config, web_dir: PathBuf) -> Result<LocalHandle> {
    let sync = SyncOptions {
        vault_dir: cfg.vault_dir.clone(),
        server_url: cfg.server_url.clone(),
        vault_id: cfg.vault_id,
        once: false,
        ca_cert: cfg.ca_cert.clone(),
        token: cfg.token.clone().or_else(|| lemmate_core::credentials::load(&cfg.server_url)),
    };
    let port = lemmate_core::local::stable_port(&cfg.vault_dir);
    let opts = |port| LocalOptions {
        bind: SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
        web_dir: Some(web_dir.clone()),
        // The phone holds one vault (SPEC §14), so there is no root to create another under.
        vault_root: None,
    };
    tauri::async_runtime::block_on(async {
        match client::start(sync.clone(), opts(port)).await {
            Ok(handle) => Ok(handle),
            Err(e) => {
                tracing::warn!(port, error = %e, "stable port unavailable; the saved layout will not be restored");
                client::start(sync, opts(0)).await.context("starting the local sync relay")
            }
        }
    })
}

/// First run: serve the UI in setup mode, wait for the form, write the config, sign in if it
/// asked to, then start the real relay and point the same webview at it.
fn start_setup(app: &tauri::App, paths: Paths, web_dir: PathBuf) -> Result<()> {
    let (addr, rx, setup_task) = tauri::async_runtime::block_on(lemmate_core::local::serve_setup(
        SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
        Some(web_dir.clone()),
        paths.config.clone(),
        paths.vault_dir.clone(),
    ))
    .context("starting the setup server")?;
    open_window(app, &format!("http://{addr}/"))?;
    app.manage(Relay(Mutex::new(None)));

    let handle = app.handle().clone();
    tauri::async_runtime::spawn(async move {
        let Ok(req) = rx.await else { return };
        let result: Result<()> = (|| {
            if let (Some(email), Some(password)) = (req.email.as_deref(), req.password.as_deref())
                && !email.is_empty()
            {
                let ca = req.ca_cert.as_deref().filter(|c| !c.is_empty()).map(std::path::Path::new);
                lemmate_core::credentials::login(
                    &req.server_url,
                    email,
                    password,
                    req.register,
                    req.invite.as_deref(),
                    ca,
                    "phone",
                )
                .context("signing in")?;
            }
            Config::write_setup(&paths.config, &req)?;
            let cfg = Config::load(&paths).context("re-reading the new configuration")?;
            let relay = start_relay(&cfg, web_dir)?;
            let url = format!("http://{}/#/v/{}", relay.addr, relay.vault_id);
            if let Some(w) = handle.get_webview_window(WINDOW_LABEL) {
                w.navigate(url.parse()?).context("navigating to the relay")?;
            }
            if let Some(state) = handle.try_state::<Relay>()
                && let Ok(mut guard) = state.0.lock()
            {
                *guard = Some(relay);
            }
            setup_task.abort();
            Ok(())
        })();
        if let Err(e) = result {
            tracing::error!(error = %format!("{e:#}"), "setup failed");
        }
    });
    Ok(())
}

/// One webview on the given URL. Size and title are the platform's business on a phone; the
/// builder takes them anyway on desktop, where this same code runs under `cargo run`.
fn open_window(app: &tauri::App, url: &str) -> Result<()> {
    tracing::info!(%url, "opening the main webview");
    let url: tauri::Url = url.parse().with_context(|| format!("relay URL {url} is not a URL"))?;
    WebviewWindowBuilder::new(app, WINDOW_LABEL, WebviewUrl::External(url))
        .title("Lemmate")
        .build()
        .context("creating the main webview")?;
    Ok(())
}

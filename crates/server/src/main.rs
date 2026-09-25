//! lemmate-server — sync relay, persistence, and REST API (SPEC §3.1, §7, §13).

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::Context;
use clap::Parser;
use lemmate_core::RetentionPolicy;
use lemmate_core::Store;
use lemmate_server::oidc::OidcConfig;
use lemmate_server::{AuthMode, ServerOptions, build_state, purge_orphans, router};
use std::time::Duration;
use tracing::info;

#[derive(Parser, Debug)]
#[command(name = "lemmate-server", version, about)]
struct Config {
    /// Address to listen on.
    #[arg(long, env = "LEMMATE_BIND", default_value = "127.0.0.1:8080")]
    bind: SocketAddr,
    /// Directory holding `lemmate.db` and `attachments/`.
    #[arg(long, env = "LEMMATE_DATA_DIR", default_value = "./data")]
    data_dir: PathBuf,
    /// Write a snapshot after this many updates to a doc.
    #[arg(long, env = "LEMMATE_SNAPSHOT_EVERY_UPDATES", default_value_t = 500)]
    snapshot_every_updates: u32,
    /// ... or when the oldest unsnapshotted update is this old (minutes).
    #[arg(long, env = "LEMMATE_SNAPSHOT_EVERY_MINUTES", default_value_t = 10)]
    snapshot_every_minutes: u64,
    /// Keep raw updates (fine-grained history) for this many days; versions are kept forever.
    #[arg(long, env = "LEMMATE_RETAIN_DAYS", default_value_t = 90)]
    retain_days: u64,
    /// Directory with the built web client (ui/dist) to serve at /; omit for API + sync only.
    #[arg(long, env = "LEMMATE_WEB_DIR")]
    web_dir: Option<PathBuf>,
    /// Disable accounts entirely (development only): every request acts as a local owner.
    #[arg(long, env = "LEMMATE_NO_AUTH", value_parser = clap::builder::BoolishValueParser::new(), num_args = 0..=1, default_missing_value = "true", default_value = "false")]
    no_auth: bool,
    /// Let anyone register. Without it, only the first account (the admin) can register and
    /// the admin creates further accounts.
    #[arg(long, env = "LEMMATE_ALLOW_REGISTRATION", value_parser = clap::builder::BoolishValueParser::new(), num_args = 0..=1, default_missing_value = "true", default_value = "false")]
    allow_registration: bool,
    /// Mark session cookies Secure (set when the server is reached over HTTPS).
    #[arg(long, env = "LEMMATE_SECURE_COOKIES", value_parser = clap::builder::BoolishValueParser::new(), num_args = 0..=1, default_missing_value = "true", default_value = "false")]
    secure_cookies: bool,
    /// pandoc binary for exports (default: `pandoc` on PATH).
    #[arg(long, env = "LEMMATE_PANDOC")]
    pandoc: Option<PathBuf>,
    /// quarto binary for "Render with Quarto" (default: `quarto` on PATH).
    #[arg(long, env = "LEMMATE_QUARTO")]
    quarto: Option<PathBuf>,
    /// Refuse Quarto renders even when quarto is installed. A render honours the note's front
    /// matter, which can run Lua filters and read files on this host; turn it off if everyone
    /// who can edit a note should not be able to do that.
    #[arg(long, env = "LEMMATE_DISABLE_QUARTO", value_parser = clap::builder::BoolishValueParser::new(), num_args = 0..=1, default_missing_value = "true", default_value = "false")]
    disable_quarto: bool,
    /// Purge attachment blobs that have been unreferenced for this many days.
    #[arg(long, env = "LEMMATE_ATTACHMENT_GRACE_DAYS", default_value_t = 30)]
    attachment_grace_days: u64,
    /// Turn off email + password sign-in; accounts then come only from the OIDC provider.
    /// Refused without --oidc-issuer, which would leave no way in.
    #[arg(long, env = "LEMMATE_DISABLE_PASSWORD_LOGIN", value_parser = clap::builder::BoolishValueParser::new(), num_args = 0..=1, default_missing_value = "true", default_value = "false")]
    disable_password_login: bool,
    /// The address people reach this server at, e.g. https://notes.example.org. Needed for OIDC:
    /// the provider sends the browser back to <public-url>/api/v1/auth/oidc/callback.
    #[arg(long, env = "LEMMATE_PUBLIC_URL")]
    public_url: Option<String>,
    /// OpenID Connect issuer URL (Authelia, Keycloak, Google, …); setting it turns OIDC sign-in on.
    #[arg(long, env = "LEMMATE_OIDC_ISSUER")]
    oidc_issuer: Option<String>,
    /// The client id registered with the provider.
    #[arg(long, env = "LEMMATE_OIDC_CLIENT_ID")]
    oidc_client_id: Option<String>,
    /// The client secret (omit for a public client).
    #[arg(long, env = "LEMMATE_OIDC_CLIENT_SECRET", hide_env_values = true)]
    oidc_client_secret: Option<String>,
    /// Read the client secret from this file instead (a Docker or systemd secret).
    #[arg(long, env = "LEMMATE_OIDC_CLIENT_SECRET_FILE")]
    oidc_client_secret_file: Option<PathBuf>,
    /// What the sign-in button calls the provider.
    #[arg(long, env = "LEMMATE_OIDC_NAME", default_value = "single sign-on")]
    oidc_name: String,
    /// Scopes to ask for.
    #[arg(long, env = "LEMMATE_OIDC_SCOPES", default_value = "openid email profile")]
    oidc_scopes: String,
}

/// The OIDC settings, if any were given, checked for consistency.
fn oidc_config(cfg: &Config) -> anyhow::Result<Option<OidcConfig>> {
    let Some(issuer) = cfg.oidc_issuer.clone() else {
        if cfg.oidc_client_id.is_some() {
            anyhow::bail!("--oidc-client-id is set but --oidc-issuer is not");
        }
        return Ok(None);
    };
    let client_id = cfg.oidc_client_id.clone().context("--oidc-issuer needs --oidc-client-id")?;
    let public = cfg.public_url.as_deref().context("--oidc-issuer needs --public-url")?;
    let client_secret = match (&cfg.oidc_client_secret, &cfg.oidc_client_secret_file) {
        (Some(_), Some(_)) => {
            anyhow::bail!("give the OIDC client secret once, not both inline and as a file")
        }
        (Some(s), None) => Some(s.clone()),
        (None, Some(p)) => Some(
            std::fs::read_to_string(p).with_context(|| format!("reading {}", p.display()))?.trim().to_owned(),
        ),
        (None, None) => None,
    };
    let config = OidcConfig {
        issuer,
        client_id,
        client_secret,
        redirect_url: format!("{}/api/v1/auth/oidc/callback", public.trim_end_matches('/')),
        display_name: cfg.oidc_name.clone(),
        scopes: cfg.oidc_scopes.clone(),
    };
    config.validate().map_err(anyhow::Error::msg)?;
    Ok(Some(config))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,tower_http=debug".into()),
        )
        .init();
    let cfg = Config::parse();
    let oidc = oidc_config(&cfg)?;
    if cfg.disable_password_login && oidc.is_none() && !cfg.no_auth {
        anyhow::bail!("--disable-password-login without --oidc-issuer would leave no way to sign in");
    }
    std::fs::create_dir_all(&cfg.data_dir).with_context(|| format!("creating {}", cfg.data_dir.display()))?;
    std::fs::create_dir_all(cfg.data_dir.join("attachments"))?;
    let mut store = Store::open(cfg.data_dir.join("lemmate.db")).context("opening lemmate.db")?;
    if let Some(n) = lemmate_server::app::reindex_if_stale(&mut store).context("re-indexing notes")? {
        info!(notes = n, "re-indexed notes for a newer indexer");
    }

    let options = ServerOptions {
        attachments_dir: cfg.data_dir.join("attachments"),
        web_dir: cfg.web_dir.clone(),
        pandoc: cfg.pandoc.clone(),
        quarto: cfg.quarto.clone(),
        quarto_enabled: !cfg.disable_quarto,
        password_login: !cfg.disable_password_login,
        oidc: oidc.clone(),
        auth: if cfg.no_auth {
            AuthMode::Disabled
        } else {
            AuthMode::Enabled {
                allow_registration: cfg.allow_registration,
                secure_cookies: cfg.secure_cookies,
            }
        },
        attachment_grace: Duration::from_secs(cfg.attachment_grace_days * 24 * 60 * 60),
        policy: RetentionPolicy {
            snapshot_every_updates: cfg.snapshot_every_updates,
            snapshot_interval: Duration::from_secs(cfg.snapshot_every_minutes * 60),
            retain_updates: Duration::from_secs(cfg.retain_days * 24 * 60 * 60),
        },
    };
    let state = build_state(store, options);
    // Hourly orphan sweep (also once at startup).
    let sweeper = state.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(60 * 60));
        loop {
            tick.tick().await;
            match purge_orphans(&sweeper, lemmate_core::store::now_ms(), sweeper.options.attachment_grace)
                .await
            {
                Ok(r) if r.purged > 0 || r.newly_orphaned > 0 => info!(?r, "attachment sweep"),
                Ok(_) => {}
                Err(e) => tracing::warn!(%e, "attachment sweep"),
            }
        }
    });
    let app = router(state);
    let listener = tokio::net::TcpListener::bind(cfg.bind).await?;
    if cfg.no_auth {
        tracing::warn!("authentication disabled (--no-auth): do not expose this server to a network");
    }
    if let Some(o) = &oidc {
        info!(issuer = %o.issuer, callback = %o.redirect_url, password_login = !cfg.disable_password_login, "OIDC sign-in on");
    }
    info!(bind = %cfg.bind, data_dir = %cfg.data_dir.display(), auth = !cfg.no_auth, "lemmate-server listening");
    axum::serve(listener, app).await?;
    Ok(())
}

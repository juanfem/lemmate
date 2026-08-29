//! notes-server — sync relay, persistence, and REST API (SPEC §3.1, §7, §13).
//!
//! M0 scope: an unauthenticated single-process relay suitable for local development. Accounts,
//! roles, and per-doc permission checks (SPEC §11) arrive in M2 and slot into `handle_frame`.

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::Context;
use clap::Parser;
use notes_core::RetentionPolicy;
use notes_core::Store;
use notes_server::{ServerOptions, build_state, router};
use std::time::Duration;
use tracing::info;

#[derive(Parser, Debug)]
#[command(name = "notes-server", version, about)]
struct Config {
    /// Address to listen on.
    #[arg(long, env = "NOTES_BIND", default_value = "127.0.0.1:8080")]
    bind: SocketAddr,
    /// Directory holding `notes.db` and `attachments/`.
    #[arg(long, env = "NOTES_DATA_DIR", default_value = "./data")]
    data_dir: PathBuf,
    /// Write a snapshot after this many updates to a doc.
    #[arg(long, env = "NOTES_SNAPSHOT_EVERY_UPDATES", default_value_t = 500)]
    snapshot_every_updates: u32,
    /// ... or when the oldest unsnapshotted update is this old (minutes).
    #[arg(long, env = "NOTES_SNAPSHOT_EVERY_MINUTES", default_value_t = 10)]
    snapshot_every_minutes: u64,
    /// Keep raw updates (fine-grained history) for this many days; versions are kept forever.
    #[arg(long, env = "NOTES_RETAIN_DAYS", default_value_t = 90)]
    retain_days: u64,
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
    std::fs::create_dir_all(&cfg.data_dir).with_context(|| format!("creating {}", cfg.data_dir.display()))?;
    std::fs::create_dir_all(cfg.data_dir.join("attachments"))?;
    let store = Store::open(cfg.data_dir.join("notes.db")).context("opening notes.db")?;

    let options = ServerOptions {
        attachments_dir: cfg.data_dir.join("attachments"),
        policy: RetentionPolicy {
            snapshot_every_updates: cfg.snapshot_every_updates,
            snapshot_interval: Duration::from_secs(cfg.snapshot_every_minutes * 60),
            retain_updates: Duration::from_secs(cfg.retain_days * 24 * 60 * 60),
        },
    };
    let app = router(build_state(store, options));
    let listener = tokio::net::TcpListener::bind(cfg.bind).await?;
    info!(bind = %cfg.bind, data_dir = %cfg.data_dir.display(), "notes-server listening");
    axum::serve(listener, app).await?;
    Ok(())
}

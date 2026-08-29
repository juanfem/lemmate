//! notes-server — sync relay, persistence, and REST API (SPEC §3.1, §7, §13).
//!
//! M0 scope: an unauthenticated single-process relay suitable for local development. Accounts,
//! roles, and per-doc permission checks (SPEC §11) arrive in M2 and slot into `handle_frame`.

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::Context;
use clap::Parser;
use notes_core::Store;
use notes_server::{build_state, router};
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

    let app = router(build_state(store));
    let listener = tokio::net::TcpListener::bind(cfg.bind).await?;
    info!(bind = %cfg.bind, data_dir = %cfg.data_dir.display(), "notes-server listening");
    axum::serve(listener, app).await?;
    Ok(())
}

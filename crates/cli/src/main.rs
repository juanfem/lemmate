//! `notes` — command-line client (SPEC §13.2).
//!
//! M0 scope: local commands built directly on `notes-core`, plus `sync` against a server.
//! Accounts (`login`) arrive with M2.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Context;
use clap::{Parser, Subcommand};
use notes_core::client::{self, SyncOptions};
use notes_core::credentials;
use notes_core::export;
use notes_core::import::{self, ImportOptions};
use notes_core::local::LocalOptions;
use notes_core::{NoteId, Projection, Store, VaultId, markdown};

#[derive(Parser)]
#[command(name = "notes", version, about = "Self-hosted markdown notes")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Index a note file or a whole vault directory and print what the engine extracts.
    Index {
        path: PathBuf,
        /// Emit JSON (one `NoteIndex` for a file, an array of `{path, index}` for a directory).
        #[arg(long)]
        json: bool,
    },
    /// Full-text search over a vault directory (builds a throwaway index in memory).
    Search {
        vault: PathBuf,
        query: String,
        #[arg(long, default_value_t = 20)]
        limit: u32,
    },
    /// Keep a vault directory in sync with a server (creates `<vault>/.notes/`).
    Sync {
        /// Vault directory; created if missing.
        #[arg(long)]
        vault: PathBuf,
        /// Server base URL, e.g. http://127.0.0.1:8080
        #[arg(long, env = "NOTES_SERVER")]
        server: String,
        /// Vault id to join (required to pull an existing vault into an empty directory).
        #[arg(long)]
        vault_id: Option<String>,
        /// Sync once and exit instead of watching for changes.
        #[arg(long)]
        once: bool,
        /// PEM file of a private CA to trust for wss:// and https:// (default: public roots).
        #[arg(long, env = "NOTES_CA_CERT")]
        ca_cert: Option<PathBuf>,
        /// Also run the local relay on this address (e.g. 127.0.0.1:8081): serves the sync
        /// socket and API from the local copy, so a UI keeps working offline.
        #[arg(long)]
        serve: Option<std::net::SocketAddr>,
        /// Built web client to serve at / on the relay (ui/dist).
        #[arg(long, env = "NOTES_WEB_DIR")]
        web_dir: Option<PathBuf>,
        /// Access token (default: the one saved by `notes login` for this server).
        #[arg(long, env = "NOTES_TOKEN")]
        token: Option<String>,
    },
    /// Sign in to a server and save the session token for `sync` and the desktop app.
    Login {
        /// Server base URL, e.g. https://notes.example.org
        #[arg(long, env = "NOTES_SERVER")]
        server: String,
        #[arg(long)]
        email: String,
        /// Password (prompted when omitted).
        #[arg(long, env = "NOTES_PASSWORD")]
        password: Option<String>,
        /// Create the account instead of signing in (first account, or when registration is open).
        #[arg(long)]
        register: bool,
        #[arg(long, env = "NOTES_CA_CERT")]
        ca_cert: Option<PathBuf>,
    },
    /// Forget the saved token for a server.
    Logout {
        #[arg(long, env = "NOTES_SERVER")]
        server: String,
    },
    /// Import notes from another tool into a vault directory (SPEC §11.4).
    Import {
        #[command(subcommand)]
        source: ImportSource,
    },
    /// Export a vault (SPEC §12).
    Export {
        #[command(subcommand)]
        format: ExportFormat,
    },
    /// Print versions and environment facts.
    Doctor,
}

#[derive(Subcommand)]
enum ImportSource {
    /// Copy an Obsidian vault, converting callouts and image embeds.
    Obsidian {
        /// The Obsidian vault directory to read.
        src: PathBuf,
        /// Destination notes vault directory; created if missing.
        #[arg(long)]
        into: PathBuf,
        /// Replace files that already exist in the destination.
        #[arg(long)]
        overwrite: bool,
    },
}

#[derive(Subcommand)]
enum ExportFormat {
    /// Zip the vault's notes and attachments (no pandoc needed).
    Zip {
        /// Vault directory to export.
        vault: PathBuf,
        /// Archive to write.
        out: PathBuf,
    },
}

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()),
        )
        .init();
    match run(Cli::parse()) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> anyhow::Result<ExitCode> {
    match cli.cmd {
        Cmd::Index { path, json } => {
            if path.is_dir() {
                let proj = Projection::new(&path);
                let mut out = Vec::new();
                for rel in proj.walk_notes()? {
                    let ix = markdown::index(&proj.read(&rel)?).with_context(|| rel.clone())?;
                    if json {
                        out.push(serde_json::json!({ "path": rel, "index": ix }));
                    } else {
                        println!(
                            "{rel}\n  title: {}\n  tags: {}\n  links: {}",
                            ix.title.as_deref().unwrap_or("-"),
                            ix.tags.join(", "),
                            ix.wikilinks.len() + ix.links.len()
                        );
                    }
                }
                if json {
                    println!("{}", serde_json::to_string_pretty(&out)?);
                }
            } else {
                let mut ix = markdown::index(&std::fs::read_to_string(&path)?)?;
                if json {
                    ix.plain_text.clear(); // keep corpus fixtures stable; plain text is not part of the contract
                    println!("{}", serde_json::to_string_pretty(&ix)?);
                } else {
                    println!("{ix:#?}");
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Search { vault, query, limit } => {
            let proj = Projection::new(&vault);
            let mut store = Store::open_in_memory()?;
            let vault_id = VaultId::new();
            let mut paths = std::collections::HashMap::new();
            for rel in proj.walk_notes()? {
                let ix = markdown::index(&proj.read(&rel)?).with_context(|| rel.clone())?;
                let id = NoteId::new();
                store.upsert_note(id, vault_id, &rel, ix.title.as_deref())?;
                store.index_note(id, &ix)?;
                paths.insert(id, rel);
            }
            let hits = store.search(&query, limit)?;
            if hits.is_empty() {
                println!("no matches");
            }
            for h in hits {
                println!("{}\n  {}", paths[&h.note_id], h.snippet.replace('\n', " "));
            }
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Login { server, email, password, register, ca_cert } => {
            let password = match password {
                Some(p) => p,
                None => rpassword::prompt_password("Password: ").context("reading password")?,
            };
            let agent = notes_core::tls::http_agent(ca_cert.as_deref())?;
            let base = credentials::key(&server);
            let path = if register { "/api/v1/auth/register" } else { "/api/v1/auth/login" };
            let body = serde_json::json!({ "email": email, "password": password, "device": hostname() });
            let mut resp = agent
                .post(format!("{base}{path}"))
                .header("content-type", "application/json")
                .send(body.to_string().as_bytes())
                .map_err(|e| {
                    anyhow::anyhow!("{}: {e}", if register { "registration failed" } else { "login failed" })
                })?;
            let json: serde_json::Value = serde_json::from_str(&resp.body_mut().read_to_string()?)?;
            let token = json["token"].as_str().context("server returned no token")?;
            credentials::save(&base, token)?;
            println!(
                "signed in as {} on {base}; token saved to {}",
                json["user"]["email"].as_str().unwrap_or(&email),
                credentials::path().display()
            );
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Logout { server } => {
            credentials::forget(&server)?;
            println!("forgot token for {}", credentials::key(&server));
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Sync { vault, server, vault_id, once, ca_cert, serve, web_dir, token } => {
            let vault_id = vault_id.map(|s| s.parse::<VaultId>()).transpose().context("--vault-id")?;
            std::fs::create_dir_all(&vault).with_context(|| format!("creating {}", vault.display()))?;
            let token = token.or_else(|| credentials::load(&server));
            let opts = SyncOptions { vault_dir: vault, server_url: server, vault_id, once, ca_cert, token };
            let rt = tokio::runtime::Runtime::new()?;
            let report = match serve {
                Some(bind) => rt.block_on(async {
                    let handle = client::start(opts, LocalOptions { bind, web_dir }).await?;
                    println!("local relay: http://{}/#/v/{}", handle.addr, handle.vault_id);
                    handle.wait().await
                })?,
                None => rt.block_on(client::run(opts))?,
            };
            if once {
                println!("in sync: vault {} ({} notes)", report.vault_id, report.notes);
            }
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Import { source } => {
            let ImportSource::Obsidian { src, into, overwrite } = source;
            let report = import::import_obsidian(&src, &into, &ImportOptions { overwrite })
                .with_context(|| format!("importing {} into {}", src.display(), into.display()))?;
            println!("{report}");
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Export { format } => {
            let ExportFormat::Zip { vault, out } = format;
            let report = export::export_zip(&vault, &out)
                .with_context(|| format!("exporting {} to {}", vault.display(), out.display()))?;
            println!("{report} -> {}", out.display());
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Doctor => {
            println!("notes {}", env!("CARGO_PKG_VERSION"));
            println!("schema version: {}", notes_core::store::SCHEMA_VERSION);
            println!("sqlite: {}", rusqlite_version());
            for tool in ["pandoc", "quarto"] {
                let found = std::env::var_os("PATH")
                    .map(|p| std::env::split_paths(&p).any(|d| d.join(tool).is_file()))
                    .unwrap_or(false);
                println!("{tool}: {}", if found { "found" } else { "not found (export disabled)" });
            }
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn hostname() -> String {
    std::fs::read_to_string("/etc/hostname").map(|s| s.trim().to_owned()).unwrap_or_else(|_| "cli".into())
}

fn rusqlite_version() -> String {
    notes_core::store::sqlite_version()
}

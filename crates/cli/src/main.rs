//! `notes` — command-line client (SPEC §13.2).
//!
//! M0 scope: local, offline commands built directly on `notes-core`. Server-backed commands
//! (`login`, `sync`) are stubs until the sync loop lands.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, bail};
use clap::{Parser, Subcommand};
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
    /// Run the projection sync loop for a vault against a server. (M0: not yet implemented.)
    Sync {
        #[arg(long)]
        vault: PathBuf,
        #[arg(long, env = "NOTES_SERVER")]
        server: String,
    },
    /// Print versions and environment facts.
    Doctor,
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
        Cmd::Sync { vault, server } => {
            bail!(
                "`notes sync` is not implemented yet (vault {}, server {server}); see SPEC.md §6.3/§7",
                vault.display()
            )
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

fn rusqlite_version() -> String {
    notes_core::store::sqlite_version()
}

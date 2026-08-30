//! `notes` — command-line client (SPEC §13.2, §13.3).
//!
//! Two families of commands: local ones built directly on `lemmate-core` (`index`, `search`,
//! `import`, `export`, `sync`, `doctor`), and server-backed ones that speak the REST API
//! (`vaults`, `ls`, `cat`, `new`, `edit`, `mv`, `rm`, `daily`, `find`, `backlinks`, `tags`) —
//! plus `mcp`, which exposes the same API to agents over the Model Context Protocol.

use std::io::{IsTerminal, Read};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, bail};
use clap::{Args, Parser, Subcommand};
use lemmate_cli::mcp;
use lemmate_cli::remote::{NotesApi, Remote, resolve_vault};
use lemmate_core::client::{self, SyncOptions};
use lemmate_core::credentials;
use lemmate_core::export;
use lemmate_core::import::{self, ImportOptions};
use lemmate_core::local::LocalOptions;
use lemmate_core::{NoteId, Projection, Store, VaultId, markdown};

#[derive(Parser)]
#[command(name = "lemmate", version, about = "Self-hosted markdown notes")]
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
    /// Keep a vault directory in sync with a server (creates `<vault>/.lemmate/`).
    Sync {
        /// Vault directory; created if missing.
        #[arg(long)]
        vault: PathBuf,
        /// Server base URL, e.g. http://127.0.0.1:8080
        #[arg(long, env = "LEMMATE_SERVER")]
        server: String,
        /// Vault id to join (required to pull an existing vault into an empty directory).
        #[arg(long)]
        vault_id: Option<String>,
        /// Sync once and exit instead of watching for changes.
        #[arg(long)]
        once: bool,
        /// PEM file of a private CA to trust for wss:// and https:// (default: public roots).
        #[arg(long, env = "LEMMATE_CA_CERT")]
        ca_cert: Option<PathBuf>,
        /// Also run the local relay on this address (e.g. 127.0.0.1:8081): serves the sync
        /// socket and API from the local copy, so a UI keeps working offline.
        #[arg(long)]
        serve: Option<std::net::SocketAddr>,
        /// Built web client to serve at / on the relay (ui/dist).
        #[arg(long, env = "LEMMATE_WEB_DIR")]
        web_dir: Option<PathBuf>,
        /// Access token (default: the one saved by `lemmate login` for this server).
        #[arg(long, env = "LEMMATE_TOKEN")]
        token: Option<String>,
    },
    /// Sign in to a server and save the session token for `sync` and the desktop app.
    Login {
        /// Server base URL, e.g. https://notes.example.org
        #[arg(long, env = "LEMMATE_SERVER")]
        server: String,
        #[arg(long)]
        email: String,
        /// Password (prompted when omitted).
        #[arg(long, env = "LEMMATE_PASSWORD")]
        password: Option<String>,
        /// Create the account instead of signing in (first account, or when registration is open).
        #[arg(long)]
        register: bool,
        /// Registration invite. Either the token or the whole URL an admin sent you; implies
        /// --register.
        #[arg(long)]
        invite: Option<String>,
        #[arg(long, env = "LEMMATE_CA_CERT")]
        ca_cert: Option<PathBuf>,
    },
    /// Change a password: your own, or (as an admin) someone else's.
    Passwd {
        /// Server base URL, e.g. https://notes.example.org
        #[arg(long, env = "LEMMATE_SERVER")]
        server: String,
        /// Reset this account instead of your own. Admin only, and no current password is asked.
        #[arg(long)]
        email: Option<String>,
        /// Access token (default: the one saved by `lemmate login` for this server).
        #[arg(long, env = "LEMMATE_TOKEN")]
        token: Option<String>,
        #[arg(long, env = "LEMMATE_CA_CERT")]
        ca_cert: Option<PathBuf>,
    },
    /// Mint, list, or revoke single-use registration invites (admin only).
    Invite {
        /// Server base URL, e.g. https://notes.example.org
        #[arg(long, env = "LEMMATE_SERVER")]
        server: String,
        /// List existing invites instead of minting one.
        #[arg(long, conflicts_with_all = ["revoke", "expires_days"])]
        list: bool,
        /// Revoke an unused invite by the id `--list` shows.
        #[arg(long, conflicts_with = "expires_days")]
        revoke: Option<String>,
        /// Expire the new invite after this many days (default: never, but still single-use).
        #[arg(long)]
        expires_days: Option<u32>,
        /// Emit JSON instead of lines.
        #[arg(long)]
        json: bool,
        /// Access token (default: the one saved by `lemmate login` for this server).
        #[arg(long, env = "LEMMATE_TOKEN")]
        token: Option<String>,
        #[arg(long, env = "LEMMATE_CA_CERT")]
        ca_cert: Option<PathBuf>,
    },
    /// Forget the saved token for a server.
    Logout {
        #[arg(long, env = "LEMMATE_SERVER")]
        server: String,
    },
    /// List the vaults this account can see.
    Vaults {
        #[command(flatten)]
        remote: RemoteArgs,
        /// Emit JSON instead of one vault per line.
        #[arg(long)]
        json: bool,
    },
    /// List the notes in a vault on the server, one path per line.
    Ls {
        #[command(flatten)]
        remote: RemoteArgs,
        /// Emit JSON (id, path, title) instead of paths.
        #[arg(long)]
        json: bool,
    },
    /// Print a note's markdown.
    Cat {
        #[command(flatten)]
        remote: RemoteArgs,
        /// Vault-relative path (the `.md` is optional) or note id.
        note: String,
        /// Emit the whole note as JSON instead of just its content.
        #[arg(long)]
        json: bool,
    },
    /// Create a note from a file, stdin, or nothing.
    New {
        #[command(flatten)]
        remote: RemoteArgs,
        /// Vault-relative path for the new note; `.md` is added when missing.
        path: String,
        /// Read the content from this file (`-` for stdin). Default: stdin when it is piped.
        #[arg(long)]
        from: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    /// Edit a note in $EDITOR and send the result back (merged as a diff, not a blind write).
    Edit {
        #[command(flatten)]
        remote: RemoteArgs,
        /// Vault-relative path or note id.
        note: String,
        /// Take the new content from this file (`-` for stdin) instead of opening an editor.
        #[arg(long)]
        from: Option<PathBuf>,
    },
    /// Move or rename a note.
    Mv {
        #[command(flatten)]
        remote: RemoteArgs,
        /// Vault-relative path or note id.
        note: String,
        /// New vault-relative path.
        new_path: String,
    },
    /// Move a note to the trash (its history is kept).
    Rm {
        #[command(flatten)]
        remote: RemoteArgs,
        /// Vault-relative path or note id.
        note: String,
    },
    /// Print the daily note for a date, creating it when it does not exist yet.
    Daily {
        #[command(flatten)]
        remote: RemoteArgs,
        /// Calendar date as YYYY-MM-DD (default: today).
        date: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Full-text search a vault on the server (the local `search` walks a directory instead).
    Find {
        #[command(flatten)]
        remote: RemoteArgs,
        query: String,
        #[arg(long, default_value_t = 20)]
        limit: u32,
        #[arg(long)]
        json: bool,
    },
    /// List the notes that link to a note.
    Backlinks {
        #[command(flatten)]
        remote: RemoteArgs,
        /// Vault-relative path or note id.
        note: String,
        #[arg(long)]
        json: bool,
    },
    /// List the tags used in a vault, most used first.
    Tags {
        #[command(flatten)]
        remote: RemoteArgs,
        #[arg(long)]
        json: bool,
    },
    /// Serve the Model Context Protocol over stdio so an agent can read and write notes.
    Mcp {
        #[command(flatten)]
        remote: RemoteArgs,
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

/// The options every server-backed command shares.
#[derive(Args, Clone, Debug)]
struct RemoteArgs {
    /// Server base URL, e.g. https://notes.example.org
    #[arg(long, env = "LEMMATE_SERVER")]
    server: String,
    /// Access token (default: the one saved by `lemmate login` for this server).
    #[arg(long, env = "LEMMATE_TOKEN")]
    token: Option<String>,
    /// PEM file of a private CA to trust for https:// (default: public roots).
    #[arg(long, env = "LEMMATE_CA_CERT")]
    ca_cert: Option<PathBuf>,
    /// Vault id; optional when the account has exactly one vault.
    #[arg(long, env = "LEMMATE_VAULT")]
    vault: Option<String>,
}

impl RemoteArgs {
    fn connect(&self) -> anyhow::Result<Remote> {
        Remote::from_args(&self.server, self.token.clone(), self.ca_cert.as_deref())
    }

    /// A client plus the vault to work in (asks the server when `--vault` was left out).
    fn open(&self) -> anyhow::Result<(Remote, String)> {
        let remote = self.connect()?;
        let vault = resolve_vault(&remote, self.vault.as_deref())?;
        Ok((remote, vault))
    }
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
    // stderr, always: `lemmate mcp` speaks JSON-RPC on stdout.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
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
        Cmd::Login { server, email, password, register, invite, ca_cert } => {
            let password = match password {
                Some(p) => p,
                None => rpassword::prompt_password("Password: ").context("reading password")?,
            };
            let base = credentials::key(&server);
            // An invite is only meaningful when creating the account, so it implies --register
            // rather than erroring on the combination people will actually type.
            let register = register || invite.is_some();
            credentials::login(
                &base,
                &email,
                &password,
                register,
                invite.as_deref(),
                ca_cert.as_deref(),
                &hostname(),
            )?;
            println!("signed in as {email} on {base}; token saved to {}", credentials::path().display());
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Passwd { server, email, token, ca_cert } => {
            let remote = Remote::from_args(&server, token, ca_cert.as_deref())?;
            // Resetting someone else is the admin path and deliberately asks for nothing but the
            // new password — the whole point is that the old one is unknown.
            let current = match &email {
                Some(_) => None,
                None => Some(rpassword::prompt_password("Current password: ").context("reading password")?),
            };
            let new = rpassword::prompt_password("New password: ").context("reading password")?;
            if new != rpassword::prompt_password("Repeat new password: ").context("reading password")? {
                bail!("the two new passwords do not match");
            }
            if new.chars().count() < 8 {
                bail!("the new password must be at least 8 characters");
            }
            let revoked = remote.change_password(email.as_deref(), current.as_deref(), &new)?;
            let whose = email.as_deref().unwrap_or("your account");
            println!("password changed for {whose}; {revoked} other session(s) signed out");
            if email.is_none() {
                println!("this device keeps its saved token; other devices need `lemmate login` again");
            }
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Invite { server, list, revoke, expires_days, json, token, ca_cert } => {
            let remote = Remote::from_args(&server, token, ca_cert.as_deref())?;
            if let Some(id) = revoke {
                remote.revoke_invite(&id)?;
                println!("revoked invite {id}");
                return Ok(ExitCode::SUCCESS);
            }
            if list {
                let invites = remote.invites()?;
                if json {
                    println!("{}", serde_json::to_string_pretty(&invites)?);
                } else if invites.is_empty() {
                    println!("no invites");
                } else {
                    for i in invites {
                        let state = match (&i.used_by, i.usable) {
                            (Some(email), _) => format!("used by {email}"),
                            (None, true) => "unused".to_owned(),
                            (None, false) => "expired".to_owned(),
                        };
                        println!("{}  {state}", i.id);
                    }
                }
                return Ok(ExitCode::SUCCESS);
            }
            let invite = remote.create_invite(expires_days)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&invite)?);
            } else {
                let link = invite.link.as_deref().unwrap_or_default();
                println!("{}{link}", remote.base());
                println!(
                    "single use; send it however you like. Revoke with `lemmate invite --revoke {}`",
                    invite.id
                );
            }
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
        Cmd::Vaults { remote, json } => {
            let vaults = remote.connect()?.vaults()?;
            if json {
                print_json(&vaults)?;
            } else if vaults.is_empty() {
                println!("no vaults yet — create one with `lemmate sync`");
            } else {
                for v in &vaults {
                    println!("{}  {} notes", v.id, v.notes);
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Ls { remote, json } => {
            let (r, vault) = remote.open()?;
            let mut notes = r.notes(&vault)?;
            notes.sort_by(|a, b| a.path.cmp(&b.path));
            if json {
                print_json(&notes)?;
            } else {
                for n in &notes {
                    println!("{}", n.path);
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Cat { remote, note, json } => {
            let (r, vault) = remote.open()?;
            let found = r.resolve_note(&vault, &note)?;
            let note = r.note(&vault, &found.id)?;
            if json {
                print_json(&note)?;
            } else {
                print_content(&note.content);
            }
            Ok(ExitCode::SUCCESS)
        }
        Cmd::New { remote, path, from, json } => {
            let (r, vault) = remote.open()?;
            let content = read_content(from.as_deref())?;
            let note = r.create(&vault, &path, &content)?;
            if json {
                print_json(&note)?;
            } else {
                println!("created {} ({})", note.path, note.id);
            }
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Edit { remote, note, from } => {
            let (r, vault) = remote.open()?;
            let found = r.resolve_note(&vault, &note)?;
            let current = r.note(&vault, &found.id)?.content;
            let edited = match from {
                Some(path) => Some(read_content(Some(&path))?),
                None => edit_in_editor(&current, &found.path)?,
            };
            match edited {
                Some(text) if text != current => {
                    let note = r.replace(&vault, &found.id, &text)?;
                    println!("updated {} ({})", note.path, note.id);
                }
                _ => println!("no changes to {}", found.path),
            }
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Mv { remote, note, new_path } => {
            let (r, vault) = remote.open()?;
            let found = r.resolve_note(&vault, &note)?;
            r.rename(&vault, &found.id, &new_path)?;
            println!("{} -> {}", found.path, lemmate_cli::remote::normalize_path(&new_path));
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Rm { remote, note } => {
            let (r, vault) = remote.open()?;
            let found = r.resolve_note(&vault, &note)?;
            r.delete(&vault, &found.id)?;
            println!("trashed {} ({})", found.path, found.id);
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Daily { remote, date, json } => {
            let date = date.map(|d| d.trim().to_owned()).unwrap_or_else(mcp::today);
            if !mcp::valid_date(&date) {
                bail!("date must be YYYY-MM-DD (got {date:?})");
            }
            let (r, vault) = remote.open()?;
            let note = r.daily(&vault, &date)?;
            if json {
                print_json(&note)?;
            } else {
                print_content(&note.content);
            }
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Find { remote, query, limit, json } => {
            let (r, vault) = remote.open()?;
            let hits = r.search(&vault, &query, limit)?;
            if json {
                print_json(&hits)?;
            } else if hits.is_empty() {
                println!("no matches");
            } else {
                let paths: std::collections::HashMap<String, String> =
                    r.notes(&vault)?.into_iter().map(|n| (n.id, n.path)).collect();
                for h in &hits {
                    let path = paths.get(&h.note_id).unwrap_or(&h.note_id);
                    println!("{path}\n  {}", h.snippet.replace('\n', " ").trim());
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Backlinks { remote, note, json } => {
            let (r, vault) = remote.open()?;
            let found = r.resolve_note(&vault, &note)?;
            let links = r.backlinks(&vault, &found.id)?;
            if json {
                print_json(&links)?;
            } else if links.is_empty() {
                println!("nothing links to {}", found.path);
            } else {
                for n in &links {
                    println!("{}", n.path);
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Tags { remote, json } => {
            let (r, vault) = remote.open()?;
            let mut tags = r.tags(&vault)?;
            tags.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.tag.cmp(&b.tag)));
            if json {
                print_json(&tags)?;
            } else {
                for t in &tags {
                    println!("#{}  {}", t.tag, t.count);
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Mcp { remote } => {
            let (r, vault) = remote.open()?;
            mcp::serve_stdio(&r, vault)?;
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
            println!("lemmate {}", env!("CARGO_PKG_VERSION"));
            println!("schema version: {}", lemmate_core::store::SCHEMA_VERSION);
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

fn print_json<T: serde::Serialize>(value: &T) -> anyhow::Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

/// Note content to stdout, with exactly one trailing newline.
fn print_content(content: &str) {
    if content.ends_with('\n') || content.is_empty() {
        print!("{content}");
    } else {
        println!("{content}");
    }
}

/// Content for `new`/`edit --from`: a file, `-`/a pipe for stdin, or empty on a bare terminal.
fn read_content(from: Option<&Path>) -> anyhow::Result<String> {
    let stdin = || {
        let mut buf = String::new();
        std::io::stdin().read_to_string(&mut buf).context("reading stdin")?;
        anyhow::Ok(buf)
    };
    match from {
        Some(p) if p == Path::new("-") => stdin(),
        Some(p) => std::fs::read_to_string(p).with_context(|| format!("reading {}", p.display())),
        None if !std::io::stdin().is_terminal() => stdin(),
        None => Ok(String::new()),
    }
}

/// Open `$VISUAL`/`$EDITOR` on `initial` in a temp file; `None` when nothing changed.
fn edit_in_editor(initial: &str, hint: &str) -> anyhow::Result<Option<String>> {
    let editor = ["VISUAL", "EDITOR"]
        .iter()
        .find_map(|k| std::env::var(k).ok().filter(|v| !v.trim().is_empty()))
        .unwrap_or_else(|| "vi".to_owned());
    let stem: String = hint
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect();
    let file = std::env::temp_dir().join(format!("lemmate-{}-{stem}", std::process::id()));
    std::fs::write(&file, initial).with_context(|| format!("writing {}", file.display()))?;
    let mut words = editor.split_whitespace();
    let program = words.next().unwrap_or("vi");
    let status = std::process::Command::new(program)
        .args(words)
        .arg(&file)
        .status()
        .with_context(|| format!("launching editor {editor:?}"))?;
    let edited = std::fs::read_to_string(&file);
    let _ = std::fs::remove_file(&file);
    if !status.success() {
        bail!("editor {editor:?} exited with {status}");
    }
    let edited = edited.with_context(|| format!("reading back {}", file.display()))?;
    Ok((edited != initial).then_some(edited))
}

fn hostname() -> String {
    std::fs::read_to_string("/etc/hostname").map(|s| s.trim().to_owned()).unwrap_or_else(|_| "cli".into())
}

fn rusqlite_version() -> String {
    lemmate_core::store::sqlite_version()
}

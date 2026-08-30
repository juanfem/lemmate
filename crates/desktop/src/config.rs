//! Desktop configuration: a small TOML file, overridable by CLI flags and environment.
//!
//! There is no setup UI yet (SPEC §14), so a missing or incomplete configuration is a
//! hard error with an explanation of the file format — see [`Config::resolve`].

use std::path::{Path, PathBuf};
use std::str::FromStr;

use anyhow::{Context, bail};
use clap::Parser;
use notes_core::VaultId;
use serde::Deserialize;

/// `notes-desktop` — the Tauri shell around the sync engine's local relay.
#[derive(Parser, Debug)]
#[command(name = "notes-desktop", version, about = "Self-hosted markdown notes (desktop)")]
pub struct Cli {
    /// Configuration file (default: `$XDG_CONFIG_HOME/notes/desktop.toml`).
    #[arg(long, value_name = "FILE", env = "NOTES_DESKTOP_CONFIG")]
    pub config: Option<PathBuf>,
    /// Vault directory to open; created by the engine if missing.
    #[arg(long, value_name = "DIR", env = "NOTES_VAULT_DIR")]
    pub vault_dir: Option<PathBuf>,
    /// Server base URL, e.g. https://notes.example.org
    #[arg(long, value_name = "URL", env = "NOTES_SERVER")]
    pub server_url: Option<String>,
    /// Vault id (ULID) to join, when pulling an existing vault into an empty directory.
    #[arg(long, value_name = "ULID")]
    pub vault_id: Option<String>,
    /// PEM file of a private CA to trust for wss:// and https:// (default: public roots).
    #[arg(long, value_name = "FILE", env = "NOTES_CA_CERT")]
    pub ca_cert: Option<PathBuf>,
    /// Access token (default: the one saved by `notes login` for this server).
    #[arg(long, env = "NOTES_TOKEN")]
    pub token: Option<String>,
    /// Directory of built web assets for the relay to serve (default: bundled, then `ui/dist`).
    #[arg(long, value_name = "DIR", env = "NOTES_WEB_DIR")]
    pub web_dir: Option<PathBuf>,
}

/// The shape of `desktop.toml`. Every key is optional here; the flags may supply it instead.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    vault_dir: Option<PathBuf>,
    server_url: Option<String>,
    vault_id: Option<String>,
    ca_cert: Option<PathBuf>,
    token: Option<String>,
    web_dir: Option<PathBuf>,
}

/// A complete, validated configuration.
#[derive(Debug, Clone)]
pub struct Config {
    pub vault_dir: PathBuf,
    pub server_url: String,
    pub vault_id: Option<VaultId>,
    pub ca_cert: Option<PathBuf>,
    pub token: Option<String>,
    /// `--web-dir` / `NOTES_WEB_DIR`; `None` means "discover it" (see `main::resolve_web_dir`).
    pub web_dir: Option<PathBuf>,
}

/// Where the configuration file lives: `$XDG_CONFIG_HOME/notes/desktop.toml`, falling back
/// to `~/.config/notes/desktop.toml`.
pub fn default_config_path() -> Option<PathBuf> {
    let base = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => PathBuf::from(std::env::var_os("HOME")?).join(".config"),
    };
    Some(base.join("notes").join("desktop.toml"))
}

/// The message printed when the configuration is missing or incomplete.
pub fn format_help(path: Option<&Path>) -> String {
    let shown =
        path.map(|p| p.display().to_string()).unwrap_or_else(|| "~/.config/notes/desktop.toml".into());
    format!(
        "notes-desktop has no vault to open yet (there is no setup UI in this milestone).\n\
         \n\
         Write {shown} like this:\n\
         \n\
         \x20   vault_dir  = \"/home/you/notes\"              # required\n\
         \x20   server_url = \"https://notes.example.org\"    # required\n\
         \x20   vault_id   = \"01J8Z9…\"                      # optional ULID: join an existing vault\n\
         \x20   ca_cert    = \"/etc/ssl/private-ca.pem\"      # optional: trust a private CA\n\
         \x20   web_dir    = \"/path/to/ui/dist\"             # optional: override the bundled web assets\n\
         \n\
         Or pass the same values as flags, which take precedence over the file:\n\
         \n\
         \x20   notes-desktop --vault-dir /home/you/notes --server-url https://notes.example.org\n"
    )
}

impl Config {
    /// Merge the file (if any) with the flags and validate. Flags win over the file.
    pub fn resolve(cli: Cli) -> anyhow::Result<Self> {
        let path = cli.config.clone().or_else(default_config_path);
        // An explicit --config that does not exist is an error; a missing default is not.
        let file = match &path {
            Some(p) if p.is_file() => {
                let text = std::fs::read_to_string(p).with_context(|| format!("reading {}", p.display()))?;
                toml::from_str::<FileConfig>(&text).with_context(|| format!("parsing {}", p.display()))?
            }
            Some(p) if cli.config.is_some() => {
                bail!("configuration file {} does not exist", p.display())
            }
            _ => FileConfig::default(),
        };

        let vault_dir = cli.vault_dir.or(file.vault_dir);
        let server_url = cli.server_url.or(file.server_url);
        let (Some(vault_dir), Some(server_url)) = (vault_dir, server_url) else {
            bail!("{}", format_help(path.as_deref()));
        };

        let vault_id = match cli.vault_id.or(file.vault_id) {
            Some(s) => Some(
                VaultId::from_str(s.trim())
                    .map_err(|_| anyhow::anyhow!("invalid vault_id {s:?}: expected a ULID"))?,
            ),
            None => None,
        };

        Ok(Self {
            vault_dir,
            server_url,
            vault_id,
            ca_cert: cli.ca_cert.or(file.ca_cert),
            token: cli.token.or(file.token),
            web_dir: cli.web_dir.or(file.web_dir),
        })
    }
}

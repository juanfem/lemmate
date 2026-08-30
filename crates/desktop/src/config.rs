//! Desktop configuration: a small TOML file, overridable by CLI flags and environment.
//!
//! There is no setup UI yet (SPEC §14), so a missing or incomplete configuration is a
//! hard error with an explanation of the file format — see [`Config::resolve`].

use std::path::{Path, PathBuf};
use std::str::FromStr;

use anyhow::{Context, bail};
use clap::Parser;
use lemmate_core::VaultId;
use serde::Deserialize;

/// `lemmate-desktop` — the Tauri shell around the sync engine's local relay.
#[derive(Parser, Debug)]
#[command(name = "lemmate-desktop", version, about = "Self-hosted markdown notes (desktop)")]
pub struct Cli {
    /// Configuration file (default: `desktop.toml` in the per-user configuration directory).
    #[arg(long, value_name = "FILE", env = "LEMMATE_DESKTOP_CONFIG")]
    pub config: Option<PathBuf>,
    /// Vault directory to open; created by the engine if missing.
    #[arg(long, value_name = "DIR", env = "LEMMATE_VAULT_DIR")]
    pub vault_dir: Option<PathBuf>,
    /// Server base URL, e.g. https://notes.example.org
    #[arg(long, value_name = "URL", env = "LEMMATE_SERVER")]
    pub server_url: Option<String>,
    /// Vault id (ULID) to join, when pulling an existing vault into an empty directory.
    #[arg(long, value_name = "ULID")]
    pub vault_id: Option<String>,
    /// PEM file of a private CA to trust for wss:// and https:// (default: public roots).
    #[arg(long, value_name = "FILE", env = "LEMMATE_CA_CERT")]
    pub ca_cert: Option<PathBuf>,
    /// Access token (default: the one saved by `lemmate login` for this server).
    #[arg(long, env = "LEMMATE_TOKEN")]
    pub token: Option<String>,
    /// Directory of built web assets for the relay to serve (default: bundled, then `ui/dist`).
    #[arg(long, value_name = "DIR", env = "LEMMATE_WEB_DIR")]
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
    /// `--web-dir` / `LEMMATE_WEB_DIR`; `None` means "discover it" (see `main::resolve_web_dir`).
    pub web_dir: Option<PathBuf>,
}

/// Where the configuration file lives: `desktop.toml` in the per-user configuration directory
/// (`~/.config/lemmate`, `~/Library/Application Support/lemmate`, `%APPDATA%\lemmate`).
pub fn default_config_path() -> Option<PathBuf> {
    Some(lemmate_core::paths::config_dir()?.join("desktop.toml"))
}

/// The message printed when the configuration is incomplete.
///
/// Reaching this means a *partial* configuration: one of `--vault-dir` / `--server-url` was given
/// without the other, so `needs_setup` stepped aside for the flags. With neither of them the app
/// opens the setup screen instead of printing this.
pub fn format_help(path: Option<&Path>) -> String {
    let shown = path
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "desktop.toml in your configuration directory".into());
    format!(
        "lemmate-desktop needs both a vault folder and a server URL, and only one of them was \
         given.\n\
         \n\
         Pass the other flag as well:\n\
         \n\
         \x20   lemmate-desktop --vault-dir /home/you/notes --server-url https://notes.example.org\n\
         \n\
         Or run it with neither flag and it opens a setup screen that writes {shown} for you.\n\
         That file looks like this, if you would rather write it yourself:\n\
         \n\
         \x20   vault_dir  = \"/home/you/notes\"              # required\n\
         \x20   server_url = \"https://notes.example.org\"    # required\n\
         \x20   vault_id   = \"01J8Z9…\"                      # optional ULID: join an existing vault\n\
         \x20   ca_cert    = \"/etc/ssl/private-ca.pem\"      # optional: trust a private CA\n\
         \x20   web_dir    = \"/path/to/ui/dist\"             # optional: override the bundled web assets\n\
         \n\
         Flags take precedence over the file.\n"
    )
}

/// What the first-run setup screen needs when there is no usable configuration yet.
pub struct SetupContext {
    pub config_path: PathBuf,
    pub web_dir: Option<PathBuf>,
    pub suggested_vault_dir: PathBuf,
}

impl Config {
    /// `Some` when neither the flags nor the config file name a vault and a server, i.e. the
    /// app should open in setup mode instead of failing.
    pub fn needs_setup(cli: &Cli) -> Option<SetupContext> {
        if cli.vault_dir.is_some() || cli.server_url.is_some() {
            return None;
        }
        let path = cli.config.clone().or_else(default_config_path)?;
        let file = std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| toml::from_str::<FileConfig>(&t).ok())
            .unwrap_or_default();
        if file.vault_dir.is_some() && file.server_url.is_some() {
            return None;
        }
        let home = lemmate_core::paths::home_dir().unwrap_or_else(|| PathBuf::from("."));
        Some(SetupContext {
            config_path: path,
            web_dir: cli.web_dir.clone().or(file.web_dir),
            suggested_vault_dir: home.join("lemmate"),
        })
    }

    /// Write the file the setup screen produced; `resolve` reads it back on the next start.
    pub fn write_setup(
        path: &std::path::Path,
        req: &lemmate_core::local::SetupRequest,
    ) -> anyhow::Result<()> {
        let mut table = toml::Table::new();
        table.insert("vault_dir".into(), toml::Value::String(req.vault_dir.clone()));
        table.insert("server_url".into(), toml::Value::String(req.server_url.clone()));
        if let Some(v) = req.vault_id.as_deref().filter(|v| !v.trim().is_empty()) {
            table.insert("vault_id".into(), toml::Value::String(v.trim().to_owned()));
        }
        if let Some(c) = req.ca_cert.as_deref().filter(|c| !c.trim().is_empty()) {
            table.insert("ca_cert".into(), toml::Value::String(c.trim().to_owned()));
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, toml::to_string(&table)?)
            .with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }

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

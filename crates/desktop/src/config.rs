//! Desktop configuration: a small TOML file, overridable by CLI flags and environment.
//!
//! The shell holds **every vault the account can read** (SPEC §9), so what it needs is a *root*
//! folder with one subfolder per vault, not a single vault directory. `--vault-dir` still opens
//! exactly one vault, for a config written before roots existed and for anyone who wants that;
//! see [`Layout`].
//!
//! A configuration that names neither opens the setup screen; one that is half-given is a hard
//! error with an explanation of the file format — see [`Config::resolve`].

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
    /// Folder holding one subfolder per vault; every vault the account can read is opened here.
    #[arg(long, value_name = "DIR", env = "LEMMATE_ROOT_DIR")]
    pub root_dir: Option<PathBuf>,
    /// Open this one vault directory instead of a root of them; created by the engine if missing.
    #[arg(long, value_name = "DIR", env = "LEMMATE_VAULT_DIR", conflicts_with = "root_dir")]
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
    root_dir: Option<PathBuf>,
    vault_dir: Option<PathBuf>,
    server_url: Option<String>,
    vault_id: Option<String>,
    ca_cert: Option<PathBuf>,
    token: Option<String>,
    web_dir: Option<PathBuf>,
}

/// Where this shell keeps its notes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Layout {
    /// One folder per vault under this root: every vault the account can read (SPEC §9).
    Root(PathBuf),
    /// A single vault in a single folder, optionally joining an existing one by id.
    Single { dir: PathBuf, id: Option<VaultId> },
}

impl Layout {
    /// The directory the relay's port is derived from, so the UI keeps its saved layout across
    /// launches (see `lemmate_core::local::stable_port`).
    pub fn anchor(&self) -> &Path {
        match self {
            Layout::Root(root) => root,
            Layout::Single { dir, .. } => dir,
        }
    }
}

/// A complete, validated configuration.
#[derive(Debug, Clone)]
pub struct Config {
    pub layout: Layout,
    pub server_url: String,
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
/// Reaching this means a *partial* configuration: a folder was given without a server URL or
/// the other way round, so `needs_setup` stepped aside for the flags. With neither of them the
/// app opens the setup screen instead of printing this.
pub fn format_help(path: Option<&Path>) -> String {
    let shown = path
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "desktop.toml in your configuration directory".into());
    format!(
        "lemmate-desktop needs both a folder for your notes and a server URL, and only one of \
         them was given.\n\
         \n\
         Pass the other flag as well:\n\
         \n\
         \x20   lemmate-desktop --root-dir /home/you/lemmate --server-url https://notes.example.org\n\
         \n\
         Or run it with neither flag and it opens a setup screen that writes {shown} for you.\n\
         That file looks like this, if you would rather write it yourself:\n\
         \n\
         \x20   root_dir   = \"/home/you/lemmate\"           # required: one folder per vault below it\n\
         \x20   server_url = \"https://notes.example.org\"    # required\n\
         \x20   ca_cert    = \"/etc/ssl/private-ca.pem\"      # optional: trust a private CA\n\
         \x20   web_dir    = \"/path/to/ui/dist\"             # optional: override the bundled web assets\n\
         \n\
         Use `vault_dir` instead of `root_dir` to open a single vault folder, with an optional\n\
         `vault_id` to join an existing vault. Flags take precedence over the file.\n"
    )
}

/// What the first-run setup screen needs when there is no usable configuration yet.
pub struct SetupContext {
    pub config_path: PathBuf,
    pub web_dir: Option<PathBuf>,
    pub suggested_root_dir: PathBuf,
}

impl Config {
    /// `Some` when neither the flags nor the config file name a folder and a server, i.e. the
    /// app should open in setup mode instead of failing.
    pub fn needs_setup(cli: &Cli) -> Option<SetupContext> {
        if cli.root_dir.is_some() || cli.vault_dir.is_some() || cli.server_url.is_some() {
            return None;
        }
        let path = cli.config.clone().or_else(default_config_path)?;
        let file = std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| toml::from_str::<FileConfig>(&t).ok())
            .unwrap_or_default();
        if (file.root_dir.is_some() || file.vault_dir.is_some()) && file.server_url.is_some() {
            return None;
        }
        let home = lemmate_core::paths::home_dir().unwrap_or_else(|| PathBuf::from("."));
        Some(SetupContext {
            config_path: path,
            web_dir: cli.web_dir.clone().or(file.web_dir),
            suggested_root_dir: home.join("lemmate"),
        })
    }

    /// Write the file the setup screen produced; `resolve` reads it back on the next start.
    pub fn write_setup(
        path: &std::path::Path,
        req: &lemmate_core::local::SetupRequest,
    ) -> anyhow::Result<()> {
        let mut table = toml::Table::new();
        table.insert("root_dir".into(), toml::Value::String(req.root_dir.clone()));
        table.insert("server_url".into(), toml::Value::String(req.server_url.clone()));
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

        // A flag naming one layout wins over a file naming the other, so `--vault-dir` opens one
        // vault even when the file has a root, and `--root-dir` opens the workspace even when
        // the file was written before roots existed.
        let root_dir = cli.root_dir.or_else(|| cli.vault_dir.is_none().then_some(file.root_dir).flatten());
        let vault_dir = cli.vault_dir.or_else(|| root_dir.is_none().then_some(file.vault_dir).flatten());
        let server_url = cli.server_url.or(file.server_url);
        let (Some(server_url), true) = (server_url, root_dir.is_some() || vault_dir.is_some()) else {
            bail!("{}", format_help(path.as_deref()));
        };

        let vault_id = match cli.vault_id.or(file.vault_id) {
            Some(s) => Some(
                VaultId::from_str(s.trim())
                    .map_err(|_| anyhow::anyhow!("invalid vault_id {s:?}: expected a ULID"))?,
            ),
            None => None,
        };
        let layout = match (root_dir, vault_dir) {
            (Some(root), _) => Layout::Root(root),
            (None, Some(dir)) => Layout::Single { dir, id: vault_id },
            (None, None) => unreachable!("checked above"),
        };

        Ok(Self {
            layout,
            server_url,
            ca_cert: cli.ca_cert.or(file.ca_cert),
            token: cli.token.or(file.token),
            web_dir: cli.web_dir.or(file.web_dir),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cli(args: &[&str]) -> Cli {
        let mut all = vec!["lemmate-desktop"];
        all.extend_from_slice(args);
        Cli::try_parse_from(all).expect("flags parse")
    }

    /// The config file is read from `--config`, so these never touch the real one.
    fn write(dir: &Path, body: &str) -> PathBuf {
        let p = dir.join("desktop.toml");
        std::fs::write(&p, body).unwrap();
        p
    }

    #[test]
    fn a_root_opens_every_vault_and_a_vault_dir_opens_one() {
        let tmp = tempfile::tempdir().unwrap();
        let p = write(tmp.path(), "root_dir = \"/n/all\"\nserver_url = \"https://s\"\n");
        let cfg = Config::resolve(cli(&["--config", p.to_str().unwrap()])).unwrap();
        assert_eq!(cfg.layout, Layout::Root("/n/all".into()));

        let p = write(
            tmp.path(),
            "vault_dir = \"/n/one\"\nvault_id = \"01ARZ3NDEKTSV4RRFFQ69G5FAV\"\nserver_url = \"https://s\"\n",
        );
        let cfg = Config::resolve(cli(&["--config", p.to_str().unwrap()])).unwrap();
        assert_eq!(
            cfg.layout,
            Layout::Single { dir: "/n/one".into(), id: Some("01ARZ3NDEKTSV4RRFFQ69G5FAV".parse().unwrap()) }
        );
    }

    #[test]
    fn a_flag_overrides_the_layout_the_file_chose() {
        let tmp = tempfile::tempdir().unwrap();
        let p = write(tmp.path(), "root_dir = \"/n/all\"\nserver_url = \"https://s\"\n");
        let cfg = Config::resolve(cli(&["--config", p.to_str().unwrap(), "--vault-dir", "/n/one"])).unwrap();
        assert_eq!(cfg.layout, Layout::Single { dir: "/n/one".into(), id: None });
    }

    #[test]
    fn a_folder_without_a_server_explains_itself() {
        let tmp = tempfile::tempdir().unwrap();
        let p = write(tmp.path(), "root_dir = \"/n/all\"\n");
        let e = Config::resolve(cli(&["--config", p.to_str().unwrap()])).unwrap_err().to_string();
        assert!(e.contains("--root-dir"), "the help names the flag to pass: {e}");
    }

    #[test]
    fn setup_is_needed_only_when_nothing_names_a_folder_and_a_server() {
        let tmp = tempfile::tempdir().unwrap();
        let empty = tmp.path().join("none.toml");
        assert!(Config::needs_setup(&cli(&["--config", empty.to_str().unwrap()])).is_some());

        let p = write(tmp.path(), "root_dir = \"/n/all\"\nserver_url = \"https://s\"\n");
        assert!(Config::needs_setup(&cli(&["--config", p.to_str().unwrap()])).is_none());

        // Written before roots existed: still a complete configuration.
        let p = write(tmp.path(), "vault_dir = \"/n/one\"\nserver_url = \"https://s\"\n");
        assert!(Config::needs_setup(&cli(&["--config", p.to_str().unwrap()])).is_none());
    }
}

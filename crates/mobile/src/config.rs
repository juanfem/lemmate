//! Mobile configuration: the same `SetupRequest` the desktop shell writes, minus everything
//! the platform decides for you.
//!
//! There are no flags and no environment on a phone, so the file in the app's data directory
//! is the whole story, and the first-run setup screen (SPEC §14, shared with the desktop) is
//! the only thing that writes it. `vault_dir` is not a key: the vault lives inside the app's
//! own storage, at a path the shell computes, so a value in the file could only ever be wrong.

use std::path::{Path, PathBuf};

use anyhow::Context;
use lemmate_core::VaultId;
use serde::Deserialize;

/// The shape of `mobile.toml`.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    server_url: Option<String>,
    vault_id: Option<String>,
    ca_cert: Option<PathBuf>,
    token: Option<String>,
}

/// A complete, validated configuration.
#[derive(Debug, Clone)]
pub struct Config {
    pub vault_dir: PathBuf,
    pub server_url: String,
    pub vault_id: Option<VaultId>,
    pub ca_cert: Option<PathBuf>,
    pub token: Option<String>,
}

/// Where everything the app owns lives, derived once from Tauri's data directory.
#[derive(Debug, Clone)]
pub struct Paths {
    /// The vault folder the engine projects notes into.
    pub vault_dir: PathBuf,
    /// `mobile.toml`.
    pub config: PathBuf,
    /// Where the bundled web client is unpacked.
    pub web: PathBuf,
}

impl Paths {
    pub fn under(data_dir: &Path) -> Self {
        Self {
            vault_dir: data_dir.join("vault"),
            config: data_dir.join("mobile.toml"),
            web: data_dir.join("web"),
        }
    }
}

impl Config {
    /// Read the file, or `None` when it is missing, unreadable or has no server in it — all
    /// three mean the same thing to the shell: open the setup screen.
    pub fn load(paths: &Paths) -> Option<Self> {
        let file: FileConfig = std::fs::read_to_string(&paths.config)
            .ok()
            .and_then(|t| toml::from_str(&t).ok())
            .unwrap_or_default();
        let server_url = file.server_url.filter(|s| !s.trim().is_empty())?;
        Some(Self {
            vault_dir: paths.vault_dir.clone(),
            server_url: server_url.trim().trim_end_matches('/').to_owned(),
            vault_id: file.vault_id.as_deref().and_then(|v| v.parse().ok()),
            ca_cert: file.ca_cert,
            token: file.token,
        })
    }

    /// Write what the setup screen produced. `vault_dir` and `web_dir` are dropped on the
    /// floor: the form offers them because the desktop needs them, and on a phone they are
    /// not the user's to choose.
    pub fn write_setup(path: &Path, req: &lemmate_core::local::SetupRequest) -> anyhow::Result<()> {
        let mut table = toml::Table::new();
        table.insert("server_url".into(), toml::Value::String(req.server_url.clone()));
        for (key, value) in [("vault_id", req.vault_id.as_deref()), ("ca_cert", req.ca_cert.as_deref())] {
            if let Some(v) = value.map(str::trim).filter(|v| !v.is_empty()) {
                table.insert(key.into(), toml::Value::String(v.to_owned()));
            }
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, toml::to_string(&table)?)
            .with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_or_serverless_file_means_setup() {
        let dir = std::env::temp_dir().join(format!("lemmate-mobile-cfg-{}", std::process::id()));
        let paths = Paths::under(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(Config::load(&paths).is_none(), "no file at all");
        std::fs::write(&paths.config, "server_url = \"\"\n").unwrap();
        assert!(Config::load(&paths).is_none(), "a blank server is not a configuration");
        std::fs::write(&paths.config, "nonsense = [[[\n").unwrap();
        assert!(Config::load(&paths).is_none(), "an unparseable file must not crash the app");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn setup_writes_the_server_and_keeps_the_vault_out_of_it() {
        let dir = std::env::temp_dir().join(format!("lemmate-mobile-set-{}", std::process::id()));
        let paths = Paths::under(&dir);
        let req = lemmate_core::local::SetupRequest {
            // The form always sends one; on a phone it is ignored.
            vault_dir: "/whatever/the/form/said".into(),
            server_url: "https://notes.example.org".into(),
            vault_id: Some("  ".into()),
            ca_cert: None,
            email: None,
            password: None,
            register: false,
            invite: None,
        };
        Config::write_setup(&paths.config, &req).unwrap();
        let written = std::fs::read_to_string(&paths.config).unwrap();
        assert!(written.contains("notes.example.org"));
        assert!(!written.contains("vault_dir"), "the phone owns the vault path: {written}");
        assert!(!written.contains("vault_id"), "a blank vault id is not a vault id: {written}");

        let cfg = Config::load(&paths).expect("just written");
        assert_eq!(cfg.server_url, "https://notes.example.org");
        assert_eq!(cfg.vault_dir, paths.vault_dir);
        std::fs::remove_dir_all(&dir).ok();
    }
}

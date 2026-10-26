//! Which Lua filters a vault's renders may run, as this device knows it (SPEC §5.6): the
//! relay's half of approvals. The server keeps its own in its database; here, per vault, a file
//! under the configuration directory holds two lists —
//!
//! - `server`: the server's approvals as last fetched, so an approved filter still runs offline;
//! - `device`: approvals made on this device — for a standalone vault, or one whose server
//!   would not take the approval (a desktop signs in with an access token, and a token approves
//!   nothing upstream).
//!
//! Never in the vault: anything that can write a vault file — an editor, a synced device, an
//! import — could otherwise approve its own filter. Whatever can write the configuration
//! directory already runs code as this user.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::ids::VaultId;
use crate::store::FilterApproval;

/// One vault's approvals on this device.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceApprovals {
    #[serde(default)]
    pub server: Vec<FilterApproval>,
    #[serde(default)]
    pub device: Vec<FilterApproval>,
}

impl DeviceApprovals {
    /// Every hash a render may run: the server's and this device's.
    pub fn hashes(&self) -> std::collections::HashSet<String> {
        self.server.iter().chain(&self.device).map(|a| a.hash.clone()).collect()
    }
}

/// Where a relay keeps its approval files: one per vault, `<dir>/<vault>.json`.
#[derive(Debug, Clone)]
pub struct ApprovalFiles {
    dir: PathBuf,
}

impl ApprovalFiles {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// `filter-approvals/` under [`crate::paths::config_dir`].
    pub fn default_dir() -> Option<PathBuf> {
        crate::paths::config_dir().map(|d| d.join("filter-approvals"))
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn path(&self, vault: VaultId) -> PathBuf {
        self.dir.join(format!("{vault}.json"))
    }

    /// A vault's approvals; none when there is no file, or one that cannot be read — which
    /// runs fewer filters, never more.
    pub fn load(&self, vault: VaultId) -> DeviceApprovals {
        std::fs::read(self.path(vault)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    pub fn save(&self, vault: VaultId, approvals: &DeviceApprovals) -> std::io::Result<()> {
        let json = serde_json::to_vec_pretty(approvals).map_err(std::io::Error::other)?;
        crate::paths::write_private(&self.path(vault), &json)
    }

    /// Change a vault's approvals in place: load, `f`, save.
    pub fn update<T>(&self, vault: VaultId, f: impl FnOnce(&mut DeviceApprovals) -> T) -> std::io::Result<T> {
        let mut approvals = self.load(vault);
        let out = f(&mut approvals);
        self.save(vault, &approvals)?;
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approval(hash: &str) -> FilterApproval {
        FilterApproval {
            hash: hash.into(),
            path: "a.lua".into(),
            approved_by: "device".into(),
            approved_ms: 1,
        }
    }

    #[test]
    fn approvals_are_kept_per_vault() {
        let dir = tempfile::tempdir().unwrap();
        let files = ApprovalFiles::new(dir.path().join("filter-approvals"));
        let (a, b) = (VaultId::new(), VaultId::new());
        assert_eq!(files.load(a), DeviceApprovals::default(), "none yet");
        files.update(a, |x| x.device.push(approval("h1"))).unwrap();
        files.update(a, |x| x.server = vec![approval("h2")]).unwrap();
        assert_eq!(files.load(a).hashes(), ["h1".to_owned(), "h2".to_owned()].into());
        assert!(files.load(b).hashes().is_empty());
        std::fs::write(dir.path().join("filter-approvals").join(format!("{b}.json")), "not json").unwrap();
        assert!(files.load(b).hashes().is_empty(), "unreadable: nothing approved");
    }
}

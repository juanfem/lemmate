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
use crate::store::{FILTER_VERSIONS_KEPT, FilterApproval};

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

    /// Keep only the newest [`FILTER_VERSIONS_KEPT`] versions of each file in each list (both
    /// newest first), as the server does with its own.
    pub fn trim(&mut self) {
        for list in [&mut self.server, &mut self.device] {
            let mut seen = std::collections::HashMap::<String, usize>::new();
            list.retain(|a| {
                let n = seen.entry(a.path.clone()).or_default();
                *n += 1;
                *n <= FILTER_VERSIONS_KEPT
            });
        }
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
    /// runs fewer filters, never more. Trimmed to [`FILTER_VERSIONS_KEPT`] per file, for a file
    /// written before there was a limit.
    pub fn load(&self, vault: VaultId) -> DeviceApprovals {
        let mut approvals: DeviceApprovals = std::fs::read(self.path(vault))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        approvals.trim();
        approvals
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
        approval_at(hash, "a.lua")
    }

    fn approval_at(hash: &str, path: &str) -> FilterApproval {
        FilterApproval { hash: hash.into(), path: path.into(), approved_by: "device".into(), approved_ms: 1 }
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

    #[test]
    fn a_file_keeps_its_last_two_versions_in_each_list() {
        let mut x = DeviceApprovals { server: vec![approval("s1")], ..Default::default() };
        for h in ["v1", "v2", "v3"] {
            x.device.insert(0, approval(h));
            x.trim();
        }
        x.device.insert(1, approval_at("b1", "b.lua"));
        x.trim();
        let device: Vec<&str> = x.device.iter().map(|a| a.hash.as_str()).collect();
        assert_eq!(device, ["v3", "b1", "v2"], "v1 is gone, b.lua is not counted");
        assert_eq!(x.server, [approval("s1")], "the server's copy counts on its own");

        // A file written before the limit is trimmed as it is read.
        let dir = tempfile::tempdir().unwrap();
        let files = ApprovalFiles::new(dir.path());
        let vault = VaultId::new();
        let old = DeviceApprovals { device: ["v3", "v2", "v1"].map(approval).to_vec(), ..Default::default() };
        files.save(vault, &old).unwrap();
        assert_eq!(files.load(vault).hashes(), ["v3".to_owned(), "v2".to_owned()].into());
    }
}

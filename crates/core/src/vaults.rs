//! Which vaults a native client holds, and in which folders (SPEC §9, §14).
//!
//! A desktop client shows **every vault the account can read**, the way the web client does, and
//! each one is a folder of markdown under a root the user picked:
//!
//! ```text
//! <root>/
//!   Work/            ← a vault named "Work"
//!     .lemmate/      ← its sidecar: local.db, attachments
//!   vault-3f9c2a/    ← a vault with no name yet, folder named after its id
//! ```
//!
//! The binding between folder and vault is the sidecar, never the folder name: rename the
//! folder and it still belongs to the same vault. That is what lets [`rehome`] give a folder
//! the vault's name once the vault doc has one, and what lets a user move things about.

use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::ids::VaultId;

/// One vault folder under the root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultFolder {
    pub dir: PathBuf,
    /// The vault it belongs to, or `None` for a folder that does not exist yet and whose vault
    /// the engine will create.
    pub id: Option<VaultId>,
}

/// The folder a vault with no name of its own gets: short enough to read, long enough not to
/// collide. The same six characters the UI falls back to when it labels an unnamed vault.
pub fn default_folder_name(id: VaultId) -> String {
    let s = id.to_string();
    format!("vault-{}", &s[s.len() - 6..])
}

/// A vault name as a folder name: path separators and the characters Windows refuses become
/// `-`, the result is trimmed and capped. Empty (or all-punctuation) names give `None`, and the
/// caller falls back to [`default_folder_name`].
pub fn folder_name_for(name: &str) -> Option<String> {
    let cleaned: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '-',
            c if c.is_control() => '-',
            c => c,
        })
        .collect();
    // Leading dots hide the folder on unix and would collide with `.lemmate`; trailing dots and
    // spaces are silently dropped by Windows.
    let trimmed = cleaned.trim().trim_matches(['.', ' ']).trim();
    if trimmed.is_empty() {
        return None;
    }
    let capped: String = trimmed.chars().take(64).collect();
    Some(capped.trim_end().to_owned())
}

/// The vault folders directly under `root`, in path order.
///
/// A folder counts when it holds a sidecar naming a vault; anything else under the root —
/// downloads, a stray checkout — is left alone. A root that does not exist yet has none.
pub fn on_disk(root: &Path) -> Vec<VaultFolder> {
    let Ok(entries) = std::fs::read_dir(root) else { return Vec::new() };
    let mut found: Vec<VaultFolder> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .filter_map(|dir| match crate::client::vault_id_at(&dir) {
            Ok(Some(id)) => Some(VaultFolder { dir, id: Some(id) }),
            _ => None,
        })
        .collect();
    found.sort_by(|a, b| a.dir.cmp(&b.dir));
    found
}

/// A free path under `root` for `name`, adding `-2`, `-3`… past anything already there or
/// already planned for this run.
fn free_path(root: &Path, name: &str, taken: &[PathBuf]) -> PathBuf {
    let free = |p: &PathBuf| !p.exists() && !taken.contains(p);
    let first = root.join(name);
    if free(&first) {
        return first;
    }
    (2..1000).map(|n| root.join(format!("{name}-{n}"))).find(free).unwrap_or(first)
}

/// Every vault to open: the folders already on disk, plus a new folder for each `remote` vault
/// that has none yet.
///
/// `remote` is what the server says the account can read; an empty list means the server was
/// unreachable or the account has nothing there, and either way what is on disk still opens —
/// offline is the point. With nothing on either side this returns one folder for a brand-new
/// vault, so a first run has somewhere to write.
pub fn plan(root: &Path, remote: &[VaultId]) -> Vec<VaultFolder> {
    let mut folders = on_disk(root);
    let mut taken: Vec<PathBuf> = folders.iter().map(|f| f.dir.clone()).collect();
    for id in remote {
        if folders.iter().any(|f| f.id == Some(*id)) {
            continue;
        }
        let dir = free_path(root, &default_folder_name(*id), &taken);
        taken.push(dir.clone());
        folders.push(VaultFolder { dir, id: Some(*id) });
    }
    if folders.is_empty() {
        // First run with no server to ask: one vault, in a folder named after nothing in
        // particular, because its id does not exist yet.
        folders.push(VaultFolder { dir: root.join("notes"), id: None });
    }
    folders
}

/// Rename folders that are still named after their vault's id to the name the vault has since
/// been given, and report what moved.
///
/// Vault names live in the vault doc, so they only arrive after the first sync — by which time
/// the folder exists. Doing this before any engine opens means nothing holds the directory, and
/// a folder the user renamed themselves is never touched: only the exact `vault-xxxxxx` default
/// is a candidate.
pub fn rehome(root: &Path) -> Vec<(PathBuf, PathBuf)> {
    let mut moved = Vec::new();
    for folder in on_disk(root) {
        let Some(id) = folder.id else { continue };
        if folder.dir.file_name().and_then(|n| n.to_str()) != Some(default_folder_name(id).as_str()) {
            continue;
        }
        let Ok(Some(name)) = crate::client::vault_name_at(&folder.dir) else { continue };
        let Some(wanted) = folder_name_for(&name) else { continue };
        let target = free_path(root, &wanted, &[]);
        if std::fs::rename(&folder.dir, &target).is_ok() {
            moved.push((folder.dir, target));
        }
    }
    moved
}

/// The vaults the account can read, from the server's REST API (SPEC §13.1).
///
/// Blocking, and best-effort by design: the caller treats a failure as "no answer" and opens
/// whatever is already on disk.
pub fn remote_ids(server_url: &str, token: Option<&str>, ca_cert: Option<&Path>) -> Result<Vec<VaultId>> {
    #[derive(serde::Deserialize)]
    struct Row {
        id: String,
    }
    let agent = crate::tls::http_agent(ca_cert)?;
    let base = server_url.trim_end_matches('/');
    let mut req = agent.get(format!("{base}/api/v1/vaults"));
    if let Some(t) = token {
        req = req.header("authorization", format!("Bearer {t}"));
    }
    let mut resp = req.call().map_err(|e| crate::error::Error::Sync(e.to_string()))?;
    let text = resp
        .body_mut()
        .with_config()
        .limit(4 * 1024 * 1024)
        .read_to_string()
        .map_err(|e| crate::error::Error::Sync(e.to_string()))?;
    let rows: Vec<Row> = serde_json::from_str(&text).map_err(|e| crate::error::Error::Sync(e.to_string()))?;
    Ok(rows.iter().filter_map(|r| r.id.parse().ok()).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_names_are_safe_and_fall_back_to_the_id() {
        let id: VaultId = "01ARZ3NDEKTSV4RRFFQ69G5FAV".parse().unwrap();
        assert_eq!(default_folder_name(id), "vault-9G5FAV");
        assert_eq!(folder_name_for("Work"), Some("Work".into()));
        assert_eq!(folder_name_for("Work/2026"), Some("Work-2026".into()));
        assert_eq!(folder_name_for("  spaced  "), Some("spaced".into()));
        assert_eq!(folder_name_for(".hidden"), Some("hidden".into()));
        assert_eq!(folder_name_for("   "), None);
        assert_eq!(folder_name_for("..."), None);
        assert_eq!(folder_name_for(&"x".repeat(200)).unwrap().chars().count(), 64);
    }

    #[test]
    fn a_first_run_with_nothing_anywhere_plans_one_vault() {
        let root = tempfile::tempdir().unwrap();
        let plan = plan(root.path(), &[]);
        assert_eq!(plan, vec![VaultFolder { dir: root.path().join("notes"), id: None }]);
    }

    #[test]
    fn remote_vaults_get_a_folder_each_and_known_ones_are_left_alone() {
        let root = tempfile::tempdir().unwrap();
        let a = VaultId::new();
        let b = VaultId::new();
        // `a` is already here, under a name the user chose.
        let mine = root.path().join("My notes");
        std::fs::create_dir_all(mine.join(".lemmate")).unwrap();
        let mut store = crate::store::Store::open(mine.join(".lemmate").join("local.db")).unwrap();
        store.meta_set("vault_id", &a.to_string()).unwrap();
        drop(store);

        let plan = plan(root.path(), &[a, b]);
        assert_eq!(plan.len(), 2, "the known vault keeps its folder, the new one gets its own");
        assert_eq!(plan[0], VaultFolder { dir: mine, id: Some(a) });
        assert_eq!(plan[1], VaultFolder { dir: root.path().join(default_folder_name(b)), id: Some(b) });
    }

    #[test]
    fn rehome_renames_only_id_named_folders_and_only_once_named() {
        let root = tempfile::tempdir().unwrap();
        let id = VaultId::new();
        let dir = root.path().join(default_folder_name(id));
        std::fs::create_dir_all(dir.join(".lemmate")).unwrap();
        let mut store = crate::store::Store::open(dir.join(".lemmate").join("local.db")).unwrap();
        store.meta_set("vault_id", &id.to_string()).unwrap();
        drop(store);

        assert!(rehome(root.path()).is_empty(), "no name in the vault doc yet");

        let doc = crate::vault_doc::VaultDoc::new();
        let update = doc.set_name("Reading list");
        let mut store = crate::store::Store::open(dir.join(".lemmate").join("local.db")).unwrap();
        store.append_update(crate::ids::DocId::Vault(id), &update, None).unwrap();
        drop(store);

        let moved = rehome(root.path());
        assert_eq!(moved.len(), 1);
        assert_eq!(moved[0].1, root.path().join("Reading list"));
        assert!(root.path().join("Reading list").join(".lemmate").is_dir());
        assert!(rehome(root.path()).is_empty(), "a folder the user could have named is left alone");
    }
}

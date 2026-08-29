//! Content-addressed attachment storage (SPEC §4.4, §6.1, §7). Files are identified by the
//! blake3 hash of their bytes; filenames are hints kept alongside for projection.

use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::ids::VaultId;

/// Upper bound accepted by the server and requested by clients.
pub const MAX_ATTACHMENT_BYTES: u64 = 64 * 1024 * 1024;

pub fn hash_bytes(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

pub fn is_valid_hash(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

pub fn mime_for_path(path: &str) -> String {
    mime_guess::from_path(path).first_or_octet_stream().essence_str().to_owned()
}

/// Server-side blob store: `<root>/<vault>/<hh>/<hash>`.
#[derive(Debug, Clone)]
pub struct AttachmentStore {
    root: PathBuf,
}

impl AttachmentStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn path_for(&self, vault: VaultId, hash: &str) -> Result<PathBuf> {
        if !is_valid_hash(hash) {
            return Err(Error::Sync(format!("invalid attachment hash {hash:?}")));
        }
        Ok(self.root.join(vault.to_string()).join(&hash[..2]).join(hash))
    }

    pub fn exists(&self, vault: VaultId, hash: &str) -> bool {
        self.path_for(vault, hash).is_ok_and(|p| p.is_file())
    }

    /// Store bytes; returns their hash and whether the blob was newly written.
    pub fn put(&self, vault: VaultId, bytes: &[u8]) -> Result<(String, bool)> {
        let hash = hash_bytes(bytes);
        let target = self.path_for(vault, &hash)?;
        if target.is_file() {
            return Ok((hash, false));
        }
        let parent = target.parent().expect("hash path has a parent");
        fs::create_dir_all(parent)?;
        let tmp = parent.join(format!(".{hash}.tmp"));
        fs::write(&tmp, bytes)?;
        fs::rename(&tmp, &target)?;
        Ok((hash, true))
    }

    pub fn get(&self, vault: VaultId, hash: &str) -> Result<Option<Vec<u8>>> {
        let path = self.path_for(vault, hash)?;
        match fs::read(&path) {
            Ok(b) => Ok(Some(b)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn put_get_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let store = AttachmentStore::new(dir.path());
        let v = VaultId::new();
        let (h, created) = store.put(v, b"hello").unwrap();
        assert!(created && is_valid_hash(&h));
        assert_eq!(store.put(v, b"hello").unwrap(), (h.clone(), false));
        assert_eq!(store.get(v, &h).unwrap().as_deref(), Some(&b"hello"[..]));
        assert_eq!(store.get(VaultId::new(), &h).unwrap(), None);
        assert!(store.path_for(v, "../x").is_err());
        assert!(store.exists(v, &h));
        assert_eq!(mime_for_path("a/b.png"), "image/png");
        assert_eq!(mime_for_path("weird.zzz"), "application/octet-stream");
    }
}

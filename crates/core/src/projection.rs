//! The on-disk view of a vault (SPEC §6.3): atomic writes of note text, and ingestion of
//! external edits by diffing the file against the *last projected* text so the change composes
//! with concurrent CRDT edits.

use std::fs;
use std::path::{Component, Path, PathBuf};

use crate::doc::NoteDoc;
use crate::error::{Error, Result};
use crate::{SIDECAR_DIR, diff};

pub const NOTE_EXTENSIONS: &[&str] = &["md", "qmd"];

#[derive(Debug, Clone)]
pub struct Projection {
    root: PathBuf,
}

impl Projection {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn sidecar_dir(&self) -> PathBuf {
        self.root.join(SIDECAR_DIR)
    }

    /// Absolute path for a vault-relative note path; rejects `..` and absolute inputs.
    pub fn resolve(&self, rel: &str) -> Result<PathBuf> {
        let p = Path::new(rel);
        if p.is_absolute() || p.components().any(|c| matches!(c, Component::ParentDir | Component::Prefix(_)))
        {
            return Err(Error::PathEscape(rel.to_owned()));
        }
        Ok(self.root.join(p))
    }

    /// Is this path something the projection ignores (sidecar, hidden dirs, temp files)?
    pub fn is_ignored(&self, path: &Path) -> bool {
        let rel = path.strip_prefix(&self.root).unwrap_or(path);
        rel.components().any(|c| match c {
            Component::Normal(s) => {
                let s = s.to_string_lossy();
                s == SIDECAR_DIR || (s.starts_with('.') && s != ".") || s.ends_with(".tmp")
            }
            _ => false,
        })
    }

    pub fn is_note_path(path: &Path) -> bool {
        path.extension().and_then(|e| e.to_str()).is_some_and(|e| NOTE_EXTENSIONS.contains(&e))
    }

    /// Atomically write `text` to `rel` (temp file in the same directory, then rename).
    pub fn write(&self, rel: &str, text: &str) -> Result<()> {
        let target = self.resolve(rel)?;
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        let tmp = target
            .with_extension(format!("{}.tmp", target.extension().and_then(|e| e.to_str()).unwrap_or("md")));
        fs::write(&tmp, text)?;
        fs::rename(&tmp, &target)?;
        Ok(())
    }

    pub fn read(&self, rel: &str) -> Result<String> {
        Ok(fs::read_to_string(self.resolve(rel)?)?)
    }

    pub fn read_bytes(&self, rel: &str) -> Result<Vec<u8>> {
        Ok(fs::read(self.resolve(rel)?)?)
    }

    /// Atomically write binary content (attachments).
    pub fn write_bytes(&self, rel: &str, bytes: &[u8]) -> Result<()> {
        let target = self.resolve(rel)?;
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        let name = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let tmp = target.with_file_name(format!(".{name}.tmp"));
        fs::write(&tmp, bytes)?;
        fs::rename(&tmp, &target)?;
        Ok(())
    }

    /// Resolve `target` relative to the directory of `from` (a vault-relative file path),
    /// collapsing `.`/`..`, and reject anything that escapes the vault.
    pub fn normalize_relative(from: &str, target: &str) -> Option<String> {
        let base = match from.rsplit_once('/') {
            Some((dir, _)) => dir,
            None => "",
        };
        let joined = if target.starts_with('/') {
            target.trim_start_matches('/').to_owned()
        } else if base.is_empty() {
            target.to_owned()
        } else {
            format!("{base}/{target}")
        };
        let mut parts: Vec<&str> = Vec::new();
        for seg in joined.split('/') {
            match seg {
                "" | "." => {}
                ".." => {
                    parts.pop()?;
                }
                s => parts.push(s),
            }
        }
        if parts.is_empty() { None } else { Some(parts.join("/")) }
    }

    /// Vault-relative paths of all non-note regular files (attachment candidates), sorted.
    pub fn walk_files(&self) -> Result<Vec<String>> {
        let mut out = Vec::new();
        self.walk_files_into(&self.root, &mut out)?;
        out.sort();
        Ok(out)
    }

    fn walk_files_into(&self, dir: &Path, out: &mut Vec<String>) -> Result<()> {
        for entry in fs::read_dir(dir)? {
            let path = entry?.path();
            if self.is_ignored(&path) {
                continue;
            }
            if path.is_dir() {
                self.walk_files_into(&path, out)?;
            } else if path.is_file() && !Self::is_note_path(&path) {
                let rel = path.strip_prefix(&self.root).unwrap_or(&path);
                out.push(rel.to_string_lossy().replace('\\', "/"));
            }
        }
        Ok(())
    }

    pub fn remove(&self, rel: &str) -> Result<()> {
        match fs::remove_file(self.resolve(rel)?) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    /// Vault-relative paths of all note files, sorted, ignoring the sidecar and hidden entries.
    pub fn walk_notes(&self) -> Result<Vec<String>> {
        let mut out = Vec::new();
        self.walk_into(&self.root, &mut out)?;
        out.sort();
        Ok(out)
    }

    fn walk_into(&self, dir: &Path, out: &mut Vec<String>) -> Result<()> {
        for entry in fs::read_dir(dir)? {
            let path = entry?.path();
            if self.is_ignored(&path) {
                continue;
            }
            if path.is_dir() {
                self.walk_into(&path, out)?;
            } else if Self::is_note_path(&path) {
                let rel = path.strip_prefix(&self.root).unwrap_or(&path);
                out.push(rel.to_string_lossy().replace('\\', "/"));
            }
        }
        Ok(())
    }
}

/// Apply an external edit to a doc. `last_projected` is the text this device last wrote to or
/// read from disk for this doc; `on_disk` is the current file content. Returns the CRDT update
/// (empty if the file matches what we already knew).
pub fn ingest_external_edit(doc: &NoteDoc, last_projected: &str, on_disk: &str) -> Vec<u8> {
    let ops = diff::text_ops(last_projected, on_disk);
    if ops.is_empty() {
        return Vec::new();
    }
    // The ops are expressed against `last_projected`. If the doc has since diverged (a remote
    // edit arrived between projection and this read), positions would be off; re-derive them
    // against the doc's current text by replaying the external change through a 3-way merge:
    // current = doc.text(); we want current ⊕ (on_disk − last_projected).
    let current = doc.text();
    if current == last_projected {
        return doc.apply_ops(&ops);
    }
    // Simple 3-way: apply the external ops to a scratch replica that starts at `last_projected`,
    // then merge both replicas' updates. Both branches share the projected base, so yrs merges
    // them positionally rather than by re-diffing.
    let base = NoteDoc::new();
    base.set_text(last_projected);
    let base_full = base.encode_full();
    let external = NoteDoc::from_updates([base_full.as_slice()]).expect("valid snapshot");
    let ext_update = external.apply_ops(&ops);
    // Bring the doc to the same base by seeding a fresh doc with base + doc's delta vs base.
    let ours = NoteDoc::from_updates([base_full.as_slice()]).expect("valid snapshot");
    ours.apply_ops(&diff::text_ops(last_projected, &current));
    ours.apply_update(&ext_update).expect("update applies");
    doc.set_text(&ours.text())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_read_walk_and_ignore() {
        let dir = tempfile::tempdir().unwrap();
        let p = Projection::new(dir.path());
        p.write("Daily/2026-08-29.md", "hello").unwrap();
        p.write("Projects/x.qmd", "---\ntitle: x\n---\n").unwrap();
        fs::create_dir_all(p.sidecar_dir()).unwrap();
        fs::write(p.sidecar_dir().join("ignored.md"), "no").unwrap();
        fs::write(dir.path().join("notes.txt"), "not a note").unwrap();
        assert_eq!(p.read("Daily/2026-08-29.md").unwrap(), "hello");
        assert_eq!(p.walk_notes().unwrap(), vec!["Daily/2026-08-29.md", "Projects/x.qmd"]);
        assert!(p.is_ignored(&p.sidecar_dir().join("local.db")));
        assert!(p.resolve("../escape.md").is_err());
        assert!(p.resolve("/abs.md").is_err());
        p.remove("Daily/2026-08-29.md").unwrap();
        p.remove("Daily/2026-08-29.md").unwrap(); // idempotent
    }

    #[test]
    fn relative_paths_and_files() {
        assert_eq!(
            Projection::normalize_relative("Projects/a.md", "../attachments/x.png").as_deref(),
            Some("attachments/x.png")
        );
        assert_eq!(Projection::normalize_relative("a.md", "img/./x.png").as_deref(), Some("img/x.png"));
        assert_eq!(Projection::normalize_relative("a.md", "/root.png").as_deref(), Some("root.png"));
        assert_eq!(Projection::normalize_relative("a.md", "../escape.png"), None);
        assert_eq!(Projection::normalize_relative("a.md", ""), None);

        let dir = tempfile::tempdir().unwrap();
        let p = Projection::new(dir.path());
        p.write("n.md", "x").unwrap();
        p.write_bytes("attachments/one.bin", &[1, 2, 3]).unwrap();
        p.write_bytes("deep/two.bin", &[4]).unwrap();
        fs::create_dir_all(p.sidecar_dir()).unwrap();
        fs::write(p.sidecar_dir().join("local.db"), "x").unwrap();
        assert_eq!(p.walk_files().unwrap(), vec!["attachments/one.bin", "deep/two.bin"]);
        assert_eq!(p.read_bytes("attachments/one.bin").unwrap(), vec![1, 2, 3]);
    }

    #[test]
    fn external_edit_merges_with_concurrent_remote_edit() {
        let doc = NoteDoc::new();
        doc.set_text("# Title\n\nbody\n");
        let projected = doc.text();
        // Remote edit arrives after projection.
        doc.set_text("# Title (remote)\n\nbody\n");
        // Meanwhile vim appended a line to the file that still reflects `projected`.
        let on_disk = "# Title\n\nbody\nfrom vim\n";
        let update = ingest_external_edit(&doc, &projected, on_disk);
        assert!(!update.is_empty());
        assert_eq!(doc.text(), "# Title (remote)\n\nbody\nfrom vim\n");
    }

    #[test]
    fn unchanged_file_is_a_noop() {
        let doc = NoteDoc::new();
        doc.set_text("x");
        assert!(ingest_external_edit(&doc, "x", "x").is_empty());
    }
}

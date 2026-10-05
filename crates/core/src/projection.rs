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
    /// `root` with every symlink resolved, when that differs: macOS FSEvents reports
    /// `/private/var/…` for a vault under `/var/…`, and a path that does not start with the root
    /// we know would be dropped as foreign.
    canonical: Option<PathBuf>,
}

impl Projection {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        let canonical = root.canonicalize().ok().filter(|c| *c != root);
        Self { root, canonical }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `abs` relative to the vault root, under either spelling of the root.
    pub fn relative<'a>(&self, abs: &'a Path) -> Option<&'a Path> {
        abs.strip_prefix(&self.root)
            .ok()
            .or_else(|| self.canonical.as_ref().and_then(|c| abs.strip_prefix(c).ok()))
    }

    pub fn sidecar_dir(&self) -> PathBuf {
        self.root.join(SIDECAR_DIR)
    }

    /// Absolute path for a vault-relative path, which must pass [`check_path`]. Every path that
    /// reaches the disk goes through here, and most of them came from another replica.
    pub fn resolve(&self, rel: &str) -> Result<PathBuf> {
        check_path(rel).map_err(|why| Error::PathEscape(format!("{rel}: {why}")))?;
        Ok(self.root.join(rel))
    }

    /// Is this path something the projection ignores (sidecar, hidden dirs, temp files)?
    pub fn is_ignored(&self, path: &Path) -> bool {
        let rel = self.relative(path).unwrap_or(path);
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
            let entry = entry?;
            let path = entry.path();
            if self.is_ignored(&path) {
                continue;
            }
            // A symlinked folder is not descended into: it can loop, or lead out of the vault.
            if entry.file_type()?.is_dir() {
                self.walk_files_into(&path, out)?;
            } else if path.is_file() && !Self::is_note_path(&path) {
                let rel = self.relative(&path).unwrap_or(&path);
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
            let entry = entry?;
            let path = entry.path();
            if self.is_ignored(&path) {
                continue;
            }
            // As in `walk_files_into`: a symlinked folder is not followed.
            let kind = entry.file_type()?;
            if kind.is_dir() {
                self.walk_into(&path, out)?;
            } else if kind.is_symlink() && path.is_dir() {
                continue;
            } else if Self::is_note_path(&path) {
                let rel = self.relative(&path).unwrap_or(&path);
                out.push(rel.to_string_lossy().replace('\\', "/"));
            }
        }
        Ok(())
    }
}

/// Why a vault-relative path may not be written, or `Ok` when it may.
///
/// Paths arrive from other replicas, so this is the line between "a note" and "a file anywhere
/// this process can write": nothing absolute or climbing out (`..`), nothing hidden — which
/// covers the sidecar (`.lemmate/`), `.git/` and every other dot-directory a tool would trust —
/// and nothing that a Windows or macOS replica of the same vault could not hold: the characters
/// Windows forbids, its reserved device names (`CON`, `aux.md`, `COM1.txt`, …), and names ending
/// in a dot or a space. Those are refused on *every* platform, so all replicas agree on which
/// notes have a file; a note refused here stays in the vault doc and keeps syncing, it simply
/// has no file on this disk.
pub fn check_path(rel: &str) -> std::result::Result<(), &'static str> {
    if rel.is_empty() {
        return Err("empty path");
    }
    if rel.contains('\\') {
        return Err("backslash in path");
    }
    if rel.starts_with('/')
        || Path::new(rel).components().any(|c| matches!(c, Component::Prefix(_) | Component::RootDir))
    {
        return Err("absolute path");
    }
    for seg in rel.split('/') {
        match seg {
            "" => return Err("empty path segment"),
            "." | ".." => return Err("relative path segment"),
            _ => {}
        }
        if seg.starts_with('.') {
            return Err("hidden path segment");
        }
        if seg.chars().any(|c| c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*')) {
            return Err("character not allowed in a file name");
        }
        if seg.ends_with('.') || seg.ends_with(' ') {
            return Err("file name ends in a dot or a space");
        }
        let stem = seg.split('.').next().unwrap_or(seg).trim_end().to_ascii_uppercase();
        let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || ((stem.starts_with("COM") || stem.starts_with("LPT"))
                && stem.len() == 4
                && stem.as_bytes()[3].is_ascii_digit()
                && stem.as_bytes()[3] != b'0');
        if reserved {
            return Err("reserved file name");
        }
    }
    Ok(())
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

    #[cfg(unix)]
    #[test]
    fn symlinked_folders_are_not_walked() {
        let dir = tempfile::tempdir().unwrap();
        let (root, outside) = (dir.path().join("vault"), dir.path().join("outside"));
        let p = Projection::new(&root);
        p.write("a.md", "a").unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("secret.md"), "s").unwrap();
        fs::write(outside.join("secret.bin"), "s").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("linked")).unwrap();
        std::os::unix::fs::symlink(&root, root.join("loop")).unwrap();
        assert_eq!(p.walk_notes().unwrap(), vec!["a.md"]);
        assert!(p.walk_files().unwrap().is_empty());
    }

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
        assert!(p.resolve("Daily/2026-08-29.md").is_ok());
        p.remove("Daily/2026-08-29.md").unwrap();
        p.remove("Daily/2026-08-29.md").unwrap(); // idempotent
    }

    /// Everything another replica can name, against what may be written here (SPEC §6.3).
    #[test]
    fn hostile_and_unportable_paths_are_refused() {
        for bad in [
            "",
            "/abs.md",
            "../up.md",
            "a/../../up.md",
            "a/./b.md",
            "a//b.md",
            "a/",
            ".lemmate/local.db",
            ".lemmate/evil.md",
            ".git/config",
            "notes/.git/hooks/pre-commit",
            ".hidden.md",
            "a\\b.md",
            "nul\0byte.md",
            "tab\there.md",
            "What?.md",
            "a:b.md",
            "star*.md",
            "pipe|.md",
            "<x>.md",
            "quote\".md",
            "CON",
            "con.md",
            "dir/Aux.txt",
            "COM1.md",
            "lpt9.tar.gz",
            "NUL .md",
            "trailing.",
            "trailing space ",
            "dir./x.md",
        ] {
            assert!(check_path(bad).is_err(), "{bad:?} must be refused");
        }
        for good in [
            "a.md",
            "Daily/2026-08-29.md",
            "v1..2.md",
            "a.b.c.md",
            "COM0.md",
            "COM10.md",
            "console.md",
            "Lpt.md",
            "Ünïcödé/naïve café.md",
            "with space/x y.md",
            "_quarto.yml",
        ] {
            assert!(check_path(good).is_ok(), "{good:?} must be allowed: {:?}", check_path(good));
        }
        let dir = tempfile::tempdir().unwrap();
        let p = Projection::new(dir.path());
        assert!(p.write(".lemmate/evil.md", "x").is_err());
        assert!(p.write_bytes(".git/config", b"x").is_err());
        assert!(!dir.path().join(".git").exists() && !dir.path().join(".lemmate").exists());
    }

    /// FSEvents names the canonical path; the vault may have been opened through a symlink.
    #[cfg(unix)]
    #[test]
    fn events_under_the_canonical_root_are_relative_too() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        fs::create_dir_all(&real).unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let p = Projection::new(&link);
        let canon = real.canonicalize().unwrap().join("n.md");
        assert_eq!(p.relative(&canon), Some(Path::new("n.md")));
        assert_eq!(p.relative(&link.join("n.md")), Some(Path::new("n.md")));
        assert!(!p.is_ignored(&canon));
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

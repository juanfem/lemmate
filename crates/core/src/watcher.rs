//! Filesystem watcher for a projected vault (SPEC §6.3). Emits coarse events for every
//! non-ignored file (notes and attachments alike); debouncing, classification, and rename
//! detection (content-hash within 2 s) are the caller's job in the sync loop.

use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use crate::error::Result;
use crate::projection::Projection;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FsEvent {
    Created(PathBuf),
    Modified(PathBuf),
    Removed(PathBuf),
}

pub struct VaultWatcher {
    _inner: RecommendedWatcher,
}

impl VaultWatcher {
    /// Start watching `projection.root()` recursively, sending note-file events to `tx`.
    pub fn start(projection: Projection, tx: Sender<FsEvent>) -> Result<Self> {
        let root = projection.root().to_path_buf();
        let mut inner = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            let Ok(event) = res else { return };
            let kind = match event.kind {
                EventKind::Create(_) => FsEvent::Created as fn(PathBuf) -> FsEvent,
                EventKind::Modify(_) => FsEvent::Modified,
                EventKind::Remove(_) => FsEvent::Removed,
                _ => return,
            };
            for path in event.paths {
                if projection.is_ignored(&path) || path.is_dir() {
                    continue;
                }
                let _ = tx.send(kind(path));
            }
        })?;
        inner.watch(Path::new(&root), RecursiveMode::Recursive)?;
        Ok(Self { _inner: inner })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn reports_note_changes_and_ignores_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        let p = Projection::new(dir.path());
        std::fs::create_dir_all(p.sidecar_dir()).unwrap();
        let (tx, rx) = mpsc::channel();
        let _w = VaultWatcher::start(p.clone(), tx).unwrap();
        std::thread::sleep(Duration::from_millis(200));

        std::fs::write(p.sidecar_dir().join("local.db"), "x").unwrap();
        std::fs::write(dir.path().join(".hidden.txt"), "x").unwrap();
        p.write("note.md", "hello").unwrap();

        let mut saw_note = false;
        while let Ok(ev) = rx.recv_timeout(Duration::from_secs(3)) {
            let path = match &ev {
                FsEvent::Created(p) | FsEvent::Modified(p) | FsEvent::Removed(p) => p,
            };
            assert!(path.ends_with("note.md"), "unexpected event {ev:?}");
            saw_note = true;
            if matches!(ev, FsEvent::Created(_) | FsEvent::Modified(_)) {
                break;
            }
        }
        assert!(saw_note, "no event for note.md");
    }
}

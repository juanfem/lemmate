//! The vault-level CRDT document (SPEC §4.3): note id → path. Renames and moves are map writes,
//! so they merge across offline devices; a concurrent rename of one note resolves LWW per entry.

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use yrs::updates::decoder::Decode;
use yrs::{Any, Array, ArrayRef, Doc, Map, MapRef, Out, ReadTxn, StateVector, Transact, Update};

use crate::daily::DailySettings;
use crate::error::{Error, Result};
use crate::ids::NoteId;

pub const NOTES_FIELD: &str = "notes";
/// Vault-relative attachment path → blake3 hash of its content (SPEC §6.3, §7).
pub const ATTACHMENTS_FIELD: &str = "attachments";
/// Ordered bookmark list, shared by every replica (SPEC §4.3, §9). The web client owns this
/// list; Rust only ever appends to it, on import.
pub const BOOKMARKS_FIELD: &str = "bookmarks";
/// Vault-level settings shared by every replica: `name`, which labels the vault's folder on a
/// native client, and the daily-note settings ([`VaultDoc::daily`]).
pub const META_FIELD: &str = "meta";
/// Attachment paths kept whether or not a note uses them: files put in the vault through the
/// file manager rather than pasted into a note (path → `true`). Everything else in
/// [`ATTACHMENTS_FIELD`] lives only as long as some note depends on it.
pub const KEPT_FIELD: &str = "kept";

/// One entry of the bookmark list. `kind` is `note`, `folder`, `search` or `heading`; the
/// importer only produces `note`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bookmark {
    pub kind: String,
    /// Vault-relative path of the bookmarked note.
    pub target: String,
    /// Display label: the bookmark's own title, else the file stem.
    pub label: String,
}

pub struct VaultDoc {
    doc: Doc,
    notes: MapRef,
    attachments: MapRef,
    bookmarks: ArrayRef,
    meta: MapRef,
    kept: MapRef,
}

impl Default for VaultDoc {
    fn default() -> Self {
        Self::new()
    }
}

impl VaultDoc {
    pub fn new() -> Self {
        let doc = Doc::new();
        let notes = doc.get_or_insert_map(NOTES_FIELD);
        let attachments = doc.get_or_insert_map(ATTACHMENTS_FIELD);
        let bookmarks = doc.get_or_insert_array(BOOKMARKS_FIELD);
        let meta = doc.get_or_insert_map(META_FIELD);
        let kept = doc.get_or_insert_map(KEPT_FIELD);
        Self { doc, notes, attachments, bookmarks, meta, kept }
    }

    pub fn from_updates<'a>(updates: impl IntoIterator<Item = &'a [u8]>) -> Result<Self> {
        let d = Self::new();
        for u in updates {
            d.apply_update(u)?;
        }
        Ok(d)
    }

    pub fn state_vector(&self) -> StateVector {
        self.doc.transact().state_vector()
    }

    pub fn encode_full(&self) -> Vec<u8> {
        self.doc.transact().encode_state_as_update_v1(&StateVector::default())
    }

    pub fn diff_since(&self, sv: &StateVector) -> Vec<u8> {
        self.doc.transact().encode_diff_v1(sv)
    }

    /// Apply a v1 update; returns whether it changed anything.
    pub fn apply_update(&self, update: &[u8]) -> Result<bool> {
        let update = Update::decode_v1(update).map_err(|e| Error::Crdt(e.to_string()))?;
        let mut txn = self.doc.transact_mut();
        txn.apply_update(update).map_err(|e| Error::Crdt(e.to_string()))?;
        // Insertions advance the state vector; pure deletions only touch the delete set.
        Ok(txn.after_state() != txn.before_state() || !txn.delete_set().is_empty())
    }

    pub fn path_of(&self, id: NoteId) -> Option<String> {
        let txn = self.doc.transact();
        match self.notes.get(&txn, &id.to_string()) {
            Some(Out::Any(Any::String(s))) => Some(s.to_string()),
            _ => None,
        }
    }

    /// All (id, path) entries, sorted by id. Entries with unparseable ids are skipped.
    pub fn entries(&self) -> Vec<(NoteId, String)> {
        let txn = self.doc.transact();
        let mut v: Vec<(NoteId, String)> = self
            .notes
            .iter(&txn)
            .filter_map(|(k, out)| match out {
                Out::Any(Any::String(s)) => k.parse().ok().map(|id| (id, s.to_string())),
                _ => None,
            })
            .collect();
        v.sort();
        v
    }

    /// The vault's display name, if the user has given it one. Written only by the web client
    /// (`meta.name`); a native client reads it to label the vault and to name its folder.
    pub fn name(&self) -> Option<String> {
        let txn = self.doc.transact();
        match self.meta.get(&txn, "name") {
            Some(Out::Any(Any::String(s))) => Some(s.to_string()).filter(|s| !s.trim().is_empty()),
            _ => None,
        }
    }

    /// Name the vault for every replica; returns the update (empty if unchanged). The web
    /// client is what normally writes this — Rust reads it to label a vault's folder — but
    /// import and tests need to be able to set it too.
    pub fn set_name(&self, name: &str) -> Vec<u8> {
        if self.name().as_deref() == Some(name) {
            return Vec::new();
        }
        let before = self.state_vector();
        {
            let mut txn = self.doc.transact_mut();
            self.meta.insert(&mut txn, "name", Any::from(name));
        }
        self.diff_since(&before)
    }

    /// Where this vault keeps its daily notes (`meta.daily_folder`, `meta.daily_format`,
    /// `meta.daily_template`); unset keys are the defaults.
    pub fn daily(&self) -> DailySettings {
        let txn = self.doc.transact();
        let get = |k: &str| match self.meta.get(&txn, k) {
            Some(Out::Any(Any::String(s))) => s.to_string(),
            _ => String::new(),
        };
        DailySettings {
            folder: get("daily_folder"),
            format: get("daily_format"),
            template: get("daily_template"),
        }
    }

    /// Store daily-note settings for every replica; returns the update (empty if unchanged).
    /// Written by the settings dialog in the web client and by an Obsidian import.
    pub fn set_daily(&self, s: &DailySettings) -> Vec<u8> {
        if self.daily() == *s {
            return Vec::new();
        }
        let before = self.state_vector();
        {
            let mut txn = self.doc.transact_mut();
            for (k, v) in
                [("daily_folder", &s.folder), ("daily_format", &s.format), ("daily_template", &s.template)]
            {
                if v.is_empty() {
                    self.meta.remove(&mut txn, k);
                } else {
                    self.meta.insert(&mut txn, k, Any::from(v.as_str()));
                }
            }
        }
        self.diff_since(&before)
    }

    /// Set a note's path; returns the update (empty if unchanged).
    pub fn set_path(&self, id: NoteId, path: &str) -> Vec<u8> {
        if self.path_of(id).as_deref() == Some(path) {
            return Vec::new();
        }
        let before = self.state_vector();
        {
            let mut txn = self.doc.transact_mut();
            self.notes.insert(&mut txn, id.to_string(), Any::from(path));
        }
        self.diff_since(&before)
    }

    /// Remove a note; returns the update (empty if it was not present).
    pub fn remove(&self, id: NoteId) -> Vec<u8> {
        let before = self.state_vector();
        {
            let mut txn = self.doc.transact_mut();
            if self.notes.remove(&mut txn, &id.to_string()).is_none() {
                return Vec::new();
            }
        }
        self.diff_since(&before)
    }
}

impl VaultDoc {
    pub fn attachment_hash(&self, path: &str) -> Option<String> {
        let txn = self.doc.transact();
        match self.attachments.get(&txn, path) {
            Some(Out::Any(Any::String(s))) => Some(s.to_string()),
            _ => None,
        }
    }

    /// All (path, hash) attachment entries, sorted by path.
    pub fn attachment_entries(&self) -> Vec<(String, String)> {
        let txn = self.doc.transact();
        let mut v: Vec<(String, String)> = self
            .attachments
            .iter(&txn)
            .filter_map(|(k, out)| match out {
                Out::Any(Any::String(s)) => Some((k.to_owned(), s.to_string())),
                _ => None,
            })
            .collect();
        v.sort();
        v
    }

    pub fn set_attachment(&self, path: &str, hash: &str) -> Vec<u8> {
        if self.attachment_hash(path).as_deref() == Some(hash) {
            return Vec::new();
        }
        let before = self.state_vector();
        {
            let mut txn = self.doc.transact_mut();
            self.attachments.insert(&mut txn, path.to_owned(), Any::from(hash));
        }
        self.diff_since(&before)
    }

    pub fn remove_attachment(&self, path: &str) -> Vec<u8> {
        let before = self.state_vector();
        {
            let mut txn = self.doc.transact_mut();
            if self.attachments.remove(&mut txn, path).is_none() {
                return Vec::new();
            }
        }
        self.diff_since(&before)
    }

    /// Whether `path` is kept regardless of use (see [`KEPT_FIELD`]).
    pub fn is_kept(&self, path: &str) -> bool {
        let txn = self.doc.transact();
        matches!(self.kept.get(&txn, path), Some(Out::Any(Any::Bool(true))))
    }

    /// Every kept path, sorted.
    pub fn kept_files(&self) -> Vec<String> {
        let txn = self.doc.transact();
        let mut v: Vec<String> = self
            .kept
            .iter(&txn)
            .filter(|(_, out)| matches!(out, Out::Any(Any::Bool(true))))
            .map(|(k, _)| k.to_owned())
            .collect();
        v.sort();
        v
    }

    /// Record a file put in the vault on purpose: its entry, and the mark that keeps it.
    pub fn put_kept_file(&self, path: &str, hash: &str) -> Vec<u8> {
        if self.attachment_hash(path).as_deref() == Some(hash) && self.is_kept(path) {
            return Vec::new();
        }
        let before = self.state_vector();
        {
            let mut txn = self.doc.transact_mut();
            self.attachments.insert(&mut txn, path.to_owned(), Any::from(hash));
            self.kept.insert(&mut txn, path.to_owned(), Any::Bool(true));
        }
        self.diff_since(&before)
    }

    /// Mark `path` as kept, without touching its entry — what a replica does while the bytes
    /// are still on their way to the server, so the entry lands only once they are there.
    pub fn mark_kept(&self, path: &str) -> Vec<u8> {
        if self.is_kept(path) {
            return Vec::new();
        }
        let before = self.state_vector();
        {
            let mut txn = self.doc.transact_mut();
            self.kept.insert(&mut txn, path.to_owned(), Any::Bool(true));
        }
        self.diff_since(&before)
    }

    /// Forget a file: its entry and its mark, in one change.
    pub fn delete_file(&self, path: &str) -> Vec<u8> {
        let before = self.state_vector();
        {
            let mut txn = self.doc.transact_mut();
            let had = self.attachments.remove(&mut txn, path).is_some();
            let marked = self.kept.remove(&mut txn, path).is_some();
            if !had && !marked {
                return Vec::new();
            }
        }
        self.diff_since(&before)
    }

    /// Move a file's entry, and its mark if it has one, from `from` to `to` in one change —
    /// so no replica ever sees the file at neither path, or at both. `None` when there is no
    /// file at `from`.
    pub fn move_file(&self, from: &str, to: &str) -> Option<Vec<u8>> {
        let hash = self.attachment_hash(from)?;
        let kept = self.is_kept(from);
        let before = self.state_vector();
        {
            let mut txn = self.doc.transact_mut();
            self.attachments.remove(&mut txn, from);
            self.kept.remove(&mut txn, from);
            self.attachments.insert(&mut txn, to.to_owned(), Any::from(hash));
            if kept {
                self.kept.insert(&mut txn, to.to_owned(), Any::Bool(true));
            }
        }
        Some(self.diff_since(&before))
    }

    /// The bookmark list, in order. Entries the web client wrote that are not plain
    /// `{kind, target, label}` objects are skipped rather than guessed at.
    pub fn bookmarks(&self) -> Vec<Bookmark> {
        let txn = self.doc.transact();
        self.bookmarks
            .iter(&txn)
            .filter_map(|out| match out {
                Out::Any(Any::Map(m)) => {
                    let field = |k: &str| match m.get(k) {
                        Some(Any::String(s)) => Some(s.to_string()),
                        _ => None,
                    };
                    Some(Bookmark { kind: field("kind")?, target: field("target")?, label: field("label")? })
                }
                _ => None,
            })
            .collect()
    }

    /// Append bookmarks that are not in the list yet (same kind and target), in the given order.
    pub fn add_bookmarks(&self, marks: &[Bookmark]) -> Vec<u8> {
        let mut have: Vec<(&str, &str)> = Vec::new();
        let existing = self.bookmarks();
        have.extend(existing.iter().map(|b| (b.kind.as_str(), b.target.as_str())));
        let mut missing: Vec<&Bookmark> = Vec::new();
        for b in marks {
            if have.iter().any(|(k, t)| *k == b.kind && *t == b.target) {
                continue;
            }
            have.push((&b.kind, &b.target));
            missing.push(b);
        }
        if missing.is_empty() {
            return Vec::new();
        }
        let before = self.state_vector();
        {
            let mut txn = self.doc.transact_mut();
            for b in missing {
                let map = HashMap::from([
                    ("kind".to_owned(), Any::from(b.kind.as_str())),
                    ("target".to_owned(), Any::from(b.target.as_str())),
                    ("label".to_owned(), Any::from(b.label.as_str())),
                ]);
                self.bookmarks.push_back(&mut txn, Any::Map(Arc::new(map)));
            }
        }
        self.diff_since(&before)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bookmarks_append_without_duplicates_and_survive_a_round_trip() {
        let a = VaultDoc::new();
        let one = Bookmark { kind: "note".into(), target: "a.md".into(), label: "A".into() };
        let two = Bookmark { kind: "note".into(), target: "b.md".into(), label: "B".into() };
        assert!(!a.add_bookmarks(&[one.clone(), two.clone()]).is_empty());
        assert_eq!(a.bookmarks(), vec![one.clone(), two.clone()]);
        // Same kind and target: nothing appended, even with a different label.
        let again = Bookmark { kind: "note".into(), target: "a.md".into(), label: "renamed".into() };
        assert!(a.add_bookmarks(&[again]).is_empty());
        assert_eq!(a.bookmarks().len(), 2);

        let b = VaultDoc::from_updates([a.encode_full().as_slice()]).unwrap();
        assert_eq!(b.bookmarks(), vec![one, two]);
    }

    #[test]
    fn daily_settings_replicate_and_clear_back_to_defaults() {
        let a = VaultDoc::new();
        assert_eq!(a.daily(), DailySettings::default());
        let s =
            DailySettings { folder: "Journal".into(), format: "DD.MM.YYYY".into(), template: String::new() };
        assert!(!a.set_daily(&s).is_empty());
        assert!(a.set_daily(&s).is_empty());
        let b = VaultDoc::from_updates([a.encode_full().as_slice()]).unwrap();
        assert_eq!(b.daily(), s);
        b.set_daily(&DailySettings::default());
        assert_eq!(b.daily(), DailySettings::default());
    }

    #[test]
    fn paths_merge_across_replicas() {
        let a = VaultDoc::new();
        let n1 = NoteId::new();
        let n2 = NoteId::new();
        let u = a.set_path(n1, "a.md");
        assert!(!u.is_empty());
        assert!(a.set_path(n1, "a.md").is_empty(), "idempotent");

        let b = VaultDoc::from_updates([a.encode_full().as_slice()]).unwrap();
        let ua = a.set_path(n2, "b.md");
        let ub = b.set_path(n1, "moved/a.md");
        assert!(b.apply_update(&ua).unwrap());
        assert!(a.apply_update(&ub).unwrap());
        assert!(!a.apply_update(&ub).unwrap(), "replay is a no-op");
        assert_eq!(a.entries(), b.entries());
        assert_eq!(a.path_of(n1).as_deref(), Some("moved/a.md"));
        assert_eq!(a.entries().len(), 2);

        let ur = a.remove(n1);
        assert!(!ur.is_empty());
        assert!(a.remove(n1).is_empty());
        assert!(b.apply_update(&ur).unwrap(), "a pure deletion is a change");
        assert!(!b.apply_update(&ur).unwrap(), "replaying it is not");
        assert_eq!(b.entries(), vec![(n2, "b.md".to_owned())]);
    }

    #[test]
    fn attachments_map() {
        let a = VaultDoc::new();
        assert!(!a.set_attachment("attachments/x.png", "h1").is_empty());
        assert!(a.set_attachment("attachments/x.png", "h1").is_empty());
        let b = VaultDoc::from_updates([a.encode_full().as_slice()]).unwrap();
        assert_eq!(b.attachment_hash("attachments/x.png").as_deref(), Some("h1"));
        let u = a.set_attachment("attachments/x.png", "h2");
        b.apply_update(&u).unwrap();
        assert_eq!(b.attachment_entries(), vec![("attachments/x.png".to_owned(), "h2".to_owned())]);
        assert!(!b.remove_attachment("attachments/x.png").is_empty());
        assert!(b.remove_attachment("attachments/x.png").is_empty());
        assert!(b.attachment_entries().is_empty());
    }

    #[test]
    fn kept_files_travel_with_their_entries() {
        let a = VaultDoc::new();
        assert!(!a.put_kept_file("styles/site.scss", "h1").is_empty());
        assert!(a.put_kept_file("styles/site.scss", "h1").is_empty(), "nothing to change");
        a.set_attachment("attachments/pic.png", "h2");
        let b = VaultDoc::from_updates([a.encode_full().as_slice()]).unwrap();
        assert_eq!(b.kept_files(), ["styles/site.scss"]);
        assert!(!b.is_kept("attachments/pic.png"), "a pasted file is not kept");

        let u = a.move_file("styles/site.scss", "theme/site.scss").unwrap();
        b.apply_update(&u).unwrap();
        assert_eq!(b.attachment_hash("theme/site.scss").as_deref(), Some("h1"));
        assert_eq!(b.attachment_hash("styles/site.scss"), None);
        assert_eq!(b.kept_files(), ["theme/site.scss"], "the mark moves with it");
        let u = a.move_file("attachments/pic.png", "img/pic.png").unwrap();
        b.apply_update(&u).unwrap();
        assert!(!b.is_kept("img/pic.png"), "and an unmarked file stays unmarked");
        assert!(a.move_file("nowhere.png", "x.png").is_none());

        b.apply_update(&a.delete_file("theme/site.scss")).unwrap();
        assert!(b.kept_files().is_empty() && b.attachment_hash("theme/site.scss").is_none());
        assert!(a.delete_file("theme/site.scss").is_empty());
    }
}

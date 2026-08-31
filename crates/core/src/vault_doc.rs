//! The vault-level CRDT document (SPEC §4.3): note id → path. Renames and moves are map writes,
//! so they merge across offline devices; a concurrent rename of one note resolves LWW per entry.

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use yrs::updates::decoder::Decode;
use yrs::{Any, Array, ArrayRef, Doc, Map, MapRef, Out, ReadTxn, StateVector, Transact, Update};

use crate::error::{Error, Result};
use crate::ids::NoteId;

pub const NOTES_FIELD: &str = "notes";
/// Vault-relative attachment path → blake3 hash of its content (SPEC §6.3, §7).
pub const ATTACHMENTS_FIELD: &str = "attachments";
/// Ordered bookmark list, shared by every replica (SPEC §4.3, §9). The web client owns this
/// list; Rust only ever appends to it, on import.
pub const BOOKMARKS_FIELD: &str = "bookmarks";

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
        Self { doc, notes, attachments, bookmarks }
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
}

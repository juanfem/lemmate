//! The vault-level CRDT document (SPEC §4.3): note id → path. Renames and moves are map writes,
//! so they merge across offline devices; a concurrent rename of one note resolves LWW per entry.

use yrs::updates::decoder::Decode;
use yrs::{Any, Doc, Map, MapRef, Out, ReadTxn, StateVector, Transact, Update};

use crate::error::{Error, Result};
use crate::ids::NoteId;

pub const NOTES_FIELD: &str = "notes";

pub struct VaultDoc {
    doc: Doc,
    notes: MapRef,
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
        Self { doc, notes }
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
        let before = txn.state_vector();
        txn.apply_update(update).map_err(|e| Error::Crdt(e.to_string()))?;
        Ok(txn.state_vector() != before)
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

#[cfg(test)]
mod tests {
    use super::*;

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
        b.apply_update(&ur).unwrap();
        assert_eq!(b.entries(), vec![(n2, "b.md".to_owned())]);
    }
}

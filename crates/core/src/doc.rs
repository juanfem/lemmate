//! A note's CRDT document: one yrs `Doc` with a single `Y.Text` named [`CONTENT_FIELD`] (SPEC §4.2).

use yrs::updates::decoder::Decode;
use yrs::updates::encoder::Encode;
use yrs::{Doc, GetString, ReadTxn, StateVector, Text, TextRef, Transact, Update};

use crate::CONTENT_FIELD;
use crate::diff::{TextOp, text_ops};
use crate::error::{Error, Result};

pub struct NoteDoc {
    doc: Doc,
    content: TextRef,
}

impl Default for NoteDoc {
    fn default() -> Self {
        Self::new()
    }
}

impl NoteDoc {
    pub fn new() -> Self {
        let doc = Doc::new();
        let content = doc.get_or_insert_text(CONTENT_FIELD);
        Self { doc, content }
    }

    /// Rebuild a doc from one or more v1 updates (snapshot first, then the log).
    pub fn from_updates<'a>(updates: impl IntoIterator<Item = &'a [u8]>) -> Result<Self> {
        let d = Self::new();
        for u in updates {
            d.apply_update(u)?;
        }
        Ok(d)
    }

    pub fn doc(&self) -> &Doc {
        &self.doc
    }

    pub fn text(&self) -> String {
        self.content.get_string(&self.doc.transact())
    }

    pub fn state_vector(&self) -> StateVector {
        self.doc.transact().state_vector()
    }

    /// Full state as a single v1 update (for snapshots and initial sync).
    pub fn encode_full(&self) -> Vec<u8> {
        self.doc.transact().encode_state_as_update_v1(&StateVector::default())
    }

    /// Everything the remote identified by `sv` is missing.
    pub fn diff_since(&self, sv: &StateVector) -> Vec<u8> {
        self.doc.transact().encode_diff_v1(sv)
    }

    pub fn apply_update(&self, update: &[u8]) -> Result<()> {
        let update = Update::decode_v1(update).map_err(|e| Error::Crdt(e.to_string()))?;
        let mut txn = self.doc.transact_mut();
        txn.apply_update(update).map_err(|e| Error::Crdt(e.to_string()))
    }

    /// Replace the text by applying a minimal diff as CRDT edits, so concurrent edits elsewhere
    /// merge instead of being clobbered. Returns the update encoding this change (empty if none).
    pub fn set_text(&self, new: &str) -> Vec<u8> {
        let old = self.text();
        self.apply_ops(&text_ops(&old, new))
    }

    /// Apply pre-computed ops (see [`crate::diff`]) in one transaction; returns the resulting update.
    pub fn apply_ops(&self, ops: &[TextOp]) -> Vec<u8> {
        if ops.is_empty() {
            return Vec::new();
        }
        let before = self.state_vector();
        {
            let mut txn = self.doc.transact_mut();
            for op in ops {
                match op {
                    TextOp::Delete { at, len } => self.content.remove_range(&mut txn, *at, *len),
                    TextOp::Insert { at, text } => self.content.insert(&mut txn, *at, text),
                }
            }
        }
        self.diff_since(&before)
    }
}

/// Helpers for state vectors on the wire.
pub fn encode_state_vector(sv: &StateVector) -> Vec<u8> {
    sv.encode_v1()
}

pub fn decode_state_vector(bytes: &[u8]) -> Result<StateVector> {
    StateVector::decode_v1(bytes).map_err(|e| Error::Crdt(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_text_and_sync_between_replicas() {
        let a = NoteDoc::new();
        let u1 = a.set_text("# Title\n\nHello world.\n");
        assert!(!u1.is_empty());

        let b = NoteDoc::from_updates([u1.as_slice()]).unwrap();
        assert_eq!(b.text(), a.text());

        // A edits the start, B edits the end, both offline; merge must keep both.
        let ua = a.set_text("# Title!\n\nHello world.\n");
        let ub = b.set_text("# Title\n\nHello world. Bye.\n");
        b.apply_update(&ua).unwrap();
        a.apply_update(&ub).unwrap();
        assert_eq!(a.text(), b.text());
        assert_eq!(a.text(), "# Title!\n\nHello world. Bye.\n");
    }

    #[test]
    fn diff_since_gives_only_missing_updates() {
        let a = NoteDoc::new();
        a.set_text("one");
        let b = NoteDoc::from_updates([a.encode_full().as_slice()]).unwrap();
        a.set_text("one two");
        let missing = a.diff_since(&b.state_vector());
        b.apply_update(&missing).unwrap();
        assert_eq!(b.text(), "one two");
        assert!(a.diff_since(&b.state_vector()).len() <= 2, "nothing left to send");
    }

    #[test]
    fn multibyte_edits_land_on_char_boundaries() {
        let d = NoteDoc::new();
        d.set_text("héllo 😀 wörld");
        d.set_text("héllo 😀😀 wörld!");
        assert_eq!(d.text(), "héllo 😀😀 wörld!");
    }
}

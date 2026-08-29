//! Wire framing for the multiplexed sync WebSocket (SPEC §7). Each binary WebSocket message is one
//! [`Frame`]: a doc id followed by a standard Yjs protocol message (`yrs::sync::Message`), so any
//! y-protocols-compatible client can be adapted by prefixing the doc id.
//!
//! Layout: `u16 BE len(doc_id) | doc_id (UTF-8) | yjs message bytes (v1 encoding)`.

pub use yrs::sync::{Awareness, AwarenessUpdate, Message, SyncMessage};
pub use yrs::updates::decoder::Decode;
pub use yrs::updates::encoder::Encode;

use crate::error::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub doc_id: String,
    pub payload: Vec<u8>,
}

impl Frame {
    pub fn new(doc_id: impl Into<String>, message: &Message) -> Self {
        Self { doc_id: doc_id.into(), payload: message.encode_v1() }
    }

    pub fn message(&self) -> Result<Message> {
        Message::decode_v1(&self.payload).map_err(|e| Error::Crdt(e.to_string()))
    }

    pub fn encode(&self) -> Vec<u8> {
        let id = self.doc_id.as_bytes();
        let mut out = Vec::with_capacity(2 + id.len() + self.payload.len());
        out.extend_from_slice(&(id.len() as u16).to_be_bytes());
        out.extend_from_slice(id);
        out.extend_from_slice(&self.payload);
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < 2 {
            return Err(Error::Frame("too short"));
        }
        let n = u16::from_be_bytes([bytes[0], bytes[1]]) as usize;
        let id = bytes.get(2..2 + n).ok_or(Error::Frame("doc id truncated"))?;
        let doc_id = std::str::from_utf8(id).map_err(|_| Error::Frame("doc id not utf-8"))?.to_owned();
        if doc_id.is_empty() {
            return Err(Error::Frame("empty doc id"));
        }
        Ok(Self { doc_id, payload: bytes[2 + n..].to_vec() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::NoteDoc;

    #[test]
    fn frame_round_trip_with_sync_step1() {
        let doc = NoteDoc::new();
        doc.set_text("hi");
        let msg = Message::Sync(SyncMessage::SyncStep1(doc.state_vector()));
        let f = Frame::new("01ARZ3NDEKTSV4RRFFQ69G5FAV", &msg);
        let bytes = f.encode();
        let back = Frame::decode(&bytes).unwrap();
        assert_eq!(back, f);
        assert_eq!(back.message().unwrap(), msg);
    }

    #[test]
    fn rejects_malformed_frames() {
        assert!(Frame::decode(&[]).is_err());
        assert!(Frame::decode(&[0, 5, b'a']).is_err());
        assert!(Frame::decode(&[0, 0]).is_err());
    }
}

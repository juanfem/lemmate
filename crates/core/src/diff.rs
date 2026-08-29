//! Text diffing used to turn a "replace the whole text" request — an external file edit, an API
//! `PUT`, an import — into minimal CRDT edits (SPEC §6.3, §13.1).
//!
//! Offsets are UTF-8 **byte** offsets, matching yrs's default `OffsetKind::Bytes`, and are
//! expressed against the document as it is *at the moment each op is applied*, so a consumer
//! applies them in order without bookkeeping.

use similar::{DiffOp, TextDiff};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TextOp {
    Delete { at: u32, len: u32 },
    Insert { at: u32, text: String },
}

/// Compute the ops that transform `old` into `new`.
pub fn text_ops(old: &str, new: &str) -> Vec<TextOp> {
    if old == new {
        return Vec::new();
    }
    let old_bytes = char_byte_offsets(old);
    let new_bytes = char_byte_offsets(new);
    let diff = TextDiff::from_chars(old, new);

    let mut ops = Vec::new();
    // Bytes added minus bytes removed so far; shifts old-coordinate positions into current ones.
    let mut shift: i64 = 0;
    let at = |old_index: usize, shift: i64| (old_bytes[old_index] as i64 + shift) as u32;

    for op in diff.ops() {
        match *op {
            DiffOp::Equal { .. } => {}
            DiffOp::Delete { old_index, old_len, .. } => {
                let len = (old_bytes[old_index + old_len] - old_bytes[old_index]) as u32;
                ops.push(TextOp::Delete { at: at(old_index, shift), len });
                shift -= len as i64;
            }
            DiffOp::Insert { old_index, new_index, new_len } => {
                let text = new[new_bytes[new_index]..new_bytes[new_index + new_len]].to_owned();
                shift += text.len() as i64;
                ops.push(TextOp::Insert { at: at(old_index, shift - text.len() as i64), text });
            }
            DiffOp::Replace { old_index, old_len, new_index, new_len } => {
                let len = (old_bytes[old_index + old_len] - old_bytes[old_index]) as u32;
                let pos = at(old_index, shift);
                ops.push(TextOp::Delete { at: pos, len });
                shift -= len as i64;
                let text = new[new_bytes[new_index]..new_bytes[new_index + new_len]].to_owned();
                shift += text.len() as i64;
                ops.push(TextOp::Insert { at: pos, text });
            }
        }
    }
    ops
}

/// Apply ops to a plain string; used by tests and by the projection to reconstruct text.
pub fn apply_ops(text: &str, ops: &[TextOp]) -> String {
    let mut s = text.to_owned();
    for op in ops {
        match op {
            TextOp::Delete { at, len } => {
                let a = *at as usize;
                s.replace_range(a..a + *len as usize, "");
            }
            TextOp::Insert { at, text } => s.insert_str(*at as usize, text),
        }
    }
    s
}

/// Byte offset of each char boundary, plus a trailing entry equal to `s.len()`.
fn char_byte_offsets(s: &str) -> Vec<usize> {
    let mut v: Vec<usize> = s.char_indices().map(|(i, _)| i).collect();
    v.push(s.len());
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(old: &str, new: &str) {
        let ops = text_ops(old, new);
        assert_eq!(apply_ops(old, &ops), new, "ops: {ops:?}");
    }

    #[test]
    fn round_trips() {
        check("", "hello");
        check("hello", "");
        check("hello world", "hello brave new world");
        check("abc", "axc");
        check("line one\nline two\n", "line one\nline 2\nline three\n");
        check("héllo wörld", "hello world");
        check("日本語のテキスト", "日本語の新しいテキスト!");
        check("emoji 😀 here", "emoji 😀😀 there");
        check("same", "same");
    }

    #[test]
    fn identical_text_yields_no_ops() {
        assert!(text_ops("x", "x").is_empty());
    }
}

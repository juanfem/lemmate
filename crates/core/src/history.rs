//! A note's version history as people read it (SPEC §9).
//!
//! The store snapshots a doc to keep its journal short (every 10 minutes of editing, or 500
//! updates), and those snapshots are the addressable points in its past. Listed one by one they
//! are the store's rhythm, not the writer's: an afternoon's work comes back as a dozen rows that
//! all say the same thing. So the list folds them into *editing sessions* — snapshots less than
//! [`SESSION_GAP`] apart are one sitting — and says of each entry what changed since the one
//! before it. A labelled snapshot (a version someone saved or named) ends the session it is in
//! and is always listed.

use std::time::Duration;

use serde::Serialize;
use similar::{DiffOp, TextDiff};

use crate::doc::NoteDoc;
use crate::error::Result;
use crate::store::Journal;

/// Snapshots closer together than this belong to one editing session.
pub const SESSION_GAP: Duration = Duration::from_secs(30 * 60);

/// Past this, a line diff settles for a coarser answer rather than hold the list up.
const DIFF_TIMEOUT: Duration = Duration::from_millis(200);

/// What changed between an entry and the one before it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Changes {
    pub added: u32,
    pub removed: u32,
    /// The headings the changed lines fall under, in the order they appear, each once.
    pub sections: Vec<String>,
}

/// One row of a note's history, newest first in [`entries`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Entry {
    /// The snapshot the entry stands for — the last one of its session.
    pub seq: i64,
    pub created_ms: i64,
    /// When the edits it holds began: the first update after the previous entry, while the
    /// journal still has it, else the session's first snapshot.
    pub started_ms: i64,
    pub label: Option<String>,
    pub author: Option<String>,
    /// How many snapshots the entry folds together (1 for a lone one).
    pub snapshots: u32,
    /// Against the previous entry, or the empty note for the first one. `None` for the oldest
    /// entry left once older history has been pruned: there is nothing to compare it with.
    pub changes: Option<Changes>,
}

/// The history of the doc whose journal this is, newest first.
pub fn entries(journal: &Journal) -> Result<Vec<Entry>> {
    entries_with_gap(journal, SESSION_GAP)
}

pub fn entries_with_gap(journal: &Journal, gap: Duration) -> Result<Vec<Entry>> {
    let gap_ms = gap.as_millis() as i64;
    // Sessions as index ranges into the snapshots (oldest first).
    let mut sessions: Vec<(usize, usize)> = Vec::new();
    for (i, snap) in journal.snapshots.iter().enumerate() {
        let joins = match sessions.last() {
            Some(&(_, end)) => {
                let prev = &journal.snapshots[end].version;
                prev.label.is_none() && snap.version.created_ms - prev.created_ms < gap_ms
            }
            None => false,
        };
        match sessions.last_mut() {
            Some(last) if joins => last.1 = i,
            _ => sessions.push((i, i)),
        }
    }

    let mut out = Vec::with_capacity(sessions.len());
    let mut before: Option<String> = journal.complete().then(String::new);
    let mut prev_seq = 0;
    for (start, end) in sessions {
        let snap = &journal.snapshots[end];
        let text = NoteDoc::from_updates([snap.bytes.as_slice()])?.text();
        let v = &snap.version;
        // Updates in (prev_seq, seq] are the edits this entry holds.
        let from = journal.updates.partition_point(|&(s, _)| s <= prev_seq);
        let started_ms = journal
            .updates
            .get(from)
            .filter(|&&(s, _)| s <= v.seq)
            .map_or(journal.snapshots[start].version.created_ms, |&(_, t)| t);
        out.push(Entry {
            seq: v.seq,
            created_ms: v.created_ms,
            started_ms,
            label: v.label.clone(),
            author: v.author.clone(),
            snapshots: (end - start + 1) as u32,
            changes: before.as_deref().map(|b| changes(b, &text)),
        });
        before = Some(text);
        prev_seq = v.seq;
    }
    out.reverse();
    Ok(out)
}

/// A label as typed, as it is stored: trimmed, at most [`LABEL_MAX`] characters, and none at
/// all when nothing is left.
pub fn clean_label(label: Option<&str>) -> Option<String> {
    let label = label?.trim();
    (!label.is_empty()).then(|| label.chars().take(LABEL_MAX).collect::<String>().trim_end().to_owned())
}

pub const LABEL_MAX: usize = 200;

/// Lines added and removed going from `old` to `new`, and the sections they are in.
pub fn changes(old: &str, new: &str) -> Changes {
    let a: Vec<&str> = old.lines().collect();
    let b: Vec<&str> = new.lines().collect();
    let diff = TextDiff::configure().timeout(DIFF_TIMEOUT).diff_slices(&a, &b);
    let (in_a, in_b) = (section_of(&a), section_of(&b));
    let mut out = Changes::default();
    // A blank line added or dropped between paragraphs names no section: it is spacing.
    let mut note = |line: &str, section: Option<&str>| {
        if let Some(s) = section
            && !line.trim().is_empty()
            && !out.sections.iter().any(|t| t == s)
        {
            out.sections.push(s.to_owned());
        }
    };
    for op in diff.ops() {
        match *op {
            DiffOp::Equal { .. } => {}
            DiffOp::Delete { old_index, old_len, .. } => {
                out.removed += old_len as u32;
                (old_index..old_index + old_len).for_each(|i| note(a[i], in_a[i]));
            }
            DiffOp::Insert { new_index, new_len, .. } => {
                out.added += new_len as u32;
                (new_index..new_index + new_len).for_each(|i| note(b[i], in_b[i]));
            }
            DiffOp::Replace { old_index, old_len, new_index, new_len } => {
                out.removed += old_len as u32;
                out.added += new_len as u32;
                (new_index..new_index + new_len).for_each(|i| note(b[i], in_b[i]));
                (old_index..old_index + old_len).for_each(|i| note(a[i], in_a[i]));
            }
        }
    }
    out
}

/// For each line, the text of the ATX heading it sits under (a heading line is under itself).
/// Front matter and fenced code are skipped: a `# comment` in a shell block is not a section.
fn section_of<'a>(lines: &[&'a str]) -> Vec<Option<&'a str>> {
    let mut out = Vec::with_capacity(lines.len());
    let mut current = None;
    let mut fence: Option<(char, usize)> = None;
    let mut front = lines.first().is_some_and(|l| l.trim_end() == "---");
    for (i, line) in lines.iter().enumerate() {
        if front {
            if i > 0 && matches!(line.trim_end(), "---" | "...") {
                front = false;
            }
            out.push(None);
            continue;
        }
        let indent = line.len() - line.trim_start_matches(' ').len();
        let body = line.trim_start_matches(' ');
        if indent <= 3 {
            let marker = body.chars().next().filter(|c| matches!(c, '`' | '~'));
            if let Some(c) = marker {
                let run = body.chars().take_while(|&x| x == c).count();
                match fence {
                    None if run >= 3 => fence = Some((c, run)),
                    Some((f, n)) if f == c && run >= n && body[run..].trim().is_empty() => fence = None,
                    _ => {}
                }
                out.push(current);
                continue;
            }
            if fence.is_none()
                && let Some(text) = atx_heading(body)
            {
                current = Some(text);
            }
        }
        out.push(current);
    }
    out
}

fn atx_heading(line: &str) -> Option<&str> {
    let level = line.chars().take_while(|&c| c == '#').count();
    if !(1..=6).contains(&level) {
        return None;
    }
    let rest = &line[level..];
    if !(rest.is_empty() || rest.starts_with([' ', '\t'])) {
        return None;
    }
    // A closing run of `#`s is not part of the text, if a space separates it.
    let rest = rest.trim();
    let trimmed = rest.trim_end_matches('#');
    let text = if trimmed.is_empty() || trimmed.ends_with([' ', '\t']) { trimmed.trim_end() } else { rest };
    (!text.is_empty()).then_some(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{DocId, NoteId};
    use crate::store::Store;

    const MIN: i64 = 60 * 1000;

    #[test]
    fn counts_lines_and_names_sections() {
        let old = "# Title\n\nintro\n\n## Methods\n\nwe did a\n\n## Results\n\nnone\n";
        let new = "# Title\n\nintro\n\n## Methods\n\nwe did b\nand c\n\n## Results\n\nnone\n";
        let c = changes(old, new);
        assert_eq!((c.added, c.removed), (2, 1));
        assert_eq!(c.sections, ["Methods"]);
        // A deletion is named by the section it was taken from.
        let c = changes(new, "# Title\n\nintro\n");
        assert_eq!(c.removed, 9);
        assert_eq!(c.sections, ["Methods", "Results"]);
        // Text above every heading has no section to name.
        assert_eq!(changes("a\n# H\nx", "b\n# H\nx").sections, Vec::<String>::new());
    }

    #[test]
    fn labels_are_trimmed_and_capped() {
        assert_eq!(clean_label(Some("  draft 2 ")), Some("draft 2".into()));
        assert_eq!(clean_label(Some("   ")), None);
        assert_eq!(clean_label(None), None);
        assert_eq!(clean_label(Some(&"é".repeat(300))).unwrap().chars().count(), LABEL_MAX);
    }

    #[test]
    fn headings_skip_front_matter_and_code() {
        let text = "---\n# not: a heading\n---\n# Real ##\n```sh\n# comment\n```\nbody\n####### seven\n#tag";
        let lines: Vec<&str> = text.lines().collect();
        let s = section_of(&lines);
        assert_eq!(s[1], None);
        assert_eq!(s[3], Some("Real"));
        assert_eq!(s[5], Some("Real"), "a shell comment in a fence is not a heading");
        assert_eq!(s[7], Some("Real"));
        assert_eq!(s[8], Some("Real"), "seven #s is not a heading");
        assert_eq!(s[9], Some("Real"), "#tag is a tag");
        assert_eq!(atx_heading("# C# notes"), Some("C# notes"));
        assert_eq!(atx_heading("#"), None);
    }

    /// A note edited in two sittings, with an autosnapshot every 10 minutes of the first.
    fn two_sittings(store: &mut Store, id: DocId, doc: &NoteDoc) {
        let edit = |store: &mut Store, text: &str, at: i64| {
            let u = doc.set_text(text);
            store.append_update_at(id, &u, None, at).unwrap();
        };
        edit(store, "# A\none\n", 0);
        edit(store, "# A\none\ntwo\n", 5 * MIN);
        store.snapshot_at(id, &doc.encode_full(), 10 * MIN).unwrap();
        edit(store, "# A\none\ntwo\nthree\n", 12 * MIN);
        store.snapshot_at(id, &doc.encode_full(), 20 * MIN).unwrap();
        // Three hours later.
        edit(store, "# A\none\ntwo\nthree\n# B\nfour\n", 200 * MIN);
        store.snapshot_at(id, &doc.encode_full(), 210 * MIN).unwrap();
    }

    #[test]
    fn folds_snapshots_into_sessions() {
        let mut store = Store::open_in_memory().unwrap();
        let id = DocId::Note(NoteId::new());
        let doc = NoteDoc::new();
        two_sittings(&mut store, id, &doc);
        let e = entries(&store.journal(id).unwrap()).unwrap();
        assert_eq!(e.len(), 2, "{e:#?}");
        assert_eq!((e[1].snapshots, e[1].started_ms, e[1].created_ms), (2, 0, 20 * MIN));
        assert_eq!(
            e[1].changes,
            Some(Changes { added: 4, removed: 0, sections: vec!["A".into()] }),
            "the first entry is compared with the empty note"
        );
        assert_eq!((e[0].snapshots, e[0].started_ms), (1, 200 * MIN));
        assert_eq!(e[0].changes, Some(Changes { added: 2, removed: 0, sections: vec!["B".into()] }));
    }

    #[test]
    fn a_label_ends_its_session() {
        let mut store = Store::open_in_memory().unwrap();
        let id = DocId::Note(NoteId::new());
        let doc = NoteDoc::new();
        two_sittings(&mut store, id, &doc);
        // Name the first sitting's first snapshot: it stands alone, and the rest of that
        // sitting becomes an entry of its own, compared with it.
        let row = store.set_version_label(id, 2, Some("draft")).unwrap().unwrap();
        assert_eq!((row.seq, row.label.as_deref()), (2, Some("draft")));
        let e = entries(&store.journal(id).unwrap()).unwrap();
        assert_eq!(e.iter().map(|e| e.seq).collect::<Vec<_>>(), [4, 3, 2]);
        assert_eq!(e[2].label.as_deref(), Some("draft"));
        assert_eq!(e[1].started_ms, 12 * MIN);
        assert_eq!(e[1].changes.as_ref().map(|c| (c.added, c.removed)), Some((1, 0)));
        // Taking the name away folds it back in.
        store.set_version_label(id, 2, None).unwrap().unwrap();
        assert_eq!(entries(&store.journal(id).unwrap()).unwrap().len(), 2);
        assert_eq!(store.set_version_label(id, 99, Some("x")).unwrap(), None, "no snapshot there");
    }

    #[test]
    fn pruned_history_has_no_first_comparison() {
        let mut store = Store::open_in_memory().unwrap();
        let id = DocId::Note(NoteId::new());
        let doc = NoteDoc::new();
        two_sittings(&mut store, id, &doc);
        let policy = crate::store::RetentionPolicy {
            retain_updates: Duration::from_secs(60 * 60),
            ..Default::default()
        };
        // Keeping an hour of updates at minute 100 drops everything up to the 20-minute
        // snapshot, which is kept as the new base.
        store.maintain(id, &policy, 100 * MIN, || unreachable!()).unwrap();
        let journal = store.journal(id).unwrap();
        assert!(!journal.complete());
        let e = entries(&journal).unwrap();
        assert_eq!(e.iter().map(|e| e.seq).collect::<Vec<_>>(), [4, 3]);
        assert_eq!(e[1].changes, None);
        assert_eq!(e[0].changes.as_ref().map(|c| c.added), Some(2));
    }
}

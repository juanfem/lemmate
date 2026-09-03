//! Folding one vault into another (SPEC §3.2).
//!
//! The user has a vault on this machine and a vault on the server, and wants one of them: not
//! two vaults side by side, but the notes of the first *inside* the second. That is what this
//! module plans and what `local::merge_vaults` carries out.
//!
//! Nothing here is clever about CRDTs, and it does not need to be. Note ids are ULIDs, unique
//! across vaults, and every note carries its own in front matter (SPEC §6.3), which the engine
//! reads when a file appears that it does not know — "moved in from another vault" is a case
//! [`crate::client::Engine::local_create`] already handles. So a merge is: write the source's
//! files into the destination's folder, let the destination's engine adopt them by id, and
//! retire the source. Identity, links and backlinks survive, because nothing is re-created —
//! the same note is simply somewhere else.
//!
//! What this module owns is the arithmetic that has to be right before any of that happens:
//! where each note lands, what to do when both vaults have a `Plan.md`, and which attachments
//! are the same file under the same name (free), a different file under the same name (renamed,
//! and the notes that point at it rewritten), or new.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::ids::{NoteId, VaultId};

/// One note, and where it will be in the destination vault.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlannedNote {
    /// The note's own id, which the merge preserves; a string because this crosses the wire to
    /// the dialog that shows the plan.
    pub id: String,
    pub from: String,
    pub to: String,
    /// The destination already had a note at the obvious path, so this one was given another.
    pub renamed: bool,
}

/// What happens to one of the source vault's attachments.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentFate {
    /// The destination has no such file: copied under the same path.
    New,
    /// The destination has the same bytes under that path already: nothing to copy.
    Same,
    /// The destination has a *different* file under that path: copied under a new name, and
    /// every reference to it in the notes being merged is rewritten.
    Renamed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlannedAttachment {
    pub from: String,
    pub to: String,
    pub hash: String,
    pub fate: AttachmentFate,
}

/// Everything a merge will do, worked out before it does any of it.
///
/// This is what the dialog shows and what the caller replays: a merge is a plan plus the file
/// copies it describes, so a dry run and the real thing differ only in whether anything is
/// written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergePlan {
    pub from: String,
    pub into: String,
    /// Folder inside the destination that the source's tree lands in; empty merges at the root.
    pub folder: String,
    pub notes: Vec<PlannedNote>,
    pub attachments: Vec<PlannedAttachment>,
}

impl MergePlan {
    pub fn renamed_notes(&self) -> usize {
        self.notes.iter().filter(|n| n.renamed).count()
    }
    pub fn renamed_attachments(&self) -> usize {
        self.attachments.iter().filter(|a| a.fate == AttachmentFate::Renamed).count()
    }
    /// Old path → new path for the attachments that had to move, which is what the notes being
    /// merged have to be rewritten against.
    pub fn attachment_rewrites(&self) -> Vec<(String, String)> {
        self.attachments
            .iter()
            .filter(|a| a.fate == AttachmentFate::Renamed)
            .map(|a| (a.from.clone(), a.to.clone()))
            .collect()
    }
}

/// A vault as a merge sees it: what it holds, by path.
#[derive(Debug, Clone, Default)]
pub struct Survey {
    /// Note id → vault-relative path.
    pub notes: Vec<(NoteId, String)>,
    /// Attachment path → content hash, as recorded in the vault doc.
    pub attachments: Vec<(String, String)>,
}

/// A folder name for the source vault's tree inside the destination, from its name.
///
/// Only used as a default: the caller may pass any folder, including none.
pub fn default_folder(name: Option<&str>, id: VaultId) -> String {
    name.and_then(crate::vaults::folder_name_for).unwrap_or_else(|| crate::vaults::default_folder_name(id))
}

/// Work out where everything goes. Pure: no engine, no disk, no ordering surprises.
pub fn plan(from: VaultId, into: VaultId, folder: &str, source: &Survey, dest: &Survey) -> MergePlan {
    let folder = folder.trim().trim_matches('/').to_owned();
    let mut taken: HashSet<String> = dest.notes.iter().map(|(_, p)| p.clone()).collect();

    let mut notes = Vec::with_capacity(source.notes.len());
    for (id, path) in &source.notes {
        let wanted = join(&folder, path);
        let to = free_path(&wanted, &taken);
        taken.insert(to.clone());
        notes.push(PlannedNote { id: id.to_string(), renamed: to != wanted, from: path.clone(), to });
    }

    // Attachments are not moved into the merge folder: a vault keeps one `attachments/` at its
    // root (SPEC §6.3), and a note that came from another vault must still resolve its images.
    let existing: HashMap<&str, &str> =
        dest.attachments.iter().map(|(p, h)| (p.as_str(), h.as_str())).collect();
    let mut attachments = Vec::with_capacity(source.attachments.len());
    let mut planned: HashSet<String> = HashSet::new();
    for (path, hash) in &source.attachments {
        let fate = match existing.get(path.as_str()) {
            None => AttachmentFate::New,
            Some(h) if *h == hash.as_str() => AttachmentFate::Same,
            Some(_) => AttachmentFate::Renamed,
        };
        let to = match fate {
            AttachmentFate::Renamed => {
                let candidate = suffixed(path, hash);
                free_path(
                    &candidate,
                    &planned.iter().cloned().chain(existing.keys().map(|k| (*k).to_owned())).collect(),
                )
            }
            _ => path.clone(),
        };
        planned.insert(to.clone());
        attachments.push(PlannedAttachment { from: path.clone(), to, hash: hash.clone(), fate });
    }

    MergePlan { from: from.to_string(), into: into.to_string(), folder, notes, attachments }
}

/// `folder/path`, or `path` when the folder is empty.
fn join(folder: &str, path: &str) -> String {
    if folder.is_empty() { path.to_owned() } else { format!("{folder}/{path}") }
}

/// `wanted`, or the first `name-2.ext`, `name-3.ext`… that nothing has taken.
fn free_path(wanted: &str, taken: &HashSet<String>) -> String {
    if !taken.contains(wanted) {
        return wanted.to_owned();
    }
    let (stem, ext) = split_ext(wanted);
    (2..1000)
        .map(|n| format!("{stem}-{n}{ext}"))
        .find(|c| !taken.contains(c))
        .unwrap_or_else(|| wanted.to_owned())
}

/// `attachments/logo.png` + a hash → `attachments/logo-3f9c2a.png`, the same shape the engine
/// uses when an upload collides with a different file of the same name.
fn suffixed(path: &str, hash: &str) -> String {
    let (stem, ext) = split_ext(path);
    let short: String = hash.chars().take(6).collect();
    format!("{stem}-{short}{ext}")
}

fn split_ext(path: &str) -> (&str, &str) {
    match path.rfind('.') {
        // A dot in a folder name, or a leading dot, is not an extension.
        Some(i) if i > path.rfind('/').map(|s| s + 1).unwrap_or(0) => path.split_at(i),
        _ => (path, ""),
    }
}

/// Point a note's attachment references at their new paths.
///
/// Two forms carry them (SPEC §5.4): a markdown link or image whose target is the vault-relative
/// path, and a wikilink embed that names the file — usually by basename, since that is how
/// Obsidian writes them. Both are rewritten; anything else is left exactly as it was, because a
/// merge has no business editing prose.
pub fn rewrite_references(text: &str, rewrites: &[(String, String)]) -> String {
    let mut out = text.to_owned();
    for (from, to) in rewrites {
        if from == to {
            continue;
        }
        out = out.replace(&format!("]({from})"), &format!("]({to})"));
        let (from_base, to_base) = (basename(from), basename(to));
        if from_base != to_base {
            for (open, close) in [("[[", "]]"), ("[[", "|")] {
                out = out.replace(&format!("{open}{from_base}{close}"), &format!("{open}{to_base}{close}"));
            }
        }
    }
    out
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn survey(notes: &[&str], attachments: &[(&str, &str)]) -> Survey {
        Survey {
            notes: notes.iter().map(|p| (NoteId::new(), (*p).to_owned())).collect(),
            attachments: attachments.iter().map(|(p, h)| ((*p).to_owned(), (*h).to_owned())).collect(),
        }
    }

    #[test]
    fn notes_land_under_the_folder_and_collisions_are_given_a_number() {
        let source = survey(&["Plan.md", "Daily/2026-09-03.md"], &[]);
        let dest = survey(&["Notes/Plan.md", "Other.md"], &[]);
        let p = plan(VaultId::new(), VaultId::new(), "Notes", &source, &dest);
        assert_eq!(p.notes[0].to, "Notes/Plan-2.md", "the destination already had that path");
        assert!(p.notes[0].renamed);
        assert_eq!(p.notes[1].to, "Notes/Daily/2026-09-03.md");
        assert!(!p.notes[1].renamed);
        assert_eq!(p.renamed_notes(), 1);
    }

    #[test]
    fn merging_at_the_root_keeps_the_paths() {
        let source = survey(&["Plan.md"], &[]);
        let p = plan(VaultId::new(), VaultId::new(), "", &source, &survey(&[], &[]));
        assert_eq!(p.notes[0].to, "Plan.md");
        assert_eq!(p.folder, "");
    }

    /// Two notes of the same name in the *source* must not both claim one destination path.
    #[test]
    fn the_plan_does_not_collide_with_itself() {
        let source = survey(&["a/Plan.md", "b/Plan.md"], &[]);
        let dest = survey(&["Plan.md"], &[]);
        let p = plan(VaultId::new(), VaultId::new(), "", &source, &dest);
        let paths: Vec<&str> = p.notes.iter().map(|n| n.to.as_str()).collect();
        assert_eq!(paths, vec!["a/Plan.md", "b/Plan.md"], "different folders never collided");

        let source = survey(&["Plan.md", "Plan.md"], &[]);
        let p = plan(VaultId::new(), VaultId::new(), "", &source, &dest);
        assert_eq!(p.notes[0].to, "Plan-2.md");
        assert_eq!(p.notes[1].to, "Plan-3.md");
    }

    #[test]
    fn attachments_are_new_identical_or_renamed() {
        let source = survey(
            &[],
            &[
                ("attachments/new.png", "aaa"),
                ("attachments/same.png", "bbb"),
                ("attachments/clash.png", "ccc"),
            ],
        );
        let dest = survey(&[], &[("attachments/same.png", "bbb"), ("attachments/clash.png", "ddd")]);
        let p = plan(VaultId::new(), VaultId::new(), "Notes", &source, &dest);
        assert_eq!(p.attachments[0].fate, AttachmentFate::New);
        assert_eq!(p.attachments[0].to, "attachments/new.png", "attachments stay at the root");
        assert_eq!(p.attachments[1].fate, AttachmentFate::Same);
        assert_eq!(p.attachments[1].to, "attachments/same.png", "the same bytes are not copied twice");
        assert_eq!(p.attachments[2].fate, AttachmentFate::Renamed);
        assert_eq!(p.attachments[2].to, "attachments/clash-ccc.png");
        assert_eq!(
            p.attachment_rewrites(),
            vec![("attachments/clash.png".into(), "attachments/clash-ccc.png".into())]
        );
    }

    #[test]
    fn only_the_moved_references_are_rewritten() {
        let text = "\
# Plan

![shot](attachments/clash.png) and ![[clash.png]] and ![[clash.png|200]]
Unrelated: ![keep](attachments/new.png), and the word clash.png in prose.
";
        let out =
            rewrite_references(text, &[("attachments/clash.png".into(), "attachments/clash-ccc.png".into())]);
        assert!(out.contains("![shot](attachments/clash-ccc.png)"));
        assert!(out.contains("![[clash-ccc.png]]"));
        assert!(out.contains("![[clash-ccc.png|200]]"));
        assert!(out.contains("![keep](attachments/new.png)"), "untouched: {out}");
        assert!(out.contains("the word clash.png in prose"), "prose is not edited: {out}");
    }

    #[test]
    fn extensions_survive_the_numbering() {
        let taken: HashSet<String> = ["a/b.tar.gz".to_owned()].into_iter().collect();
        assert_eq!(free_path("a/b.tar.gz", &taken), "a/b.tar-2.gz");
        let taken: HashSet<String> = ["no.ext/file".to_owned()].into_iter().collect();
        assert_eq!(free_path("no.ext/file", &taken), "no.ext/file-2");
    }
}

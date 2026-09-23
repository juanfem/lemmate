//! The vault's files that are not notes, as the file manager sees them (SPEC §9): what it lists,
//! which paths it accepts, and how notes follow a file that moves. Shared by the server and the
//! local relay, which store the bytes differently and agree on everything else.

use serde::Serialize;

use crate::attachments::{resolve_reference, yaml_paths};
use crate::ids::NoteId;

/// One file, as the file manager lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileEntry {
    pub path: String,
    pub hash: String,
    /// Bytes, when the store knows them.
    pub size: Option<u64>,
    /// The notes that depend on it (their ids): link to it, embed it, name it in front matter,
    /// or use a stylesheet that imports it.
    pub used_by: Vec<String>,
    /// Put in the vault on purpose, so kept whether or not a note uses it.
    pub kept: bool,
    /// A vault resource (`_quarto.yml`, `_metadata.yml`, `export/`): kept for the vault.
    pub vault: bool,
}

/// Assemble the listing from a vault doc's entries and marks, the store's (note, path) pairs,
/// and whatever the caller knows about sizes.
pub fn listing(
    entries: Vec<(String, String)>,
    kept: &[String],
    users: &[(NoteId, String)],
    size: impl Fn(&str, &str) -> Option<u64>,
) -> Vec<FileEntry> {
    entries
        .into_iter()
        .map(|(path, hash)| {
            let mut used_by: Vec<String> =
                users.iter().filter(|(_, p)| *p == path).map(|(id, _)| id.to_string()).collect();
            used_by.sort();
            used_by.dedup();
            FileEntry {
                size: size(&path, &hash),
                kept: kept.contains(&path),
                vault: crate::attachments::is_vault_resource(&path),
                used_by,
                hash,
                path,
            }
        })
        .collect()
}

/// A path the file manager may write to: vault-relative, inside the vault, not a note (notes
/// are made as notes), and nothing the sync leaves alone — no hidden segment, no sidecar, no
/// `.tmp`. `None` for anything else.
pub fn file_path(raw: &str) -> Option<String> {
    let path = crate::import::upload_path(raw)?;
    let name = path.rsplit('/').next()?;
    let lower = name.to_ascii_lowercase();
    if path.split('/').any(|seg| seg.starts_with('.'))
        || lower.ends_with(".md")
        || lower.ends_with(".qmd")
        || lower.ends_with(".tmp")
    {
        return None;
    }
    Some(path)
}

/// `to` (vault-relative) as a link from the note at `note_path`: up out of the note's folder as
/// far as the two share nothing, then down.
pub fn relative_link(note_path: &str, to: &str) -> String {
    let from: Vec<&str> = note_path.split('/').collect();
    let from = &from[..from.len().saturating_sub(1)];
    let target: Vec<&str> = to.split('/').collect();
    let (dirs, name) = target.split_at(target.len() - 1);
    let common = from.iter().zip(dirs).take_while(|(a, b)| a == b).count();
    let mut parts: Vec<&str> = vec![".."; from.len() - common];
    parts.extend(&dirs[common..]);
    parts.extend(name);
    parts.join("/")
}

/// The note's text with every reference to the file `from` pointed at `to` instead, or `None`
/// when it has none. `files` is every file in the vault *before* the move, `from` among them:
/// a reference means what it resolves to there, the way [`resolve_reference`] resolves it.
///
/// - Markdown links and images, `[x](…)`, `![](<…>)`, and `[id]: …` definitions: the new
///   target is relative to the note (or from the root, when the old one started with `/`), with
///   the old one's `#fragment` kept and its spelling of spaces (`%20`, `<…>`) followed.
/// - `[[…]]` and `![[…]]`: the new file's bare name when that finds it unambiguously, else its
///   vault path; an alias or a `|300` width stays.
/// - Front-matter values (`theme: [cosmo, custom.scss]`): the new path, relative to the note.
///
/// Code — fenced blocks and inline spans — is left as it is.
pub fn rewrite_references(
    note_path: &str,
    text: &str,
    from: &str,
    to: &str,
    files: &[String],
) -> Option<String> {
    let exists = |c: &str| files.iter().any(|f| f == c);
    let points_at_from = |target: &str, wiki: bool| {
        resolve_reference(note_path, target, wiki, exists, || files.to_vec()).as_deref() == Some(from)
    };
    let mut out = String::with_capacity(text.len() + 32);
    let mut changed = false;

    let (front, body_start) = match crate::frontmatter::block(text) {
        Some((yaml, end)) => {
            let head = &text[..yaml.start];
            let mut yaml_text = text[yaml.clone()].to_owned();
            for value in yaml_paths(&yaml_text) {
                if points_at_from(&value, false) {
                    let new =
                        if value.starts_with('/') { format!("/{to}") } else { relative_link(note_path, to) };
                    if let Some(replaced) = replace_token(&yaml_text, &value, &new) {
                        yaml_text = replaced;
                        changed = true;
                    }
                }
            }
            (format!("{head}{yaml_text}{}", &text[yaml.end..end]), end)
        }
        None => (String::new(), 0),
    };
    out.push_str(&front);

    let new_wiki = {
        let name = to.rsplit('/').next().unwrap_or(to);
        let others = files.iter().filter(|f| *f != from && f.rsplit('/').next() == Some(name)).count();
        if others == 0 { name.to_owned() } else { to.to_owned() }
    };
    let mut fence: Option<(char, usize)> = None;
    for line in text[body_start..].split_inclusive('\n') {
        if let Some((ch, n)) = fence {
            if crate::import::closes_fence(line, ch, n) {
                fence = None;
            }
            out.push_str(line);
            continue;
        }
        if let Some(open) = crate::import::fence_marker(line) {
            fence = Some(open);
            out.push_str(line);
            continue;
        }
        let rewritten =
            rewrite_line(line, &|target, wiki| points_at_from(target, wiki), note_path, to, &new_wiki);
        if rewritten != line {
            changed = true;
        }
        out.push_str(&rewritten);
    }
    changed.then_some(out)
}

/// Replace whole occurrences of `old` in a YAML text — as a value, not inside a longer word —
/// with `new`. `None` when there was none.
fn replace_token(yaml: &str, old: &str, new: &str) -> Option<String> {
    let before_ok =
        |c: Option<char>| c.is_none_or(|c| matches!(c, ' ' | '\t' | '[' | ',' | '"' | '\'' | ':' | '-'));
    // Not a space after: `title: custom.scss notes` is prose that happens to mention the file.
    let after_ok = |c: Option<char>| c.is_none_or(|c| matches!(c, ']' | ',' | '"' | '\'' | '\n' | '\r'));
    let mut out = String::with_capacity(yaml.len());
    let mut rest = yaml;
    let mut hit = false;
    while let Some(at) = rest.find(old) {
        let prev = rest[..at].chars().next_back().or_else(|| out.chars().next_back());
        let next = rest[at + old.len()..].chars().next();
        out.push_str(&rest[..at]);
        if before_ok(prev) && after_ok(next) {
            out.push_str(new);
            hit = true;
        } else {
            out.push_str(old);
        }
        rest = &rest[at + old.len()..];
    }
    out.push_str(rest);
    hit.then_some(out)
}

/// One line outside fenced code. Inline code spans pass through; links and wikilinks outside
/// them are rewritten when `points` says they name the moved file.
fn rewrite_line(
    line: &str,
    points: &dyn Fn(&str, bool) -> bool,
    note_path: &str,
    to: &str,
    new_wiki: &str,
) -> String {
    // A link reference definition: `[id]: target "title"`.
    if let Some(def) = definition_target(line) {
        let (start, end) = def;
        let target = &line[start..end];
        let (bare, suffix) = split_suffix(unwrap_angle(target));
        if points(&decode(bare), false) {
            return format!(
                "{}{}{}",
                &line[..start],
                new_target(target, bare, suffix, note_path, to),
                &line[end..]
            );
        }
        return line.to_owned();
    }
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while !rest.is_empty() {
        let tick = rest.find('`');
        let paren = rest.find("](");
        let wiki = rest.find("[[");
        let next = [tick, paren, wiki].into_iter().flatten().min();
        let Some(at) = next else {
            out.push_str(rest);
            break;
        };
        if Some(at) == tick {
            out.push_str(&rest[..at]);
            let run = rest[at..].chars().take_while(|&c| c == '`').count();
            let closer = "`".repeat(run);
            match rest[at + run..].find(&closer) {
                Some(end) => {
                    let span = at + run + end + run;
                    out.push_str(&rest[at..span]);
                    rest = &rest[span..];
                }
                None => {
                    out.push_str(&rest[at..]);
                    break;
                }
            }
        } else if Some(at) == wiki {
            out.push_str(&rest[..at + 2]);
            let inner_from = at + 2;
            let Some(end) = rest[inner_from..].find("]]") else {
                out.push_str(&rest[inner_from..]);
                break;
            };
            let inner = &rest[inner_from..inner_from + end];
            let cut = inner.find(['|', '#']).map(|i| if inner[..i].ends_with('\\') { i - 1 } else { i });
            let (target, tail) = match cut {
                Some(i) => (&inner[..i], &inner[i..]),
                None => (inner, ""),
            };
            if points(target.trim(), true) {
                out.push_str(new_wiki);
                out.push_str(tail);
            } else {
                out.push_str(inner);
            }
            out.push_str("]]");
            rest = &rest[inner_from + end + 2..];
        } else {
            out.push_str(&rest[..at + 2]);
            let after = &rest[at + 2..];
            let target = link_target(after);
            if target.is_empty() {
                rest = after;
                continue;
            }
            let lead = after.len() - after.trim_start().len();
            let (bare, suffix) = split_suffix(unwrap_angle(target));
            out.push_str(&after[..lead]);
            if points(&decode(bare), false) {
                out.push_str(&new_target(target, bare, suffix, note_path, to));
            } else {
                out.push_str(target);
            }
            rest = &after[lead + target.len()..];
        }
    }
    out
}

/// The target at the start of a link's parentheses (after `](`, leading spaces skipped):
/// `<…>` whole, else up to whitespace or the closing `)`, balanced parentheses allowed inside.
/// Empty when there is none.
fn link_target(after: &str) -> &str {
    let s = after.trim_start();
    if s.starts_with('<') {
        return s.find('>').map_or("", |end| &s[..end + 1]);
    }
    let mut depth = 0usize;
    for (i, c) in s.char_indices() {
        match c {
            '(' => depth += 1,
            ')' if depth == 0 => return &s[..i],
            ')' => depth -= 1,
            c if c.is_whitespace() => return &s[..i],
            _ => {}
        }
    }
    ""
}

/// `[id]: target` at the start of a line: the byte range of the target.
fn definition_target(line: &str) -> Option<(usize, usize)> {
    let t = line.trim_start();
    if !t.starts_with('[') || t.starts_with("[[") || t.starts_with("[^") {
        return None;
    }
    let close = t.find("]:")?;
    let offset = line.len() - t.len() + close + 2;
    let rest = &line[offset..];
    let lead = rest.len() - rest.trim_start().len();
    let s = &rest[lead..];
    let len =
        if s.starts_with('<') { s.find('>')? + 1 } else { s.find(char::is_whitespace).unwrap_or(s.len()) };
    (len > 0).then_some((offset + lead, offset + lead + len))
}

fn unwrap_angle(target: &str) -> &str {
    target.strip_prefix('<').and_then(|t| t.strip_suffix('>')).unwrap_or(target)
}

/// `path#frag` / `path?q` → (`path`, `#frag`).
fn split_suffix(target: &str) -> (&str, &str) {
    match target.find(['#', '?']) {
        Some(i) => (&target[..i], &target[i..]),
        None => (target, ""),
    }
}

fn decode(target: &str) -> String {
    target.replace("%20", " ")
}

/// The replacement for a link target that named the moved file, written the way the old one
/// was: from the root or relative, `%20` or `<…>` for spaces, its fragment kept.
fn new_target(old: &str, bare: &str, suffix: &str, note_path: &str, to: &str) -> String {
    let path = if bare.starts_with('/') { format!("/{to}") } else { relative_link(note_path, to) };
    let angled = old.starts_with('<');
    if angled {
        return format!("<{path}{suffix}>");
    }
    if bare.contains("%20") || path.contains(' ') && !path.contains(['(', ')']) {
        return format!("{}{suffix}", path.replace(' ', "%20"));
    }
    if path.contains([' ', '(', ')']) {
        return format!("<{path}{suffix}>");
    }
    format!("{path}{suffix}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn paths_the_file_manager_accepts() {
        assert_eq!(file_path("styles/site.scss").as_deref(), Some("styles/site.scss"));
        assert_eq!(file_path("/a/./b.png").as_deref(), Some("a/b.png"));
        assert_eq!(file_path("_quarto.yml").as_deref(), Some("_quarto.yml"));
        for bad in
            ["a/../../x.png", "note.md", "talk.QMD", ".lemmate/x", "a/.git/config", "x.tmp", "", "C:/x"]
        {
            assert_eq!(file_path(bad), None, "{bad}");
        }
    }

    #[test]
    fn relative_links_between_folders() {
        assert_eq!(relative_link("talks/deck.qmd", "talks/img/a.png"), "img/a.png");
        assert_eq!(relative_link("talks/deck.qmd", "attachments/a.png"), "../attachments/a.png");
        assert_eq!(relative_link("deck.md", "styles/a.scss"), "styles/a.scss");
        assert_eq!(relative_link("a/b/c.md", "a/x.png"), "../x.png");
    }

    #[test]
    fn links_images_and_definitions_follow_the_file() {
        let vault = files(&["talks/img/pic.png", "attachments/other.png", "attachments/my pic.png"]);
        let text = "See ![alt](img/pic.png \"t\") and [same](/talks/img/pic.png#x) and ![](other.png).\n\
                    Inline `![](img/pic.png)` stays.\n\n```md\n![](img/pic.png)\n```\n[ref]: img/pic.png\n";
        let out =
            rewrite_references("talks/deck.qmd", text, "talks/img/pic.png", "slides/2026/pic.png", &vault)
                .unwrap();
        assert_eq!(
            out,
            "See ![alt](../slides/2026/pic.png \"t\") and [same](/slides/2026/pic.png#x) and ![](other.png).\n\
             Inline `![](img/pic.png)` stays.\n\n```md\n![](img/pic.png)\n```\n[ref]: ../slides/2026/pic.png\n"
        );
        // Spaces are written the way the link already wrote them.
        let text = "![](../attachments/my%20pic.png) ![](<../attachments/my pic.png>)\n";
        let out = rewrite_references(
            "talks/d.md",
            text,
            "attachments/my pic.png",
            "attachments/new pic.png",
            &vault,
        )
        .unwrap();
        assert_eq!(out, "![](../attachments/new%20pic.png) ![](<../attachments/new pic.png>)\n");
    }

    #[test]
    fn wikilinks_keep_their_alias_and_width() {
        let vault = files(&["attachments/pic.png", "attachments/doc.pdf", "other/doc.pdf"]);
        let text = "![[pic.png|300]] [[pic.png#page=2|see]] ![[Another note]]\n";
        let out = rewrite_references("n.md", text, "attachments/pic.png", "img/diagram.png", &vault).unwrap();
        assert_eq!(out, "![[diagram.png|300]] [[diagram.png#page=2|see]] ![[Another note]]\n");
        // A name another file also has is written as a path, or it would find the wrong one.
        let text = "![[doc.pdf]]\n";
        let out = rewrite_references("n.md", text, "attachments/doc.pdf", "archive/doc.pdf", &vault).unwrap();
        assert_eq!(out, "![[archive/doc.pdf]]\n");
    }

    #[test]
    fn front_matter_paths_follow_too() {
        let vault = files(&["talks/custom.scss", "talks/custom.scss.bak"]);
        let text = "---\ntitle: custom.scss notes\nformat:\n  html:\n    theme: [cosmo, custom.scss]\n    css: \"custom.scss\"\n---\nbody custom.scss\n";
        let out = rewrite_references("talks/deck.qmd", text, "talks/custom.scss", "styles/deck.scss", &vault)
            .unwrap();
        assert_eq!(
            out,
            "---\ntitle: custom.scss notes\nformat:\n  html:\n    theme: [cosmo, ../styles/deck.scss]\n    css: \"../styles/deck.scss\"\n---\nbody custom.scss\n"
        );
    }

    #[test]
    fn a_note_that_does_not_name_the_file_is_untouched() {
        let vault = files(&["talks/pic.png", "other/pic.png"]);
        assert_eq!(
            rewrite_references("other/n.md", "![](pic.png)\n", "talks/pic.png", "x/pic.png", &vault),
            None
        );
        assert_eq!(rewrite_references("n.md", "no links\n", "talks/pic.png", "x/pic.png", &vault), None);
    }

    #[test]
    fn the_listing_joins_entries_marks_and_users() {
        let a: NoteId = "01K5Z0000000000000000000AA".parse().unwrap();
        let b: NoteId = "01K5Z0000000000000000000AB".parse().unwrap();
        let entries = vec![
            ("_quarto.yml".to_owned(), "h0".to_owned()),
            ("styles/site.scss".to_owned(), "h1".to_owned()),
            ("attachments/pic.png".to_owned(), "h2".to_owned()),
        ];
        let users = vec![
            (b, "attachments/pic.png".to_owned()),
            (a, "attachments/pic.png".to_owned()),
            (a, "x".to_owned()),
        ];
        let list = listing(entries, &["styles/site.scss".to_owned()], &users, |p, _| (p != "x").then_some(3));
        assert!(list[0].vault && !list[0].kept && list[0].used_by.is_empty());
        assert!(list[1].kept && !list[1].vault);
        assert_eq!(list[2].used_by, [a.to_string(), b.to_string()]);
        assert_eq!(list[2].size, Some(3));
    }
}

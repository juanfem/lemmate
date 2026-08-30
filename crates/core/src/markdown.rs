//! Markdown indexing for the SPEC §5 dialect: extracts what the relational tables and search
//! index need (title, tags, links, headings, feature flags). This is *not* the renderer; the
//! editor renders in the browser. The JS parser must agree with this one on the `corpus/`
//! cases (SPEC §3.3).

use markdown::mdast::Node;
use markdown::{Constructs, ParseOptions};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoteIndex {
    /// Front-matter `title`, else the first H1, else `None` (caller falls back to the filename).
    pub title: Option<String>,
    pub front_matter: Option<FrontMatter>,
    /// Lower-cased, deduplicated, in first-seen order; merges inline `#tags` and front matter.
    pub tags: Vec<String>,
    pub wikilinks: Vec<WikiLink>,
    /// Destinations of standard `[text](url)` and `![](url)` links, verbatim.
    pub links: Vec<String>,
    pub headings: Vec<Heading>,
    pub has_math: bool,
    pub has_tasks: bool,
    /// Languages of fenced code blocks, braces stripped (`{python}` → `python`), deduplicated.
    pub code_langs: Vec<String>,
    /// Plain text with markup removed; what goes into the FTS `body` column.
    #[serde(default)]
    pub plain_text: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrontMatter {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default, deserialize_with = "one_or_many")]
    pub tags: Vec<String>,
    #[serde(default, deserialize_with = "one_or_many")]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WikiLink {
    pub target: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heading: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default)]
    pub embed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Heading {
    pub depth: u8,
    pub text: String,
}

fn one_or_many<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Vec<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        One(String),
        Many(Vec<String>),
        Null,
    }
    Ok(match OneOrMany::deserialize(d)? {
        OneOrMany::One(s) => s.split(',').map(|t| t.trim().to_owned()).filter(|t| !t.is_empty()).collect(),
        OneOrMany::Many(v) => v,
        OneOrMany::Null => Vec::new(),
    })
}

pub fn parse_options() -> ParseOptions {
    ParseOptions {
        constructs: Constructs { frontmatter: true, math_flow: true, math_text: true, ..Constructs::gfm() },
        ..ParseOptions::gfm()
    }
}

pub fn index(source: &str) -> Result<NoteIndex> {
    let tree = markdown::to_mdast(source, &parse_options()).map_err(|e| Error::Markdown(e.to_string()))?;
    let mut ix = NoteIndex::default();
    let mut plain = String::new();
    walk(&tree, &mut ix, &mut plain);
    ix.plain_text = plain.split_whitespace().collect::<Vec<_>>().join(" ");

    if let Some(fm) = &ix.front_matter {
        if fm.title.is_some() {
            ix.title = fm.title.clone();
        }
        for t in fm.tags.clone() {
            push_tag(&mut ix.tags, &t);
        }
    }
    if ix.title.is_none() {
        ix.title = ix.headings.iter().find(|h| h.depth == 1).map(|h| h.text.clone());
    }
    Ok(ix)
}

fn walk(node: &Node, ix: &mut NoteIndex, plain: &mut String) {
    match node {
        Node::Yaml(y) => {
            // Malformed YAML: keep indexing the body, record an empty front matter.
            ix.front_matter = Some(serde_yaml_ng::from_str::<FrontMatter>(&y.value).unwrap_or_default());
        }
        Node::Heading(h) => {
            let text = inline_text(&h.children);
            scan_inline(&text, ix);
            plain.push_str(&text);
            plain.push('\n');
            collect_links(&h.children, ix);
            ix.headings.push(Heading { depth: h.depth, text });
        }
        Node::Paragraph(p) => {
            let text = inline_text(&p.children);
            scan_inline(&text, ix);
            plain.push_str(&text);
            plain.push('\n');
            collect_links(&p.children, ix);
        }
        Node::Math(_) | Node::InlineMath(_) => ix.has_math = true,
        Node::ListItem(li) if li.checked.is_some() => ix.has_tasks = true,
        Node::Code(c) => {
            if let Some(lang) = &c.lang {
                let lang = lang.trim_matches(|ch| ch == '{' || ch == '}').to_owned();
                if !lang.is_empty() && !ix.code_langs.contains(&lang) {
                    ix.code_langs.push(lang);
                }
            }
        }
        _ => {}
    }
    if let Some(children) = node.children() {
        for c in children {
            walk(c, ix, plain);
        }
    }
}

fn collect_links(nodes: &[Node], ix: &mut NoteIndex) {
    for n in nodes {
        match n {
            Node::Link(l) => ix.links.push(l.url.clone()),
            Node::Image(i) => ix.links.push(i.url.clone()),
            _ => {}
        }
        if let Some(c) = n.children() {
            collect_links(c, ix);
        }
    }
}

/// Concatenated text of inline children, skipping code and math (no tags/links live there).
fn inline_text(nodes: &[Node]) -> String {
    let mut s = String::new();
    for n in nodes {
        match n {
            Node::Text(t) => s.push_str(&t.value),
            Node::InlineCode(_) | Node::InlineMath(_) => s.push(' '),
            Node::Break(_) => s.push('\n'),
            _ => {
                if let Some(c) = n.children() {
                    s.push_str(&inline_text(c));
                }
            }
        }
    }
    s
}

/// Find `#tags` and `[[wikilinks]]` in already-parsed inline text.
fn scan_inline(text: &str, ix: &mut NoteIndex) {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // Wikilink / embed: ![[...]] or [[...]]
        if bytes[i] == b'[' && bytes.get(i + 1) == Some(&b'[') {
            let embed = i > 0 && bytes[i - 1] == b'!';
            if let Some(end) = text[i + 2..].find("]]") {
                let inner = &text[i + 2..i + 2 + end];
                if !inner.is_empty() && !inner.contains("[[") {
                    ix.wikilinks.push(parse_wikilink(inner, embed));
                    i += 2 + end + 2;
                    continue;
                }
            }
        }
        // Tag: '#' at start or after a non-alphanumeric, followed by a body containing a letter.
        if bytes[i] == b'#' {
            let boundary = i == 0 || !text[..i].chars().next_back().is_some_and(char::is_alphanumeric);
            if boundary {
                let body: String = text[i + 1..]
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '/'))
                    .collect();
                if body.chars().any(|c| c.is_alphabetic()) && !body.starts_with('/') {
                    push_tag(&mut ix.tags, &body);
                    i += 1 + body.len();
                    continue;
                }
            }
        }
        i += text[i..].chars().next().map_or(1, char::len_utf8);
    }
}

fn parse_wikilink(inner: &str, embed: bool) -> WikiLink {
    let (target_part, label) = match inner.split_once('|') {
        Some((t, l)) => (t, Some(l.trim().to_owned()).filter(|s| !s.is_empty())),
        None => (inner, None),
    };
    let (target, heading) = match target_part.split_once('#') {
        Some((t, h)) => (t, Some(h.trim().to_owned()).filter(|s| !s.is_empty())),
        None => (target_part, None),
    };
    WikiLink { target: target.trim().to_owned(), heading, label, embed }
}

/// Rewrite `[[old]]`-style links that resolve to `old_path` so they point at `new_path`
/// (SPEC §4.4: renames update links in referring notes). Matches the same forms the resolver
/// accepts — full path, path without extension, basename without extension — and keeps any
/// `#heading` / `|label` suffix. Returns `None` when nothing changed.
pub fn rewrite_wikilinks(text: &str, old_path: &str, new_path: &str) -> Option<String> {
    let strip = |p: &str| p.trim_end_matches(".md").trim_end_matches(".qmd").to_owned();
    let (old_stem, new_stem) = (strip(old_path), strip(new_path));
    let old_base = old_stem.rsplit('/').next().unwrap_or(&old_stem).to_owned();
    let new_base = new_stem.rsplit('/').next().unwrap_or(&new_stem).to_owned();
    let mut out = String::with_capacity(text.len());
    let mut changed = false;
    let mut rest = text;
    while let Some(i) = rest.find("[[") {
        out.push_str(&rest[..i + 2]);
        rest = &rest[i + 2..];
        let Some(end) = rest.find("]]") else { break };
        let inner = &rest[..end];
        let (target, suffix) = match inner.find(['#', '|']) {
            Some(k) => (&inner[..k], &inner[k..]),
            None => (inner, ""),
        };
        let t = target.trim();
        let replacement = if t == old_path || t == old_stem {
            Some(new_stem.as_str())
        } else if t == old_base && old_base != new_base {
            // A bare basename keeps working only while the basename is unique; rewrite it to
            // the new basename so the link still resolves.
            Some(new_base.as_str())
        } else {
            None
        };
        match replacement {
            Some(r) => {
                out.push_str(r);
                out.push_str(suffix);
                changed = true;
            }
            None => out.push_str(inner),
        }
        rest = &rest[end..];
    }
    out.push_str(rest);
    changed.then_some(out)
}

fn push_tag(tags: &mut Vec<String>, tag: &str) {
    let t = tag.trim().trim_matches('/').to_lowercase();
    if !t.is_empty() && !tags.contains(&t) {
        tags.push(t);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_everything() {
        let src = "---\ntitle: My Note\ntags: [alpha, Beta/Gamma]\naliases: nick\n---\n\n# Heading One\n\nSee [[Other Note#Section|label]] and ![[img.png]] plus #inline-tag and #Alpha.\n\nMath $x^2$ and `#notatag` and [link](https://x.y/z).\n\n- [ ] todo\n\n```{python}\nprint(1)\n```\n\n## Sub\n";
        let ix = index(src).unwrap();
        assert_eq!(ix.title.as_deref(), Some("My Note"));
        assert_eq!(ix.tags, vec!["inline-tag", "alpha", "beta/gamma"]);
        assert_eq!(ix.wikilinks.len(), 2);
        assert_eq!(
            ix.wikilinks[0],
            WikiLink {
                target: "Other Note".into(),
                heading: Some("Section".into()),
                label: Some("label".into()),
                embed: false
            }
        );
        assert!(ix.wikilinks[1].embed);
        assert_eq!(ix.links, vec!["https://x.y/z"]);
        assert!(ix.has_math && ix.has_tasks);
        assert_eq!(ix.code_langs, vec!["python"]);
        assert_eq!(ix.headings.len(), 2);
        assert_eq!(ix.front_matter.as_ref().unwrap().aliases, vec!["nick"]);
        assert!(ix.plain_text.contains("Heading One"));
    }

    #[test]
    fn rewrites_links_on_rename() {
        let t = "see [[Projects/Plan]] and [[Plan|the plan]] and [[Projects/Plan.md#Goals]] but not [[Planning]] or `[[Plan]]`\n";
        let r = rewrite_wikilinks(t, "Projects/Plan.md", "Archive/Roadmap.md").unwrap();
        assert_eq!(
            r,
            "see [[Archive/Roadmap]] and [[Roadmap|the plan]] and [[Archive/Roadmap#Goals]] but not [[Planning]] or `[[Roadmap]]`\n"
        );
        assert_eq!(rewrite_wikilinks("nothing here", "a.md", "b.md"), None);
        // Same basename, different folder: bare basenames stay.
        assert_eq!(
            rewrite_wikilinks("[[Plan]] [[Projects/Plan]]", "Projects/Plan.md", "Done/Plan.md").unwrap(),
            "[[Plan]] [[Done/Plan]]"
        );
    }

    #[test]
    fn title_falls_back_to_h1() {
        assert_eq!(index("# Hello\n\ntext").unwrap().title.as_deref(), Some("Hello"));
        assert_eq!(index("just text").unwrap().title, None);
    }

    #[test]
    fn numeric_and_heading_hashes_are_not_tags() {
        let ix = index("# Not a tag\n\nissue #123 and #1a is a tag\n").unwrap();
        assert_eq!(ix.tags, vec!["1a"]);
    }

    /// Conformance corpus shared with the JS parser: `corpus/<name>.md` ↔ `corpus/<name>.json`.
    #[test]
    fn corpus() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus");
        let mut checked = 0;
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|e| e == "md")
                && path.file_name().is_some_and(|n| n != "README.md")
            {
                let expected_path = path.with_extension("json");
                let raw = std::fs::read_to_string(&expected_path)
                    .unwrap_or_else(|_| panic!("missing {}", expected_path.display()));
                let expected: NoteIndex = serde_json::from_str(&raw).unwrap();
                let mut got = index(&std::fs::read_to_string(&path).unwrap()).unwrap();
                got.plain_text.clear(); // not part of the cross-parser contract
                assert_eq!(got, expected, "corpus case {}", path.display());
                checked += 1;
            }
        }
        assert!(checked > 0, "no corpus cases found in {}", dir.display());
    }
}

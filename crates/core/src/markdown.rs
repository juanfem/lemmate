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

/// What [`index`] would derive from the same text, versioned. Bump it whenever a change to the
/// indexer derives something different from text that has not changed — a construct it starts
/// or stops looking inside — so the stores built by the old one are re-derived on their next
/// start ([`crate::store::Store::index_is_current`]).
///
/// 2: table cells are indexed like paragraphs.
/// 3: links inside raw HTML blocks, and `src="…"` attributes, are links.
/// 4: autolinked URLs hold no tags; tags are NFC; over-deep nesting is tamed ([`tame`]).
pub const INDEX_VERSION: u32 = 4;

/// Container markers (`>`, list bullets and numbers) a line may open before the rest is text.
pub const MAX_CONTAINERS: usize = 32;
/// Columns of whitespace (a tab counts 4) a line's container prefix may hold.
pub const MAX_INDENT: usize = 128;
/// Emphasis delimiters (`*`, `~`, and `_` not inside a word) a block may hold.
pub const MAX_EMPHASIS: usize = 500;
/// How deep `[` may nest within a block — and how many `]` closing nothing it may hold, each of
/// which sends the parser looking back through the whole block for a `[`.
pub const MAX_BRACKETS: usize = 32;

/// The source with pathological nesting defused, before either parser sees it. Each level of
/// nesting is a level of recursion — in the parsers' trees and in whatever walks them — and the
/// parsers' work grows with the square of the depth, so a note of ten thousand `>` or `*` would
/// overflow the stack or take minutes. No real note comes near these limits; past them, markers
/// are escaped with `\` and read as text. A *block* here is a run of non-blank lines.
///
/// `ui/src/markdown/index.ts` has the same function, so both indexers still read the same text.
pub fn tame(source: &str) -> std::borrow::Cow<'_, str> {
    let mut out: Vec<u8> = Vec::with_capacity(source.len());
    let mut changed = false;
    let (mut emphasis, mut brackets, mut stray) = (0usize, 0usize, 0usize);
    for line in source.split_inclusive('\n') {
        let b = line.as_bytes();
        if b.iter().all(|c| matches!(c, b' ' | b'\t' | b'\r' | b'\n')) {
            (emphasis, brackets, stray) = (0, 0, 0);
            out.extend_from_slice(b);
            continue;
        }
        let (mut i, mut ws, mut markers) = (0, 0, 0);
        loop {
            let start = i;
            let mut cols = 0;
            while i < b.len() && matches!(b[i], b' ' | b'\t') {
                cols += if b[i] == b'\t' { 4 } else { 1 };
                i += 1;
            }
            if ws + cols > MAX_INDENT {
                // At least one space stays, or the marker before it would stop being one.
                let keep = MAX_INDENT.saturating_sub(ws).max(1);
                out.resize(out.len() + keep, b' ');
                ws += keep;
                changed = true;
            } else {
                out.extend_from_slice(&b[start..i]);
                ws += cols;
            }
            let m = container_marker(b, i);
            if m == 0 {
                break;
            }
            out.extend_from_slice(&b[i..i + m - 1]);
            if markers == MAX_CONTAINERS {
                out.push(b'\\');
                changed = true;
            }
            out.push(b[i + m - 1]);
            i += m;
            if markers == MAX_CONTAINERS {
                break;
            }
            markers += 1;
        }
        while i < b.len() {
            let c = b[i];
            if c == b'\\' && b.get(i + 1).is_some_and(u8::is_ascii_punctuation) {
                out.extend_from_slice(&b[i..i + 2]);
                i += 2;
                continue;
            }
            let escape = match c {
                b'*' | b'~' => counted(&mut emphasis, MAX_EMPHASIS),
                b'_' if !(i > 0
                    && b[i - 1].is_ascii_alphanumeric()
                    && b.get(i + 1).is_some_and(u8::is_ascii_alphanumeric)) =>
                {
                    counted(&mut emphasis, MAX_EMPHASIS)
                }
                b'[' => counted(&mut brackets, MAX_BRACKETS),
                b']' if brackets == 0 => counted(&mut stray, MAX_BRACKETS),
                b']' => {
                    brackets -= 1;
                    false
                }
                _ => false,
            };
            if escape {
                out.push(b'\\');
                changed = true;
            }
            out.push(c);
            i += 1;
        }
    }
    if !changed {
        return std::borrow::Cow::Borrowed(source);
    }
    // Only ASCII was inserted, between whole characters.
    std::borrow::Cow::Owned(String::from_utf8(out).expect("still UTF-8"))
}

/// Count one more against `limit`; whether this one is past it (and so to be escaped).
fn counted(n: &mut usize, limit: usize) -> bool {
    if *n >= limit {
        return true;
    }
    *n += 1;
    false
}

/// The length of the container marker at `i` — `>`, `-`/`+`/`*` before a space, or up to nine
/// digits and `.`/`)` before one — or 0. Its last byte is the punctuation an escape goes before.
fn container_marker(b: &[u8], i: usize) -> usize {
    let spaced = |at: usize| matches!(b.get(at), None | Some(b' ' | b'\t' | b'\r' | b'\n'));
    match b.get(i) {
        Some(b'>') => 1,
        Some(b'-' | b'+' | b'*') if spaced(i + 1) => 1,
        Some(c) if c.is_ascii_digit() => {
            let n = b[i..].iter().take_while(|c| c.is_ascii_digit()).count();
            if n <= 9 && matches!(b.get(i + n), Some(b'.' | b')')) && spaced(i + n + 1) { n + 1 } else { 0 }
        }
        _ => 0,
    }
}

pub fn index(source: &str) -> Result<NoteIndex> {
    let source = tame(source);
    let tree = markdown::to_mdast(&source, &parse_options()).map_err(|e| Error::Markdown(e.to_string()))?;
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

/// Pre-order over the tree, with an explicit stack: however deep a note nests, this does not
/// recurse.
fn walk(root: &Node, ix: &mut NoteIndex, plain: &mut String) {
    // Each node, and whether its parent holds blocks (so a raw HTML child is read for links).
    let mut stack: Vec<(&Node, bool)> = vec![(root, false)];
    while let Some((node, in_blocks)) = stack.pop() {
        if in_blocks && let Node::Html(h) = node {
            html_block_links(&h.value, ix);
        }
        match node {
            Node::Yaml(y) => {
                // Malformed YAML: keep indexing the body, record an empty front matter.
                ix.front_matter = Some(serde_yaml_ng::from_str::<FrontMatter>(&y.value).unwrap_or_default());
            }
            Node::Heading(h) => {
                let (text, scan) = inline_text(&h.children);
                scan_inline(&scan, ix);
                plain.push_str(&text);
                plain.push('\n');
                collect_links(&h.children, ix);
                ix.headings.push(Heading { depth: h.depth, text });
            }
            // A table cell holds inline content just as a paragraph does, and the same links and tags.
            Node::Paragraph(_) | Node::TableCell(_) => {
                let children = node.children().map_or(&[][..], Vec::as_slice);
                let (text, scan) = inline_text(children);
                scan_inline(&scan, ix);
                plain.push_str(&text);
                plain.push('\n');
                collect_links(children, ix);
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
            let holds_blocks = matches!(
                node,
                Node::Root(_) | Node::Blockquote(_) | Node::ListItem(_) | Node::FootnoteDefinition(_)
            );
            stack.extend(children.iter().rev().map(|c| (c, holds_blocks)));
        }
    }
}

/// The links in a raw HTML block. CommonMark runs a block that opens with a tag like `<p>` or
/// `<div>` on to the next blank line, so a Quarto slide's
///
/// ```text
/// <p class="cite">…</p>
/// :::
/// ![](figures/plot.png)
/// ```
///
/// is all HTML, image included. Pandoc — what Quarto renders with — reads the HTML as HTML and
/// the lines after it as markdown, and shows the image. Read again without HTML blocks, the
/// tags are inline HTML and the image is an image: what pandoc would find, it finds.
fn html_block_links(html: &str, ix: &mut NoteIndex) {
    let mut options = parse_options();
    options.constructs.html_flow = false;
    if let Ok(tree) = markdown::to_mdast(html, &options)
        && let Some(children) = tree.children()
    {
        collect_links(children, ix);
    }
}

fn collect_links(nodes: &[Node], ix: &mut NoteIndex) {
    let mut stack: Vec<&Node> = nodes.iter().rev().collect();
    while let Some(n) = stack.pop() {
        match n {
            Node::Link(l) => ix.links.push(l.url.clone()),
            Node::Image(i) => ix.links.push(i.url.clone()),
            Node::Html(h) => ix.links.extend(src_attributes(&h.value)),
            _ => {}
        }
        if let Some(c) = n.children() {
            stack.extend(c.iter().rev());
        }
    }
}

/// The `src` attribute values in a run of HTML — `<img src="pic.png">` names a file just as
/// `![](pic.png)` does. Quoted or bare; the name is matched without regard to ASCII case.
fn src_attributes(html: &str) -> Vec<String> {
    let lower = html.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(at) = lower[from..].find("src").map(|i| i + from) {
        from = at + 3;
        if at == 0 || !bytes[at - 1].is_ascii_whitespace() {
            continue;
        }
        let mut i = at + 3;
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if bytes.get(i) != Some(&b'=') {
            continue;
        }
        i += 1;
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let (start, end) = match bytes.get(i) {
            Some(&q @ (b'"' | b'\'')) => match lower[i + 1..].find(q as char) {
                Some(len) => (i + 1, i + 1 + len),
                None => continue,
            },
            Some(_) => {
                let len =
                    lower[i..].find(|c: char| c.is_ascii_whitespace() || c == '>').unwrap_or(lower.len() - i);
                (i, i + len)
            }
            None => continue,
        };
        if end > start {
            out.push(html[start..end].to_owned());
        }
        from = end;
    }
    out
}

/// Concatenated text of inline children, skipping code and math (no tags/links live there):
/// the text itself, and the same with every autolinked URL blanked out — what is scanned for
/// tags and wikilinks, so that `https://example.com/#anchor` does not tag the note `anchor`.
fn inline_text(nodes: &[Node]) -> (String, String) {
    let (mut s, mut scan) = (String::new(), String::new());
    let mut stack: Vec<(&Node, bool)> = nodes.iter().rev().map(|n| (n, false)).collect();
    while let Some((n, url)) = stack.pop() {
        match n {
            Node::Text(t) => {
                s.push_str(&t.value);
                scan.push_str(if url { " " } else { &t.value });
            }
            Node::InlineCode(_) | Node::InlineMath(_) => {
                s.push(' ');
                scan.push(' ');
            }
            Node::Break(_) => {
                s.push('\n');
                scan.push('\n');
            }
            _ => {
                let url = url || matches!(n, Node::Link(l) if is_autolink(&l.url, &l.children));
                if let Some(c) = n.children() {
                    stack.extend(c.iter().rev().map(|c| (c, url)));
                }
            }
        }
    }
    (s, scan)
}

/// Whether a link is its own URL written out — `<https://…>`, `<a@b.c>`, or a bare URL or `www.`
/// address GFM links by itself — rather than text someone wrote for it.
fn is_autolink(url: &str, children: &[Node]) -> bool {
    let [Node::Text(t)] = children else { return false };
    let text = t.value.as_str();
    url == text || ["http://", "mailto:"].iter().any(|p| url.strip_prefix(p) == Some(text))
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
        // The body is read composed (NFC), so `#áb` typed as `a` + a combining accent is `áb`.
        if bytes[i] == b'#' {
            let boundary = i == 0 || !text[..i].chars().next_back().is_some_and(char::is_alphanumeric);
            if boundary {
                let word = &text[i + 1..];
                let word = &word[..word
                    .find(|c: char| {
                        c.is_ascii() && !(c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '/'))
                    })
                    .unwrap_or(word.len())];
                let composed = icu_normalizer::ComposingNormalizerBorrowed::new_nfc().normalize(word);
                let body: String = composed
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '/'))
                    .collect();
                if body.chars().any(|c| c.is_alphabetic()) && !body.starts_with('/') {
                    push_tag(&mut ix.tags, &body);
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

fn strip_note_ext(p: &str) -> &str {
    p.strip_suffix(".md").or_else(|| p.strip_suffix(".qmd")).unwrap_or(p)
}

fn folder_of(p: &str) -> &str {
    p.rsplit_once('/').map_or("", |(dir, _)| dir)
}

/// Which of the vault's note `paths` the wikilink `target`, written in the note at `from`,
/// reaches (SPEC §5.4). An exact path wins, with or without its extension. Otherwise a bare
/// name reaches the note of that name — and where several share it, the one nearest `from`:
/// the most folders in common, then the shallowest, then the first by path. A target with a
/// folder in it is a path and nothing else. `ui/src/lib/links.ts` holds the same rule.
pub fn resolve_wikilink<'a>(target: &str, from: &str, paths: &[&'a str]) -> Option<&'a str> {
    let t = target.trim();
    let exact = |want: &str| paths.iter().copied().find(|p| *p == want);
    if t.ends_with(".md") || t.ends_with(".qmd") {
        if let Some(p) = exact(t) {
            return Some(p);
        }
    } else if let Some(p) =
        exact(t).or_else(|| exact(&format!("{t}.md"))).or_else(|| exact(&format!("{t}.qmd")))
    {
        return Some(p);
    }
    if t.contains('/') {
        return None;
    }
    let name = strip_note_ext(t);
    let here: Vec<&str> = folder_of(from).split('/').filter(|s| !s.is_empty()).collect();
    let shared = |p: &str| {
        folder_of(p).split('/').filter(|s| !s.is_empty()).zip(&here).take_while(|(a, b)| a == *b).count()
    };
    paths.iter().copied().filter(|p| strip_note_ext(p.rsplit('/').next().unwrap_or(p)) == name).min_by(
        |a, b| {
            shared(b).cmp(&shared(a)).then(a.matches('/').count().cmp(&b.matches('/').count())).then(a.cmp(b))
        },
    )
}

/// Rewrite `[[old]]`-style links that resolve to `old_path` so they point at `new_path`
/// (SPEC §4.4: renames update links in referring notes). A full path, or one without its
/// extension, becomes the new path. A bare name is the note's only where `bare.before` says it
/// reached it from the note being rewritten — another note of that name may be nearer there —
/// and it stays bare only while `bare.after` says the new name still reaches it; otherwise it is
/// qualified with the new path. Any `#heading` / `|label` suffix is kept. Returns `None` when
/// nothing changed.
pub fn rewrite_wikilinks(text: &str, old_path: &str, new_path: &str, bare: BareName) -> Option<String> {
    let strip = |p: &str| strip_note_ext(p).to_owned();
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
        // Inside a table the alias pipe is escaped, `[[Plan\|label]]`; the `\` goes with the suffix.
        let (target, suffix) = match inner.find(['#', '|']) {
            Some(k) if inner[..k].ends_with('\\') && inner[k..].starts_with('|') => {
                (&inner[..k - 1], &inner[k - 1..])
            }
            Some(k) => (&inner[..k], &inner[k..]),
            None => (inner, ""),
        };
        let t = target.trim();
        let replacement = if t == old_path || t == old_stem {
            Some(new_stem.as_str())
        } else if t == old_base && bare.before {
            if !bare.after {
                Some(new_stem.as_str())
            } else if old_base != new_base {
                Some(new_base.as_str())
            } else {
                None
            }
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

/// Whether a bare name, written in the note a rename rewrites, reached the renamed note
/// `before` the rename, and whether its new name reaches it `after` (see [`resolve_wikilink`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BareName {
    pub before: bool,
    pub after: bool,
}

impl BareName {
    /// From the note at `from`: `before` against the vault's paths with the note at `old`,
    /// `after` against `paths`, which hold it at `new`.
    pub fn of(from: &str, old: &str, new: &str, paths: &[&str]) -> Self {
        let name = |p: &str| strip_note_ext(p.rsplit('/').next().unwrap_or(p)).to_owned();
        let before: Vec<&str> = paths.iter().map(|p| if *p == new { old } else { p }).collect();
        BareName {
            before: resolve_wikilink(&name(old), from, &before) == Some(old),
            after: resolve_wikilink(&name(new), from, paths) == Some(new),
        }
    }
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
        let r = rewrite_wikilinks(t, "Projects/Plan.md", "Archive/Roadmap.md", BARE).unwrap();
        assert_eq!(
            r,
            "see [[Archive/Roadmap]] and [[Roadmap|the plan]] and [[Archive/Roadmap#Goals]] but not [[Planning]] or `[[Roadmap]]`\n"
        );
        assert_eq!(rewrite_wikilinks("nothing here", "a.md", "b.md", BARE), None);
        // Same basename, different folder: bare basenames stay.
        assert_eq!(
            rewrite_wikilinks("[[Plan]] [[Projects/Plan]]", "Projects/Plan.md", "Done/Plan.md", BARE)
                .unwrap(),
            "[[Plan]] [[Done/Plan]]"
        );
        // A table cell escapes the alias pipe.
        assert_eq!(
            rewrite_wikilinks("| [[Plan\\|the plan]] |", "Projects/Plan.md", "Archive/Roadmap.md", BARE)
                .unwrap(),
            "| [[Roadmap\\|the plan]] |"
        );
    }

    /// A bare name that reached the note before the rename and still does after it.
    const BARE: BareName = BareName { before: true, after: true };

    #[test]
    fn a_bare_name_reaches_the_nearest_note_of_that_name() {
        let paths = [
            "Plan.md",
            "Projects/Plan.md",
            "Projects/A/Plan.md",
            "Archive/Old/Plan.md",
            "Talks/Deck.qmd",
            "Solo.md",
        ];
        let r = |t: &str, from: &str| resolve_wikilink(t, from, &paths);
        // An exact path wins, with or without the extension, from anywhere.
        assert_eq!(r("Plan", "Projects/A/x.md"), Some("Plan.md"));
        assert_eq!(r("Projects/Plan", "x.md"), Some("Projects/Plan.md"));
        assert_eq!(r("Talks/Deck", "x.md"), Some("Talks/Deck.qmd"));
        assert_eq!(r("Solo", "Deep/er/x.md"), Some("Solo.md"));
        // Without one, the name: the same folder, then the most folders in common, then the
        // shallowest, then the first by path.
        let paths = ["Projects/Plan.md", "Projects/A/Plan.md", "Archive/Old/Plan.md", "Archive/New/Plan.md"];
        let r = |t: &str, from: &str| resolve_wikilink(t, from, &paths);
        assert_eq!(r("Plan", "Projects/A/x.md"), Some("Projects/A/Plan.md"));
        assert_eq!(r("Plan", "Projects/x.md"), Some("Projects/Plan.md"));
        assert_eq!(r("Plan", "Projects/B/x.md"), Some("Projects/Plan.md"));
        assert_eq!(r("Plan", "Archive/Old/Deep/x.md"), Some("Archive/Old/Plan.md"));
        assert_eq!(r("Plan.md", "Archive/x.md"), Some("Archive/New/Plan.md"));
        assert_eq!(r("Plan", "x.md"), Some("Projects/Plan.md"));
        // A folder makes it a path, and a path that is not there reaches nothing.
        assert_eq!(r("A/Plan", "Projects/x.md"), None);
        assert_eq!(r("Nothing", "x.md"), None);
    }

    #[test]
    fn a_rename_leaves_bare_names_that_meant_another_note_alone() {
        let text = "[[Plan]] and [[Projects/Plan]]";
        // The bare one reached Archive/Plan from here: only the path follows the rename.
        let not_ours = BareName { before: false, after: false };
        assert_eq!(
            rewrite_wikilinks(text, "Projects/Plan.md", "Done/Roadmap.md", not_ours).unwrap(),
            "[[Plan]] and [[Done/Roadmap]]"
        );
        // It was ours, but after the move a nearer Plan would take it: qualified instead.
        let lost = BareName { before: true, after: false };
        assert_eq!(
            rewrite_wikilinks(text, "Projects/Plan.md", "Done/Plan.md", lost).unwrap(),
            "[[Done/Plan]] and [[Done/Plan]]"
        );
        // Worked out from the paths: from Archive/, the Plan there is nearer than Projects'.
        assert_eq!(
            BareName::of(
                "Archive/x.md",
                "Projects/Plan.md",
                "Projects/Roadmap.md",
                &["Projects/Roadmap.md", "Archive/Plan.md"]
            ),
            BareName { before: false, after: true }
        );
        assert_eq!(
            BareName::of(
                "Projects/x.md",
                "Projects/Plan.md",
                "Done/Plan.md",
                &["Done/Plan.md", "Archive/Plan.md"]
            ),
            BareName { before: true, after: false }
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

    /// Notes nested ten thousand deep — in quotes, emphasis, brackets, lists — index on a 2 MiB
    /// stack (a thread's default) and in bounded time, rather than aborting the process.
    #[test]
    fn deep_nesting_neither_overflows_nor_hangs() {
        let cases: Vec<(&str, String)> = vec![
            ("quotes", format!("{} x #end\n", ">".repeat(10_000))),
            ("emphasis", format!("{}x{} #end\n", "*".repeat(10_000), "*".repeat(10_000))),
            ("spaced emphasis", format!("{}x{} #end\n", "*a ".repeat(10_000), " a*".repeat(10_000))),
            ("strikethrough", format!("{}x{} #end\n", "~~a ".repeat(5_000), " a~~".repeat(5_000))),
            ("brackets", format!("{}x{} #end\n", "[".repeat(10_000), "]".repeat(10_000))),
            ("images", format!("{}x{} #end\n", "![".repeat(10_000), "](u)".repeat(10_000))),
            ("list markers", format!("{}x #end\n", "- ".repeat(10_000))),
            (
                "indented lists",
                (0..300).map(|i| format!("{}- x\n", "  ".repeat(i))).collect::<String>() + "\n#end\n",
            ),
            (
                "quote lines",
                (0..300).map(|i| format!("{}x\n", "> ".repeat(i))).collect::<String>() + "\n#end\n",
            ),
        ];
        for (name, src) in cases {
            let started = std::time::Instant::now();
            let ix = std::thread::Builder::new()
                .stack_size(2 << 20)
                .spawn(move || index(&src).unwrap())
                .unwrap()
                .join()
                .unwrap_or_else(|_| panic!("{name}: the indexer crashed"));
            assert!(
                started.elapsed() < std::time::Duration::from_secs(20),
                "{name}: {:?}",
                started.elapsed()
            );
            assert!(ix.tags.contains(&"end".to_owned()), "{name}: {:?}", ix.tags);
        }
    }

    #[test]
    fn taming_leaves_ordinary_notes_alone() {
        let note = "---\ntitle: T\n---\n> > quoted *em* _em_ snake_case [[link]] [a](b)\n\n- a\n  - b\n";
        assert!(matches!(tame(note), std::borrow::Cow::Borrowed(_)));
        let deep = format!("{}x\n", ">".repeat(MAX_CONTAINERS + 5));
        assert_eq!(tame(&deep), format!("{}\\>{}x\n", ">".repeat(MAX_CONTAINERS), ">".repeat(4)));
        // Already-escaped markers are not escaped twice, and a blank line starts a fresh count.
        let stars = format!("{}\\*\n\n*a*\n", "*".repeat(MAX_EMPHASIS + 1));
        assert_eq!(tame(&stars), format!("{}\\*\\*\n\n*a*\n", "*".repeat(MAX_EMPHASIS)));
        let indented = format!("{}- x\n", " ".repeat(MAX_INDENT + 50));
        assert_eq!(tame(&indented), format!("{}- x\n", " ".repeat(MAX_INDENT)));
    }

    #[test]
    fn urls_hold_no_tags() {
        let ix = index(
            "https://example.com/#anchor <https://a.b/#c> www.x.org/#w <me@x.org> \
             [label #kept](https://y.z/#gone) #real\n",
        )
        .unwrap();
        assert_eq!(ix.tags, vec!["kept", "real"]);
        assert_eq!(ix.links[0], "https://example.com/#anchor");
    }

    #[test]
    fn tags_are_composed() {
        assert_eq!(index("#cafe\u{301} and #a\u{301}b\n").unwrap().tags, vec!["caf\u{e9}", "\u{e1}b"]);
    }

    #[test]
    fn scalar_titles_and_ids_are_text() {
        let ix = index("---\ntitle: 2024\nid: 0123\n---\n# H\n").unwrap();
        assert_eq!(ix.title.as_deref(), Some("2024"));
        assert_eq!(ix.front_matter.unwrap().id.as_deref(), Some("0123"));
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

//! Obsidian vault import (SPEC §11.4).
//!
//! Copies an Obsidian vault into a notes vault directory, preserving folder structure and
//! filenames, and rewriting the two pieces of Obsidian-only syntax the dialect (SPEC §5.3,
//! §5.4) replaces: blockquote callouts become Quarto fenced divs, and image embeds become
//! standard markdown images. Wikilinks, tags, maths, and front matter are left alone — the
//! sync engine assigns note ids on first sync, so nothing is written into front matter here.
//!
//! Vault-level Obsidian settings that have a home in this app are translated into the sidecar
//! as `*.import.json` files for the vault doc to pick up later: bookmarks and daily notes.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::SIDECAR_DIR;
use crate::error::{Error, Result};
use crate::projection::NOTE_EXTENSIONS;

/// Directory entries that are never imported, on top of the general "hidden" rule.
const SKIP_DIRS: &[&str] = &[".obsidian", ".trash", SIDECAR_DIR];

/// Embed targets with these extensions become `![](…)`; everything else stays an `![[…]]`
/// embed for tier-3 transclusion.
const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "gif", "svg", "webp", "bmp", "avif"];

#[derive(Debug, Clone, Default)]
pub struct ImportOptions {
    /// Overwrite files that already exist in the destination instead of skipping them.
    pub overwrite: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportReport {
    /// `.md`/`.qmd` files copied.
    pub notes: usize,
    /// Non-note files copied.
    pub attachments: usize,
    /// Obsidian callouts turned into Quarto fenced divs.
    pub callouts: usize,
    /// `![[image]]` embeds rewritten to `![](image)`.
    pub embeds: usize,
    /// Files left alone because the destination already had them.
    pub skipped: usize,
    /// Bookmarks read from `.obsidian/bookmarks.json`.
    pub bookmarks: usize,
    /// Whether `.obsidian/daily-notes.json` was translated.
    pub daily_notes: bool,
}

impl fmt::Display for ImportReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "imported {} note{}, {} attachment{} ({} callout{} converted, {} embed{} rewritten)",
            self.notes,
            plural(self.notes),
            self.attachments,
            plural(self.attachments),
            self.callouts,
            plural(self.callouts),
            self.embeds,
            plural(self.embeds),
        )?;
        if self.skipped > 0 {
            write!(f, "; {} file{} skipped (already present)", self.skipped, plural(self.skipped))?;
        }
        if self.bookmarks > 0 {
            write!(
                f,
                "; {} bookmark{} written to {SIDECAR_DIR}/{BOOKMARKS_FILE}",
                self.bookmarks,
                plural(self.bookmarks)
            )?;
        }
        if self.daily_notes {
            write!(f, "; daily-notes settings written to {SIDECAR_DIR}/{DAILY_FILE}")?;
        }
        Ok(())
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// Sidecar file holding the imported bookmark list.
pub const BOOKMARKS_FILE: &str = "bookmarks.import.json";
/// Sidecar file holding the imported daily-note settings.
pub const DAILY_FILE: &str = "daily.import.json";

/// Imported bookmarks are the vault doc's own bookmarks (SPEC §4.3): the sidecar file and the
/// CRDT list hold the same shape. Obsidian's search/graph/heading bookmarks are dropped, so
/// everything the importer produces has `kind: "note"`.
pub use crate::vault_doc::Bookmark;

/// Imported daily-note settings (`.obsidian/daily-notes.json`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DailySettings {
    /// Folder new daily notes go into ("" = vault root).
    pub folder: String,
    /// Moment.js-style date format Obsidian used for the filename.
    pub format: String,
}

/// Import the Obsidian vault at `src` into the notes vault at `dest`.
pub fn import_obsidian(src: &Path, dest: &Path, opts: &ImportOptions) -> Result<ImportReport> {
    if !src.is_dir() {
        return Err(Error::Import(format!("{} is not a directory", src.display())));
    }
    fs::create_dir_all(dest)?;
    let mut report = ImportReport::default();
    import_dir(src, dest, opts, &mut report)?;
    import_bookmarks(src, dest, opts, &mut report)?;
    import_daily_notes(src, dest, opts, &mut report)?;
    Ok(report)
}

fn import_dir(src: &Path, dest: &Path, opts: &ImportOptions, report: &mut ImportReport) -> Result<()> {
    let mut entries: Vec<PathBuf> = Vec::new();
    for entry in fs::read_dir(src)? {
        entries.push(entry?.path());
    }
    entries.sort();
    for path in entries {
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n,
            None => continue,
        };
        if name.starts_with('.') || SKIP_DIRS.contains(&name) {
            continue;
        }
        let target = dest.join(name);
        if path.is_dir() {
            fs::create_dir_all(&target)?;
            import_dir(&path, &target, opts, report)?;
        } else if path.is_file() {
            if is_note(&path) {
                import_note(&path, &target, opts, report)?;
            } else {
                if !claim(&target, opts, report)? {
                    continue;
                }
                fs::copy(&path, &target)?;
                report.attachments += 1;
            }
        }
    }
    Ok(())
}

fn import_note(src: &Path, dest: &Path, opts: &ImportOptions, report: &mut ImportReport) -> Result<()> {
    if !claim(dest, opts, report)? {
        return Ok(());
    }
    let text = fs::read_to_string(src)?;
    let converted = convert_note(&text);
    fs::write(dest, converted.text)?;
    report.notes += 1;
    report.callouts += converted.callouts;
    report.embeds += converted.embeds;
    Ok(())
}

/// May we write `target`? Records a skip in the report when we may not.
fn claim(target: &Path, opts: &ImportOptions, report: &mut ImportReport) -> Result<bool> {
    if target.exists() && !opts.overwrite {
        report.skipped += 1;
        return Ok(false);
    }
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    Ok(true)
}

fn is_note(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .is_some_and(|e| NOTE_EXTENSIONS.contains(&e.as_str()))
}

// ---------------------------------------------------------------------------------------------
// Text conversion
// ---------------------------------------------------------------------------------------------

/// One note's text after conversion, with what changed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Converted {
    pub text: String,
    pub callouts: usize,
    pub embeds: usize,
}

/// Rewrite Obsidian-only syntax in a note's markdown. Front matter and fenced code blocks are
/// copied through untouched; wikilinks, tags, and maths are never rewritten.
pub fn convert_note(text: &str) -> Converted {
    let mut out: Vec<String> = Vec::new();
    let mut callouts = 0;
    let mut embeds = 0;
    let lines: Vec<&str> = text.lines().collect();
    let mut i = 0;

    // Front matter: a `---` on line 1 up to the next `---`/`...` line, verbatim.
    if lines.first().is_some_and(|l| l.trim_end() == "---") {
        out.push(lines[0].to_owned());
        i = 1;
        while i < lines.len() {
            let line = lines[i];
            out.push(line.to_owned());
            i += 1;
            if line.trim_end() == "---" || line.trim_end() == "..." {
                break;
            }
        }
    }

    let mut fence: Option<(char, usize)> = None;
    while i < lines.len() {
        let line = lines[i];
        match fence {
            Some((ch, n)) => {
                out.push(line.to_owned());
                if closes_fence(line, ch, n) {
                    fence = None;
                }
                i += 1;
                continue;
            }
            None => {
                if let Some(open) = fence_marker(line) {
                    fence = Some(open);
                    out.push(line.to_owned());
                    i += 1;
                    continue;
                }
            }
        }
        if let Some(head) = parse_callout(line) {
            let indent = &line[..line.len() - line.trim_start().len()];
            out.push(format!("{indent}{}", head.open()));
            i += 1;
            while i < lines.len() {
                let body = lines[i];
                match strip_quote(body) {
                    Some(inner) => {
                        out.push(rewrite_embeds(inner, &mut embeds));
                        i += 1;
                    }
                    None => break,
                }
            }
            out.push(format!("{indent}:::"));
            callouts += 1;
            continue;
        }
        out.push(rewrite_embeds(line, &mut embeds));
        i += 1;
    }

    let mut text_out = out.join("\n");
    if text.ends_with('\n') || text.is_empty() {
        text_out.push('\n');
    }
    Converted { text: text_out, callouts, embeds }
}

/// A parsed `> [!kind]± Title` header line.
struct CalloutHead {
    kind: &'static str,
    title: String,
    collapsed: bool,
}

impl CalloutHead {
    fn open(&self) -> String {
        let mut s = format!("::: {{.callout-{}", self.kind);
        if !self.title.is_empty() {
            s.push_str(&format!(" title=\"{}\"", self.title.replace('"', "\\\"")));
        }
        if self.collapsed {
            s.push_str(" collapse=\"true\"");
        }
        s.push('}');
        s
    }
}

fn parse_callout(line: &str) -> Option<CalloutHead> {
    let rest = line.trim_start().strip_prefix('>')?;
    // `>> [!x]` is a nested blockquote, not a callout we convert.
    let rest = rest.trim_start();
    let rest = rest.strip_prefix("[!")?;
    let end = rest.find(']')?;
    let kind = rest[..end].trim().to_ascii_lowercase();
    if kind.is_empty() {
        return None;
    }
    let after = &rest[end + 1..];
    let (collapsed, after) = match after.strip_prefix('-') {
        Some(a) => (true, a),
        None => (false, after.strip_prefix('+').unwrap_or(after)),
    };
    Some(CalloutHead { kind: map_kind(&kind), title: after.trim().to_owned(), collapsed })
}

/// Obsidian callout kinds → the five Quarto kinds this dialect supports (SPEC §5.3).
fn map_kind(kind: &str) -> &'static str {
    match kind {
        "tip" | "hint" | "success" => "tip",
        "warning" => "warning",
        "caution" => "caution",
        "important" => "important",
        _ => "note",
    }
}

/// A callout continuation line with its `> ` (or bare `>`) removed, or `None` if the
/// blockquote has ended.
fn strip_quote(line: &str) -> Option<&str> {
    let rest = line.trim_start().strip_prefix('>')?;
    Some(rest.strip_prefix(' ').unwrap_or(rest))
}

fn fence_marker(line: &str) -> Option<(char, usize)> {
    let t = line.trim_start();
    let ch = t.chars().next()?;
    if ch != '`' && ch != '~' {
        return None;
    }
    let n = t.chars().take_while(|&c| c == ch).count();
    if n < 3 { None } else { Some((ch, n)) }
}

fn closes_fence(line: &str, ch: char, opened: usize) -> bool {
    match fence_marker(line) {
        Some((c, n)) => c == ch && n >= opened && line.trim_start()[n..].trim().is_empty(),
        None => false,
    }
}

/// Rewrite `![[image.png]]` / `![[image.png|300]]` to `![](image.png)`; leave every other
/// `![[…]]` embed for tier-3 transclusion.
fn rewrite_embeds(line: &str, count: &mut usize) -> String {
    if !line.contains("![[") {
        return line.to_owned();
    }
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(at) = rest.find("![[") {
        out.push_str(&rest[..at]);
        let inner_start = &rest[at + 3..];
        let Some(end) = inner_start.find("]]") else {
            out.push_str(&rest[at..]);
            return out;
        };
        let inner = &inner_start[..end];
        let target = inner.split('|').next().unwrap_or(inner).trim();
        if is_image(target) {
            out.push_str(&image_link(target));
            *count += 1;
        } else {
            out.push_str(&rest[at..at + 3 + end + 2]);
        }
        rest = &inner_start[end + 2..];
    }
    out.push_str(rest);
    out
}

fn is_image(target: &str) -> bool {
    Path::new(target)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .is_some_and(|e| IMAGE_EXTENSIONS.contains(&e.as_str()))
}

fn image_link(target: &str) -> String {
    if target.contains(char::is_whitespace) || target.contains('(') || target.contains(')') {
        format!("![](<{target}>)")
    } else {
        format!("![]({target})")
    }
}

// ---------------------------------------------------------------------------------------------
// Vault settings
// ---------------------------------------------------------------------------------------------

fn import_bookmarks(src: &Path, dest: &Path, opts: &ImportOptions, report: &mut ImportReport) -> Result<()> {
    let path = src.join(".obsidian").join("bookmarks.json");
    if !path.is_file() {
        return Ok(());
    }
    let raw = fs::read_to_string(&path)?;
    let bookmarks = parse_bookmarks(&raw)?;
    if bookmarks.is_empty() {
        return Ok(());
    }
    let target = dest.join(SIDECAR_DIR).join(BOOKMARKS_FILE);
    if !claim(&target, opts, report)? {
        return Ok(());
    }
    let json = serde_json::to_string_pretty(&bookmarks).map_err(|e| Error::Import(e.to_string()))?;
    fs::write(&target, format!("{json}\n"))?;
    report.bookmarks = bookmarks.len();
    Ok(())
}

/// Flatten Obsidian's `bookmarks.json` (nested `group` items) into note bookmarks.
pub fn parse_bookmarks(raw: &str) -> Result<Vec<Bookmark>> {
    let root: serde_json::Value = serde_json::from_str(raw).map_err(|e| Error::Import(e.to_string()))?;
    let mut out = Vec::new();
    collect_bookmarks(root.get("items"), &mut out);
    Ok(out)
}

fn collect_bookmarks(items: Option<&serde_json::Value>, out: &mut Vec<Bookmark>) {
    let Some(items) = items.and_then(|v| v.as_array()) else {
        return;
    };
    for item in items {
        match item.get("type").and_then(|t| t.as_str()) {
            Some("group") => collect_bookmarks(item.get("items"), out),
            Some("file") => {
                let Some(path) = item.get("path").and_then(|p| p.as_str()) else {
                    continue;
                };
                // A `subpath` (heading/block link) is dropped: only whole notes are bookmarked.
                let label = item
                    .get("title")
                    .and_then(|t| t.as_str())
                    .filter(|t| !t.is_empty())
                    .map(str::to_owned)
                    .unwrap_or_else(|| file_stem(path));
                out.push(Bookmark { kind: "note".to_owned(), target: path.to_owned(), label });
            }
            _ => {}
        }
    }
}

fn file_stem(path: &str) -> String {
    Path::new(path).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| path.to_owned())
}

fn import_daily_notes(
    src: &Path,
    dest: &Path,
    opts: &ImportOptions,
    report: &mut ImportReport,
) -> Result<()> {
    let path = src.join(".obsidian").join("daily-notes.json");
    if !path.is_file() {
        return Ok(());
    }
    let raw = fs::read_to_string(&path)?;
    let settings = parse_daily_notes(&raw)
        .ok_or_else(|| Error::Import(format!("{} is not valid JSON", path.display())))?;
    let target = dest.join(SIDECAR_DIR).join(DAILY_FILE);
    if !claim(&target, opts, report)? {
        return Ok(());
    }
    let json = serde_json::to_string_pretty(&settings).map_err(|e| Error::Import(e.to_string()))?;
    fs::write(&target, format!("{json}\n"))?;
    report.daily_notes = true;
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Uploaded vaults (the web UI)
// ---------------------------------------------------------------------------------------------

/// What one uploaded file of an Obsidian vault turns into. The caller decides where it lands:
/// the server writes notes into the CRDT, the local relay into the vault folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Upload {
    /// A converted note, ready to be created at `path`.
    Note { path: String, text: String, callouts: usize, embeds: usize },
    /// Any other file the vault referenced.
    Attachment { path: String, bytes: Vec<u8> },
    /// `.obsidian/bookmarks.json`, flattened.
    Bookmarks(Vec<Bookmark>),
    /// `.obsidian/daily-notes.json`.
    Daily(DailySettings),
}

/// What one upload request did (SPEC §11.4). A whole import is several requests — the browser
/// sends the picked folder in batches — so these counts are per request and the UI adds them up.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UploadReport {
    pub notes: usize,
    pub attachments: usize,
    pub callouts: usize,
    pub embeds: usize,
    /// Files left alone: a path the vault already had, or an attachment over the size limit.
    pub skipped: usize,
    pub bookmarks: usize,
    /// Whether daily-note settings were stored. Only a vault folder has somewhere to put them
    /// (the sidecar), so this is false for an import into a server.
    pub daily_notes: bool,
}

/// Normalise a browser-supplied relative path: backslashes become `/`, leading slashes and `.`
/// segments go, and anything that could escape the vault (`..`, an absolute Windows path, an
/// empty result) is rejected.
pub fn upload_path(rel: &str) -> Option<String> {
    let rel = rel.replace('\\', "/");
    if rel.contains(':') {
        return None;
    }
    let mut out: Vec<&str> = Vec::new();
    for seg in rel.split('/') {
        match seg.trim() {
            "" | "." => continue,
            ".." => return None,
            s => out.push(s),
        }
    }
    if out.is_empty() { None } else { Some(out.join("/")) }
}

/// Classify and convert one uploaded file (SPEC §11.4). `None` means "not imported": Obsidian's
/// own workspace state, its trash, our sidecar, and hidden files generally — except the two
/// settings files that have a home here.
pub fn import_upload(rel: &str, bytes: Vec<u8>) -> Option<Upload> {
    let path = upload_path(rel)?;
    let hidden = path.split('/').any(|seg| seg.starts_with('.'));
    if hidden {
        return match path.as_str() {
            ".obsidian/bookmarks.json" => {
                let raw = String::from_utf8(bytes).ok()?;
                let marks = parse_bookmarks(&raw).ok()?;
                (!marks.is_empty()).then_some(Upload::Bookmarks(marks))
            }
            ".obsidian/daily-notes.json" => {
                let raw = String::from_utf8(bytes).ok()?;
                Some(Upload::Daily(parse_daily_notes(&raw)?))
            }
            _ => None,
        };
    }
    if is_note(Path::new(&path)) {
        let converted = convert_note(&String::from_utf8_lossy(&bytes));
        return Some(Upload::Note {
            path,
            text: converted.text,
            callouts: converted.callouts,
            embeds: converted.embeds,
        });
    }
    Some(Upload::Attachment { path, bytes })
}

/// Obsidian's `daily-notes.json`, with its defaults filled in.
pub fn parse_daily_notes(raw: &str) -> Option<DailySettings> {
    let value: serde_json::Value = serde_json::from_str(raw).ok()?;
    Some(DailySettings {
        folder: value.get("folder").and_then(|v| v.as_str()).unwrap_or("").to_owned(),
        format: value.get("format").and_then(|v| v.as_str()).unwrap_or("YYYY-MM-DD").to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upload_paths_are_normalised_and_escapes_rejected() {
        assert_eq!(upload_path("Daily\\2026-01-01.md").as_deref(), Some("Daily/2026-01-01.md"));
        assert_eq!(upload_path("/Projects/./plan.md").as_deref(), Some("Projects/plan.md"));
        assert_eq!(upload_path("../escape.md"), None);
        assert_eq!(upload_path("C:/vault/note.md"), None);
        assert_eq!(upload_path("   "), None);
    }

    #[test]
    fn uploaded_notes_are_converted_and_other_files_pass_through() {
        let note = import_upload("Notes/a.md", b"> [!tip] T\n> body\n".to_vec()).unwrap();
        assert_eq!(
            note,
            Upload::Note {
                path: "Notes/a.md".to_owned(),
                text: "::: {.callout-tip title=\"T\"}\nbody\n:::\n".to_owned(),
                callouts: 1,
                embeds: 0,
            }
        );
        let img = import_upload("attachments/logo.png", vec![1, 2, 3]).unwrap();
        assert_eq!(img, Upload::Attachment { path: "attachments/logo.png".to_owned(), bytes: vec![1, 2, 3] });
    }

    #[test]
    fn uploaded_obsidian_state_is_ignored_apart_from_the_two_settings_files() {
        assert_eq!(import_upload(".obsidian/workspace.json", b"{}".to_vec()), None);
        assert_eq!(import_upload(".trash/old.md", b"x".to_vec()), None);
        assert_eq!(import_upload(".lemmate/local.db", b"x".to_vec()), None);
        let marks = import_upload(
            ".obsidian/bookmarks.json",
            br#"{"items":[{"type":"file","path":"Projects/plan.md","title":"Plan"}]}"#.to_vec(),
        );
        assert_eq!(
            marks,
            Some(Upload::Bookmarks(vec![Bookmark {
                kind: "note".to_owned(),
                target: "Projects/plan.md".to_owned(),
                label: "Plan".to_owned(),
            }]))
        );
        let daily = import_upload(".obsidian/daily-notes.json", br#"{"folder":"Daily"}"#.to_vec());
        assert_eq!(
            daily,
            Some(Upload::Daily(DailySettings {
                folder: "Daily".to_owned(),
                format: "YYYY-MM-DD".to_owned()
            }))
        );
    }

    fn convert(src: &str) -> String {
        convert_note(src).text
    }

    #[test]
    fn callout_with_title() {
        let out = convert("> [!warning] Careful\n> body\n");
        assert_eq!(out, "::: {.callout-warning title=\"Careful\"}\nbody\n:::\n");
    }

    #[test]
    fn callout_without_title() {
        let c = convert_note("> [!NOTE]\n> body\n");
        assert_eq!(c.text, "::: {.callout-note}\nbody\n:::\n");
        assert_eq!(c.callouts, 1);
    }

    #[test]
    fn collapsed_callout() {
        let out = convert("> [!note]- Hidden\n> body\n");
        assert_eq!(out, "::: {.callout-note title=\"Hidden\" collapse=\"true\"}\nbody\n:::\n");
    }

    #[test]
    fn multi_line_callout_between_prose() {
        let out = convert("intro\n\n> [!tip] T\n> one\n>\n> two\n\nafter\n");
        assert_eq!(out, "intro\n\n::: {.callout-tip title=\"T\"}\none\n\ntwo\n:::\n\nafter\n");
    }

    #[test]
    fn unknown_kinds_fall_back_to_note_and_aliases_map() {
        assert_eq!(convert("> [!hint] H\n"), "::: {.callout-tip title=\"H\"}\n:::\n");
        assert_eq!(convert("> [!bug] B\n"), "::: {.callout-note title=\"B\"}\n:::\n");
        assert_eq!(convert("> [!caution] C\n"), "::: {.callout-caution title=\"C\"}\n:::\n");
        assert_eq!(convert("> [!important] I\n"), "::: {.callout-important title=\"I\"}\n:::\n");
    }

    #[test]
    fn plain_blockquote_is_untouched() {
        let c = convert_note("> just a quote\n> more\n");
        assert_eq!(c.text, "> just a quote\n> more\n");
        assert_eq!(c.callouts, 0);
    }

    #[test]
    fn callout_inside_code_block_is_untouched() {
        let src = "```md\n> [!note] Example\n> body\n```\n\n> [!note] Real\n> body\n";
        let c = convert_note(src);
        assert_eq!(
            c.text,
            "```md\n> [!note] Example\n> body\n```\n\n::: {.callout-note title=\"Real\"}\nbody\n:::\n"
        );
        assert_eq!(c.callouts, 1);
    }

    #[test]
    fn tilde_fences_and_embeds_inside_code_are_untouched() {
        let c = convert_note("~~~\n![[img.png]]\n~~~\n![[img.png]]\n");
        assert_eq!(c.text, "~~~\n![[img.png]]\n~~~\n![](img.png)\n");
        assert_eq!(c.embeds, 1);
    }

    #[test]
    fn image_embeds_are_rewritten() {
        let c = convert_note("![[img.png]] ![[img.png|300]] ![[folder/img.JPG]] ![[my pic.webp]]\n");
        assert_eq!(c.text, "![](img.png) ![](img.png) ![](folder/img.JPG) ![](<my pic.webp>)\n");
        assert_eq!(c.embeds, 4);
    }

    #[test]
    fn non_image_embeds_and_wikilinks_are_left_alone() {
        let src = "![[Some Note]] ![[paper.pdf]] [[wiki|label]] #tag $x_1$\n";
        let c = convert_note(src);
        assert_eq!(c.text, src);
        assert_eq!(c.embeds, 0);
    }

    #[test]
    fn front_matter_is_untouched_and_no_id_is_added() {
        let src = "---\ntitle: T\ntags: [a]\n---\n\n![[img.png]]\n";
        let c = convert_note(src);
        assert_eq!(c.text, "---\ntitle: T\ntags: [a]\n---\n\n![](img.png)\n");
        assert!(!c.text.contains("id:"));
    }

    #[test]
    fn embeds_inside_callouts_are_rewritten() {
        let c = convert_note("> [!note] N\n> ![[img.png]]\n");
        assert_eq!(c.text, "::: {.callout-note title=\"N\"}\n![](img.png)\n:::\n");
        assert_eq!((c.callouts, c.embeds), (1, 1));
    }

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    #[test]
    fn folder_structure_attachments_and_skips() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("vault");
        let dest = tmp.path().join("out");
        write(&src.join("Root.md"), "> [!tip] Hi\n> ![[a/img.png]]\n");
        write(&src.join("a/Nested.qmd"), "body\n");
        write(&src.join("a/img.png"), "PNG");
        write(&src.join(".obsidian/app.json"), "{}");
        write(&src.join(".trash/Gone.md"), "gone\n");
        write(&src.join(".hidden"), "x");
        write(&dest.join("Root.md"), "mine\n");

        let report = import_obsidian(&src, &dest, &ImportOptions::default()).unwrap();
        assert_eq!(report, ImportReport { notes: 1, attachments: 1, skipped: 1, ..Default::default() });
        // The pre-existing file was not clobbered.
        assert_eq!(fs::read_to_string(dest.join("Root.md")).unwrap(), "mine\n");
        assert_eq!(fs::read_to_string(dest.join("a/Nested.qmd")).unwrap(), "body\n");
        assert_eq!(fs::read_to_string(dest.join("a/img.png")).unwrap(), "PNG");
        assert!(!dest.join(".obsidian").exists());
        assert!(!dest.join(".trash").exists());
        assert!(!dest.join(".hidden").exists());

        // With --overwrite the note is imported (and converted) after all.
        let report = import_obsidian(&src, &dest, &ImportOptions { overwrite: true }).unwrap();
        assert_eq!(
            report,
            ImportReport { notes: 2, attachments: 1, callouts: 1, embeds: 1, ..Default::default() }
        );
        assert_eq!(
            fs::read_to_string(dest.join("Root.md")).unwrap(),
            "::: {.callout-tip title=\"Hi\"}\n![](a/img.png)\n:::\n"
        );
    }

    #[test]
    fn bookmarks_with_nested_groups() {
        let raw = r#"{
          "items": [
            { "type": "file", "path": "Inbox.md", "title": "Inbox" },
            { "type": "search", "query": "tag:#x" },
            { "type": "group", "title": "Work", "items": [
                { "type": "file", "path": "Work/Plan.md" },
                { "type": "group", "title": "Deep", "items": [
                    { "type": "file", "path": "Work/Deep/Spec.md", "title": "The Spec" }
                ]}
            ]}
          ]
        }"#;
        let got = parse_bookmarks(raw).unwrap();
        assert_eq!(
            got,
            vec![
                Bookmark { kind: "note".into(), target: "Inbox.md".into(), label: "Inbox".into() },
                Bookmark { kind: "note".into(), target: "Work/Plan.md".into(), label: "Plan".into() },
                Bookmark {
                    kind: "note".into(),
                    target: "Work/Deep/Spec.md".into(),
                    label: "The Spec".into()
                },
            ]
        );
    }

    #[test]
    fn settings_land_in_the_sidecar() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("vault");
        let dest = tmp.path().join("out");
        write(&src.join("N.md"), "n\n");
        write(&src.join(".obsidian/bookmarks.json"), r#"{"items":[{"type":"file","path":"N.md"}]}"#);
        write(&src.join(".obsidian/daily-notes.json"), r#"{"folder":"Journal","format":"YYYY-MM-DD"}"#);

        let report = import_obsidian(&src, &dest, &ImportOptions::default()).unwrap();
        assert_eq!(report.bookmarks, 1);
        assert!(report.daily_notes);

        let bookmarks: Vec<Bookmark> =
            serde_json::from_str(&fs::read_to_string(dest.join(SIDECAR_DIR).join(BOOKMARKS_FILE)).unwrap())
                .unwrap();
        assert_eq!(
            bookmarks,
            vec![Bookmark { kind: "note".into(), target: "N.md".into(), label: "N".into() }]
        );
        let daily: DailySettings =
            serde_json::from_str(&fs::read_to_string(dest.join(SIDECAR_DIR).join(DAILY_FILE)).unwrap())
                .unwrap();
        assert_eq!(daily, DailySettings { folder: "Journal".into(), format: "YYYY-MM-DD".into() });
        assert!(report.to_string().contains("bookmark"));
    }
}

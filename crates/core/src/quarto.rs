//! Rendering a note through Quarto (SPEC §5.6, §12): "Render with Quarto" for `.qmd` notes —
//! and any other note, which Quarto reads just as well.
//!
//! A render never runs code: `--no-execute` is always passed, so a `{python}` cell is shown,
//! not run (SPEC §14). The note's own front matter *is* honoured — that is the point of Quarto —
//! except what would run code or read files on the host at an editor's say-so: Lua filters and
//! shortcodes, include files, templates, paths out of the project, and the shortcodes that read
//! files or the environment (see [`DENIED_KEYS`] and [`GUARD`]). The render sees only a cleared
//! environment. A server can still switch rendering off altogether.
//!
//! Quarto wants a project on disk, so a render builds one in a temporary directory: the note
//! at its vault path (as `.qmd`, since Quarto refuses code cells in a `.md`), the attachments
//! it references at theirs — so a relative image link resolves exactly as it does in the vault
//! — and a `_quarto.yml` that makes HTML self-contained and supplies the vault's bibliography.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::error::{Error, Result};

/// What a render can produce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// One self-contained page — what the app shows in its preview pane.
    Html,
    /// PDF through Typst, which Quarto bundles; LaTeX is not needed.
    Pdf,
    Docx,
    /// reveal.js slides, self-contained like `Html`.
    RevealJs,
}

impl Format {
    pub fn parse(s: &str) -> Option<Format> {
        Some(match s.to_ascii_lowercase().as_str() {
            "html" => Format::Html,
            "pdf" | "typst" => Format::Pdf,
            "docx" => Format::Docx,
            "revealjs" | "slides" => Format::RevealJs,
            _ => return None,
        })
    }
    /// The `--to` Quarto is given.
    fn quarto_name(self) -> &'static str {
        match self {
            Format::Html => "html",
            Format::Pdf => "typst",
            Format::Docx => "docx",
            Format::RevealJs => "revealjs",
        }
    }
    pub fn extension(self) -> &'static str {
        match self {
            Format::Html | Format::RevealJs => "html",
            Format::Pdf => "pdf",
            Format::Docx => "docx",
        }
    }
    pub fn mime(self) -> &'static str {
        match self {
            Format::Html | Format::RevealJs => "text/html; charset=utf-8",
            Format::Pdf => "application/pdf",
            Format::Docx => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        }
    }
}

/// The formats a note's front matter declares, in its order, base names only
/// (`revealjs+code` → `revealjs`): `format: x`, `format: [x, y]`, or a `format:` block's keys.
fn declared_formats(text: &str) -> Vec<String> {
    use serde_yaml_ng::Value;
    let Some((yaml, _)) = crate::frontmatter::block(text) else { return Vec::new() };
    let Ok(Value::Mapping(front)) = serde_yaml_ng::from_str::<Value>(&text[yaml]) else { return Vec::new() };
    let names: Vec<String> = match front.get("format") {
        Some(Value::String(f)) => vec![f.clone()],
        Some(Value::Sequence(fs)) => fs.iter().filter_map(|f| f.as_str().map(str::to_owned)).collect(),
        Some(Value::Mapping(m)) => m.keys().filter_map(|k| k.as_str().map(str::to_owned)).collect(),
        _ => Vec::new(),
    };
    names.into_iter().map(|f| f.split('+').next().unwrap_or(&f).to_owned()).collect()
}

/// What "Render with Quarto" makes of a note when nobody says otherwise (`"auto"`): the first
/// format its front matter declares that a render can produce — a deck as slides, `pdf` or
/// `typst` as a PDF, `docx` as a Word file — else a plain page.
pub fn declared_format(text: &str) -> Format {
    declared_formats(text)
        .iter()
        .find_map(|f| match f.as_str() {
            "revealjs" => Some(Format::RevealJs),
            "html" => Some(Format::Html),
            "pdf" | "typst" => Some(Format::Pdf),
            "docx" => Some(Format::Docx),
            _ => None,
        })
        .unwrap_or(Format::Html)
}

/// The first *page* the note declares (`"preview"`): slides or HTML, never a file to save.
pub fn preview_format(text: &str) -> Format {
    declared_formats(text)
        .iter()
        .find_map(|f| match f.as_str() {
            "revealjs" => Some(Format::RevealJs),
            "html" => Some(Format::Html),
            _ => None,
        })
        .unwrap_or(Format::Html)
}

/// The `Content-Security-Policy` a render opened as a page of its own is served with. Quarto's
/// page runs its own scripts — and a note's author wrote what is in it — so, as in the app's
/// sandboxed frame, it gets an origin of its own: its scripts run, and cannot reach the app,
/// the session or the API of the site it came from.
pub const PAGE_SANDBOX: &str = "sandbox allow-scripts allow-popups allow-popups-to-escape-sandbox";

/// Renders made for viewing, kept a while under an id of their own, so that the same page can be
/// opened again — in a tab of its own, say — without Quarto making it a second time. A handful,
/// for half an hour: this is a courtesy, not a store; a miss means rendering again.
pub struct RenderCache {
    kept: std::sync::Mutex<std::collections::VecDeque<Kept>>,
}

struct Kept {
    id: String,
    /// The note it was made from: an id is only good together with its note.
    note: String,
    bytes: std::sync::Arc<Vec<u8>>,
    mime: &'static str,
    disposition: String,
    made: Instant,
}

/// A render kept by [`RenderCache`]: bytes, MIME type, `Content-Disposition`.
pub type KeptRender = (Vec<u8>, &'static str, String);

impl RenderCache {
    const KEEP: usize = 16;
    const FOR: Duration = Duration::from_secs(30 * 60);

    pub fn new() -> Self {
        Self { kept: std::sync::Mutex::new(std::collections::VecDeque::new()) }
    }

    /// A fresh id for [`RenderCache::put_as`], for a render that has to name itself before it
    /// is kept (a deck's speaker view is sent to it).
    pub fn new_id() -> String {
        ulid::Ulid::generate().to_string()
    }

    /// Keep a render of `note`; the id to fetch it again by.
    pub fn put(&self, note: &str, bytes: &[u8], mime: &'static str, disposition: &str) -> String {
        let id = Self::new_id();
        self.put_as(&id, note, bytes, mime, disposition);
        id
    }

    /// Keep a render of `note` under `id` ([`RenderCache::new_id`]).
    pub fn put_as(&self, id: &str, note: &str, bytes: &[u8], mime: &'static str, disposition: &str) {
        let id = id.to_owned();
        let mut kept = self.kept.lock().unwrap_or_else(|e| e.into_inner());
        kept.retain(|k| k.made.elapsed() < Self::FOR);
        while kept.len() >= Self::KEEP {
            kept.pop_front();
        }
        kept.push_back(Kept {
            id: id.clone(),
            note: note.to_owned(),
            bytes: std::sync::Arc::new(bytes.to_vec()),
            mime,
            disposition: disposition.to_owned(),
            made: Instant::now(),
        });
    }

    /// The render kept as `id`, if it is still kept and was made from `note`.
    pub fn get(&self, id: &str, note: &str) -> Option<KeptRender> {
        let kept = self.kept.lock().unwrap_or_else(|e| e.into_inner());
        kept.iter()
            .find(|k| k.id == id && k.note == note && k.made.elapsed() < Self::FOR)
            .map(|k| (k.bytes.to_vec(), k.mime, k.disposition.clone()))
    }
}

impl Default for RenderCache {
    fn default() -> Self {
        Self::new()
    }
}

/// The `Content-Disposition` a render is served with: inline, so HTML can be shown in place,
/// and named after the note for whoever saves it.
pub fn disposition(note_path: &str, format: Format) -> String {
    let stem = Path::new(note_path)
        .file_stem()
        .map(|s| s.to_string_lossy().replace(['"', '\\', '\r', '\n'], ""))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "note".into());
    format!("inline; filename=\"{stem}.{}\"", format.extension())
}

#[derive(Debug, Clone)]
pub struct RenderOptions {
    /// `quarto` binary; `None` → `$LEMMATE_QUARTO`, then `quarto` on `PATH`.
    pub quarto: Option<PathBuf>,
    /// A render that takes longer is killed. Quarto starts in about a second and a note renders
    /// in a few; this is for the one that never finishes.
    pub timeout: Duration,
    /// Made to be looked at in the app — the render pane, or a tab of its own — rather than
    /// saved. Such a page is sandboxed, and a sandboxed document may not rewrite its own URL
    /// the way reveal.js does on every slide (`hash`, `history`): WebKit refuses, and on an
    /// iPhone the slide then turned without the screen showing it. So a deck made for viewing
    /// keeps its URL alone; one saved to a file keeps its slide links.
    pub viewing: bool,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self { quarto: None, timeout: Duration::from_secs(120), viewing: false }
    }
}

/// The binary a render runs: the one given, else `$LEMMATE_QUARTO`, else `quarto` on `PATH`.
pub fn quarto_bin(explicit: Option<&Path>) -> PathBuf {
    explicit
        .map(Path::to_path_buf)
        .or_else(|| std::env::var_os("LEMMATE_QUARTO").filter(|v| !v.is_empty()).map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("quarto"))
}

pub fn quarto_available(quarto: Option<&Path>) -> bool {
    Command::new(quarto_bin(quarto))
        .arg("--version")
        .stdin(Stdio::null())
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Where the vault's bibliography lives (SPEC §12): used when the note does not name its own.
const BIBLIOGRAPHY: &str = "export/references.bib";
const CSL: &str = "export/style.csl";

/// Render the note at vault path `note_path`. `attachments` are the vault's attachment paths
/// and `read` fetches any vault file by path — only the ones the note references, and the
/// vault's bibliography, are asked for. Returns the bytes and their MIME type.
pub fn render(
    note_path: &str,
    text: &str,
    format: Format,
    attachments: &[String],
    read: impl Fn(&str) -> Option<Vec<u8>>,
    opts: &RenderOptions,
) -> Result<(Vec<u8>, &'static str)> {
    let work = crate::pandoc::tempdir()?;
    render_in(&work, note_path, text, format, attachments, read, opts)
}

fn render_in(
    work: &Path,
    note_path: &str,
    text: &str,
    format: Format,
    attachments: &[String],
    read: impl Fn(&str) -> Option<Vec<u8>>,
    opts: &RenderOptions,
) -> Result<(Vec<u8>, &'static str)> {
    let project = work.join("project");
    let rel = source_path(note_path);
    // The note's place in the project, which is what every path in it is relative to.
    let base = rel.iter().map(|s| s.to_string_lossy()).collect::<Vec<_>>().join("/");
    // What the note depends on — images, stylesheets and what they import, filters, anything its
    // front matter names — and the vault's own resources: `_quarto.yml`, `_metadata.yml`, the
    // export folder. All of it at its vault path, so every relative path means what it does in
    // the vault.
    let exists = |p: &str| attachments.iter().any(|a| a == p);
    let mut files = crate::attachments::referenced(note_path, text, attachments, &read)?;
    for p in crate::attachments::vault_resources(exists, || attachments.to_vec(), &read) {
        if !files.contains(&p) {
            files.push(p);
        }
    }
    let mut vault_project = None;
    for path in &files {
        // An extension is Lua to run: a render never has one.
        if path.split('/').any(|seg| seg == "_extensions") {
            continue;
        }
        let (Some(safe), Some(bytes)) = (safe_relative(path), read(path)) else { continue };
        if matches!(path.as_str(), "_quarto.yml" | "_quarto.yaml") {
            // Merged into the project file below rather than written as it is.
            vault_project = Some(String::from_utf8_lossy(&bytes).into_owned());
            continue;
        }
        let bytes = if is_yaml(path) {
            let Some(guarded) = guard_yaml(path, &String::from_utf8_lossy(&bytes)) else { continue };
            serde_yaml_ng::to_string(&guarded).map_err(|e| Error::Export(e.to_string()))?.into_bytes()
        } else if is_stylesheet(path) {
            guard_stylesheet(path, &String::from_utf8_lossy(&bytes)).into_bytes()
        } else {
            bytes
        };
        write(&project.join(safe), &bytes)?;
    }
    let extra: Vec<(&str, PathBuf)> = [(BIBLIOGRAPHY, "bibliography"), (CSL, "csl")]
        .into_iter()
        .filter(|(path, _)| files.iter().any(|f| f == path))
        .map(|(path, key)| (key, project.join(path)))
        .collect();
    let guard = work.join("guard.lua");
    let depth = rel.components().count().saturating_sub(1);
    write(&guard, GUARD.replace("__DEPTH__", &depth.to_string()).as_bytes())?;
    write(&project.join("_quarto.yml"), project_yaml(vault_project.as_deref(), &extra, &guard)?.as_bytes())?;
    let text = guard_metadata(&base, text)?;
    write(&project.join(&rel), defuse_shortcodes(&prepare(&text, note_path, attachments)).as_bytes())?;

    let bin = quarto_bin(opts.quarto.as_deref());
    let log = work.join("quarto.log");
    let mut cmd = Command::new(&bin);
    cmd.env_clear().envs(std::env::vars_os().filter(|(k, _)| k.to_str().is_some_and(kept_env)));
    cmd.current_dir(&project)
        .arg("render")
        .arg(&rel)
        // Not `--quiet`: that silences the error along with the progress, and the error is
        // the one part of the log a failed render is read for.
        .args(["--to", format.quarto_name(), "--no-execute"])
        // Command-line metadata wins over the note's own front matter.
        .args(if opts.viewing && format == Format::RevealJs {
            &["-M", "hash:false", "-M", "history:false"][..]
        } else {
            &[][..]
        })
        .stdout(Stdio::null())
        .stderr(std::fs::File::create(&log)?);
    // Killed with what it started — pandoc, Typst, Deno — if it runs too long.
    let status = crate::pandoc::run_bounded(&mut cmd, None, opts.timeout)
        .map_err(|e| Error::Export(format!("running {}: {e}", bin.display())))?
        .ok_or_else(|| Error::Export(format!("quarto did not finish within {}s", opts.timeout.as_secs())))?
        .status;
    // Named by Quarto after the source, beside it: asking for another name with `--output`
    // quietly stops HTML from embedding its resources.
    let out = project.join(rel.with_extension(format.extension()));
    if !status.success() || !out.is_file() {
        let stderr = std::fs::read_to_string(&log).unwrap_or_default();
        return Err(Error::Export(tail(&stderr)));
    }
    let bytes = std::fs::read(out)?;
    let bytes = match (opts.viewing, format) {
        (true, Format::Html | Format::RevealJs) => with_storage(bytes),
        _ => bytes,
    };
    Ok((bytes, format.mime()))
}

/// Web storage that lives in memory, for a page that has none. A render made for viewing runs
/// sandboxed, with an origin of its own, and such a document may not touch `localStorage` or
/// `sessionStorage`: reading either throws. reveal.js reads `sessionStorage` when it switches a
/// deck to its scroll view — which it does on a phone-narrow screen — and the throw left the
/// deck half-switched: Chrome carried on, but on an iPhone only the first swipe turned a slide,
/// and the screen showed it only after the phone was rotated. With this in the page before
/// any of its own scripts, storage works for as long as the page is open and is gone after.
const STORAGE_STAND_IN: &str = r#"<script>(function(){function m(){var d={};return{getItem:function(k){return Object.prototype.hasOwnProperty.call(d,k)?d[k]:null},setItem:function(k,v){d[k]=String(v)},removeItem:function(k){delete d[k]},clear:function(){d={}},key:function(i){return Object.keys(d)[i]||null},get length(){return Object.keys(d).length}}}["localStorage","sessionStorage"].forEach(function(k){try{window[k].getItem("x")}catch(e){try{Object.defineProperty(window,k,{value:m(),configurable:true})}catch(e2){}}})})();</script>"#;

/// A deck's speaker view, the Lemmate way. reveal.js opens its own by writing into a window it
/// opens — which a sandboxed deck cannot do: each sandboxed document has an origin of its own,
/// and one may not touch another. So `S`, and the menu's *Speaker View*, open Lemmate's speaker
/// page instead (`/speaker.html`, the web client's), and the two only ever talk by
/// `postMessage`: the deck says where it is and what the slide's notes are; the speaker page
/// turns the deck with reveal.js's own postMessage commands. `__IDS__` becomes the query that
/// names the render the speaker page shows its previews from.
const SPEAKER_HOOK: &str = r#"<script>(function(){var q="__IDS__",w=null;function st(){var R=window.Reveal;if(!R)return null;var s=R.getCurrentSlide(),n=s&&s.querySelector("aside.notes");return{lemmateDeck:1,state:R.getState(),notes:n?n.innerHTML:(s&&s.getAttribute("data-notes"))||"",index:R.getSlidePastCount()+1,total:R.getTotalSlides()}}function send(){if(w&&!w.closed)try{w.postMessage(JSON.stringify(st()),"*")}catch(e){}}function open(){if(w&&!w.closed){w.focus();send();return}w=window.open("/speaker.html?"+q,"lemmate-speaker","width=1180,height=720")}window.addEventListener("message",function(e){if(!w||e.source!==w)return;var d;try{d=JSON.parse(e.data)}catch(x){return}if(d&&d.lemmateSpeaker==="hello")send()});function hook(){var R=window.Reveal;if(!R||!R.isReady||!R.isReady())return void setTimeout(hook,200);["slidechanged","fragmentshown","fragmenthidden","overviewshown","overviewhidden","paused","resumed"].forEach(function(v){R.on(v,send)});try{R.removeKeyBinding(83)}catch(e){}R.addKeyBinding({keyCode:83,key:"S",description:"Speaker view"},open);var p=R.getPlugin&&R.getPlugin("notes");if(p)p.open=open}if(document.readyState==="loading")document.addEventListener("DOMContentLoaded",hook);else hook()})();</script>"#;

/// `page` — a deck made for viewing — with [`SPEAKER_HOOK`], naming the render kept as `render`
/// of note `note` in `vault` (ids: letters and digits only, or nothing is added).
pub fn with_speaker(page: Vec<u8>, vault: &str, note: &str, render: &str) -> Vec<u8> {
    let plain = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric());
    if !(plain(vault) && plain(note) && plain(render)) {
        return page;
    }
    let hook = SPEAKER_HOOK.replace("__IDS__", &format!("vault={vault}&note={note}&render={render}"));
    insert_in_head(page, &hook)
}

/// `page` with [`STORAGE_STAND_IN`] as the first thing in its `<head>` (or at its very start,
/// if it has none) — ahead of every script of its own.
fn with_storage(page: Vec<u8>) -> Vec<u8> {
    insert_in_head(page, STORAGE_STAND_IN)
}

/// `page` with `html` just inside its `<head>` (or at its very start, if it has none).
fn insert_in_head(page: Vec<u8>, html: &str) -> Vec<u8> {
    let at = page
        .windows(5)
        .position(|w| w.eq_ignore_ascii_case(b"<head"))
        .and_then(|i| page[i..].iter().position(|&b| b == b'>').map(|j| i + j + 1))
        .unwrap_or(0);
    let mut out = Vec::with_capacity(page.len() + html.len());
    out.extend_from_slice(&page[..at]);
    out.extend_from_slice(html.as_bytes());
    out.extend_from_slice(&page[at..]);
    out
}

fn write(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    Ok(std::fs::write(path, bytes)?)
}

/// Where the note goes in the project: its own vault path, with `.qmd` for an extension.
fn source_path(note_path: &str) -> PathBuf {
    safe_relative(note_path).unwrap_or_else(|| PathBuf::from("note")).with_extension("qmd")
}

/// A vault path as a relative filesystem path that stays inside the project, or `None`.
fn safe_relative(path: &str) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => return None,
            s if s.contains('\\') || s.contains(':') => return None,
            s => out.push(s),
        }
    }
    (!out.as_os_str().is_empty()).then_some(out)
}

/// The project around the note: the vault's `_quarto.yml` when it has one, so a theme or
/// options shared across documents apply, with three things settled here whatever it says.
///
/// - The project is this one note: `type: default`, and nothing else of the vault's `project:`
///   — no website or book, no output directory, and no `pre-render`/`post-render` scripts,
///   which are programs to run and have no business in a preview.
/// - HTML and reveal.js are self-contained, because the pane shows one page and nothing else.
/// - The vault's `export/references.bib` and `style.csl` are the bibliography and style unless
///   the vault's file names its own — and a note's front matter wins over both, as Quarto
///   always lets a document override its project.
fn project_yaml(vault: Option<&str>, extra: &[(&str, PathBuf)], guard: &Path) -> Result<String> {
    use serde_yaml_ng::{Mapping, Value};
    let mut root = match vault {
        Some(text) if !text.trim().is_empty() => match serde_yaml_ng::from_str::<Value>(text) {
            Ok(mut v) => {
                v.apply_merge().map_err(|e| Error::Export(format!("the vault's _quarto.yml: {e}")))?;
                guard_value("_quarto.yml", &mut v);
                match v {
                    Value::Mapping(m) => m,
                    _ => Mapping::new(),
                }
            }
            Err(e) => return Err(Error::Export(format!("the vault's _quarto.yml is not valid YAML: {e}"))),
        },
        _ => Mapping::new(),
    };
    let key = |k: &str| Value::String(k.to_owned());
    let mut project = Mapping::new();
    project.insert(key("type"), key("default"));
    root.insert(key("project"), Value::Mapping(project));

    let mut format = match root.remove("format") {
        Some(Value::Mapping(m)) => m,
        // `format: html`, or a list of them: the formats are named, their options are not.
        Some(Value::String(f)) => Mapping::from_iter([(key(&f), key("default"))]),
        Some(Value::Sequence(fs)) => {
            fs.into_iter().filter(|f| f.is_string()).map(|f| (f, key("default"))).collect()
        }
        _ => Mapping::new(),
    };
    for name in ["html", "revealjs", "typst", "docx"] {
        let mut options = match format.remove(name) {
            Some(Value::Mapping(m)) => m,
            _ => Mapping::new(),
        };
        if matches!(name, "html" | "revealjs") {
            options.insert(key("embed-resources"), Value::Bool(true));
        }
        if !options.contains_key("from") {
            options.insert(key("from"), key(READER));
        }
        format.insert(key(name), Value::Mapping(options));
    }
    root.insert(key("format"), Value::Mapping(format));
    // Ours, and the only one: [`DENIED_KEYS`] took any the vault named.
    let mut filter = Mapping::new();
    filter.insert(key("path"), key(&guard.to_string_lossy()));
    filter.insert(key("at"), key("post-render"));
    root.insert(key("filters"), Value::Sequence(vec![Value::Mapping(filter)]));

    for (name, path) in extra {
        if !root.contains_key(*name) {
            root.insert(key(name), key(&path.to_string_lossy()));
        }
    }
    serde_yaml_ng::to_string(&Value::Mapping(root)).map_err(|e| Error::Export(e.to_string()))
}

/// What a failed render says: Quarto's `ERROR` and the lines that point at the problem, without
/// its colours or its stack trace — or, when there is no `ERROR`, the last few lines it wrote.
fn tail(stderr: &str) -> String {
    let plain = strip_ansi(stderr);
    let lines: Vec<&str> = plain.lines().map(str::trim_end).collect();
    let picked: Vec<&str> = match lines.iter().position(|l| l.trim_start().starts_with("ERROR")) {
        Some(at) => {
            lines[at..].iter().take_while(|l| !l.starts_with("Stack trace:")).take(12).copied().collect()
        }
        None => {
            let kept: Vec<&str> = lines
                .iter()
                .copied()
                .filter(|l| !l.is_empty() && !l.trim_start().starts_with("at "))
                .collect();
            kept[kept.len().saturating_sub(6)..].to_vec()
        }
    };
    let msg = picked.join("\n").trim().trim_start_matches("ERROR:").trim().to_owned();
    if msg.is_empty() { "quarto failed without saying why".into() } else { format!("quarto: {msg}") }
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// The note as Quarto should read it. Quarto's reader knows nothing of wikilinks, so outside
/// code they become what they mean in a single rendered page: an attachment embed an image,
/// with an Obsidian width (`|300`) kept; any other link or embed its label, marked `.wikilink`
/// — the other note is not part of this document, so there is nowhere for it to go.
pub fn prepare(text: &str, note_path: &str, attachments: &[String]) -> String {
    let mut out = String::with_capacity(text.len());
    let mut fence: Option<(char, usize)> = None;
    for line in text.split_inclusive('\n') {
        match fence {
            Some((ch, n)) => {
                if crate::import::closes_fence(line, ch, n) {
                    fence = None;
                }
                out.push_str(line);
            }
            None => {
                if let Some(open) = crate::import::fence_marker(line) {
                    fence = Some(open);
                    out.push_str(line);
                } else {
                    rewrite_line(line, note_path, attachments, &mut out);
                }
            }
        }
    }
    out
}

/// One line outside fenced code: inline code spans are copied as they are, `[[…]]` rewritten.
fn rewrite_line(line: &str, note_path: &str, attachments: &[String], out: &mut String) {
    let mut rest = line;
    while !rest.is_empty() {
        let tick = rest.find('`');
        let link = rest.find("[[");
        match (tick, link) {
            (Some(t), l) if l.is_none_or(|l| t < l) => {
                out.push_str(&rest[..t]);
                let run = rest[t..].chars().take_while(|&c| c == '`').count();
                let after = &rest[t + run..];
                let closer = "`".repeat(run);
                match after.find(&closer) {
                    Some(end) => {
                        let span = t + run + end + run;
                        out.push_str(&rest[t..span]);
                        rest = &rest[span..];
                    }
                    None => {
                        out.push_str(&rest[t..]);
                        return;
                    }
                }
            }
            (_, Some(l)) => {
                let embed = l > 0 && rest.as_bytes()[l - 1] == b'!';
                let start = if embed { l - 1 } else { l };
                out.push_str(&rest[..start]);
                let inner_from = l + 2;
                let Some(end) = rest[inner_from..].find("]]") else {
                    out.push_str(&rest[start..]);
                    return;
                };
                let inner = &rest[inner_from..inner_from + end];
                out.push_str(&wikilink(inner, embed, note_path, attachments));
                rest = &rest[inner_from + end + 2..];
            }
            _ => {
                out.push_str(rest);
                return;
            }
        }
    }
}

fn wikilink(inner: &str, embed: bool, note_path: &str, attachments: &[String]) -> String {
    // Inside a table the alias pipe is written `\|`.
    let (target, alias) = match inner.find('|') {
        Some(i) => (inner[..i].trim_end_matches('\\'), Some(inner[i + 1..].trim())),
        None => (inner, None),
    };
    let target = target.trim();
    if embed
        && let Some(path) = crate::attachments::resolve_reference(
            note_path,
            target,
            true,
            |c| attachments.iter().any(|a| a == c),
            || attachments.to_vec(),
        )
    {
        let src = relative_from(note_path, &path);
        let width = alias.filter(|a| !a.is_empty() && a.chars().all(|c| c.is_ascii_digit()));
        return match width {
            Some(w) => format!("![](<{src}>){{width={w}px}}"),
            None => format!("![](<{src}>)"),
        };
    }
    let label = match alias.filter(|a| !a.is_empty()) {
        Some(a) => a.to_owned(),
        None => match target.split_once('#') {
            Some((note, section)) if !note.is_empty() => {
                format!("{note} › {}", section.trim_start_matches('^'))
            }
            Some((_, section)) => section.trim_start_matches('^').to_owned(),
            None => target.to_owned(),
        },
    };
    format!("[{}]{{.wikilink}}", escape_inline(&label))
}

/// `path` (vault-relative) as seen from the folder holding `note_path`.
fn relative_from(note_path: &str, path: &str) -> String {
    let depth = note_path.matches('/').count();
    format!("{}{path}", "../".repeat(depth))
}

fn escape_inline(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '[' | ']' | '*' | '_' | '`' | '\\' | '$' | '<' | '>' | '#' | '^' | '~' | '@') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

// What a render may not do. A note is somebody's text, rendered on a server or on a
// collaborator's machine; Quarto would happily run the Lua filters it names and read any file it
// points at. So everything Quarto reads from the vault is checked first — the note, its
// `_metadata.yml` files, the vault's `_quarto.yml`, its stylesheets — and the page Quarto makes
// passes through [`GUARD`] before it is written.

/// Metadata keys that make a render run a program, or read or write files named in them: Lua
/// filters and shortcodes, engines and their settings, include files, templates, other
/// metadata files, PDF engines, LaTeX installs. Dropped wherever they appear — under `format:`
/// too. (`latex-*` goes as well, by prefix.)
const DENIED_KEYS: &[&str] = &[
    "filters",
    "shortcodes",
    "lua-filters",
    "ipynb-filters",
    "engine",
    "engines",
    "jupyter",
    "knitr",
    "julia",
    "execute",
    "execute-dir",
    "server",
    "pdf-engine",
    "pdf-engine-opt",
    "pdf-engine-opts",
    "template",
    "template-partials",
    "metadata-files",
    "metadata-file",
    "include-in-header",
    "include-before-body",
    "include-after-body",
    "resources",
    "output-file",
    "output-dir",
    "extract-media",
    "data-dir",
    "resource-path",
    "defaults",
    "pre-render",
    "post-render",
    "freeze",
    "cache",
];

fn extension_is(path: &str, exts: &[&str]) -> bool {
    path.rsplit_once('.').is_some_and(|(_, e)| exts.iter().any(|x| e.eq_ignore_ascii_case(x)))
}

fn is_yaml(path: &str) -> bool {
    extension_is(path, &["yml", "yaml"])
}

fn is_stylesheet(path: &str) -> bool {
    extension_is(path, &["css", "scss", "sass"])
}

fn denied_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase().replace('_', "-");
    DENIED_KEYS.contains(&key.as_str()) || key.starts_with("latex-")
}

/// pandoc's reader, as every render runs it: metadata comes from the front matter Quarto reads
/// (and this module checks), never from a YAML block pandoc might find further down.
const READER: &str = "markdown-yaml_metadata_block";

/// Environment a render keeps; everything else — a server's secrets among it — Quarto, and the
/// `{{< env >}}` shortcode, never see.
fn kept_env(name: &str) -> bool {
    const KEPT: &[&str] = &[
        "PATH",
        "HOME",
        "USER",
        "LOGNAME",
        "LANG",
        "LANGUAGE",
        "TZ",
        "TMPDIR",
        "TMP",
        "TEMP",
        "SYSTEMROOT",
        "WINDIR",
        "COMSPEC",
        "PATHEXT",
        "APPDATA",
        "LOCALAPPDATA",
        "USERPROFILE",
        "PROGRAMDATA",
        "PROGRAMFILES",
        "HOMEDRIVE",
        "HOMEPATH",
    ];
    let upper = name.to_ascii_uppercase();
    KEPT.contains(&upper.as_str()) || ["LC_", "XDG_", "QUARTO_", "DENO_"].iter().any(|p| upper.starts_with(p))
}

/// `%xx` escapes decoded, as pandoc reads a local path.
fn percent_decoded(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = |c: u8| (c as char).to_digit(16);
        if b[i] == b'%'
            && let (Some(h), Some(l)) =
                (b.get(i + 1).and_then(|&c| hex(c)), b.get(i + 2).and_then(|&c| hex(c)))
        {
            out.push((h * 16 + l) as u8);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Whether `target`, named in the file at vault path `base`, reaches out of the project: a
/// `file:` URL, an absolute, home (`~`), drive or UNC path, or `..` past the vault root. Web and
/// `data:` URLs do not; nor does a plain relative path.
fn escapes(base: &str, target: &str) -> bool {
    let t = percent_decoded(target.trim()).replace('\\', "/");
    let lower = t.to_ascii_lowercase();
    let b = lower.as_bytes();
    if lower.starts_with("file:")
        || t.starts_with(['/', '~'])
        || (b.len() > 1 && b[0].is_ascii_alphabetic() && b[1] == b':')
    {
        return true;
    }
    let scheme = lower.split_once(':').is_some_and(|(s, _)| {
        !s.is_empty() && s.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'+' | b'.' | b'-'))
    });
    if scheme {
        return false;
    }
    let mut depth = base.matches('/').count() as isize;
    for seg in t.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                depth -= 1;
                if depth < 0 {
                    return true;
                }
            }
            _ => depth += 1,
        }
    }
    false
}

/// Whether raw HTML or CSS (`<script src=…>`, `url(…)`, `@import "…"`) refers to a file out of
/// the project — what pandoc's `embed-resources` would read into the page. Every attribute value
/// and every token in it counts; an HTML character reference anywhere in one counts as such a
/// file, since it could spell one. The same test as [`GUARD`]'s `unsafe_raw`.
fn raw_escapes(base: &str, raw: &str) -> bool {
    /// The value starting at `from`: quoted, or up to whatever ends a bare one.
    fn value_at(raw: &str, from: usize) -> &str {
        let rest = raw[from..].trim_start();
        match rest.chars().next() {
            Some(q @ ('"' | '\'')) => rest[1..].split(q).next().unwrap_or(""),
            _ => rest
                .split(|c: char| c.is_whitespace() || matches!(c, '>' | ')' | ';' | '"' | '\''))
                .next()
                .unwrap_or(""),
        }
    }
    let lower = raw.to_ascii_lowercase();
    let mut values: Vec<&str> = lower.match_indices('=').map(|(i, _)| value_at(raw, i + 1)).collect();
    for mark in ["url(", "@import"] {
        values.extend(lower.match_indices(mark).map(|(i, _)| value_at(raw, i + mark.len())));
    }
    values.iter().any(|v| {
        v.contains('&')
            || v.split(|c: char| c.is_whitespace() || c == ',').any(|t| !t.is_empty() && escapes(base, t))
    })
}

/// A metadata string from the file at vault path `base`, as a render may have it: a path from
/// the vault root (`/refs.bib`) made relative to `base` — the same file, as Quarto reads it — and
/// `None` for a path out of the project, or raw HTML or CSS that names one.
fn guard_string(base: &str, s: &str) -> Option<String> {
    if s.contains('<') || s.contains("url(") || s.contains("@import") {
        return (!raw_escapes(base, s)).then(|| s.to_owned());
    }
    let t = s.trim();
    if t.is_empty() || t.contains(char::is_whitespace) {
        return Some(s.to_owned());
    }
    if t.starts_with('/') && !t.starts_with("//") && !escapes("", &t[1..]) {
        let inside = crate::projection::Projection::normalize_relative(base, t)?;
        return Some(relative_from(base, &inside));
    }
    (!escapes(base, t)).then(|| s.to_owned())
}

/// Metadata from the file at vault path `base` without [`DENIED_KEYS`] and with every string
/// through [`guard_string`]. A `from:` reader keeps pandoc off YAML blocks ([`READER`]).
fn guard_value(base: &str, value: &mut serde_yaml_ng::Value) {
    use serde_yaml_ng::Value;
    match value {
        Value::Mapping(m) => {
            m.retain(|k, _| !k.as_str().is_some_and(denied_key));
            let keys: Vec<Value> = m.keys().cloned().collect();
            for k in keys {
                let reader = k.as_str().is_some_and(|k| matches!(k, "from" | "reader"));
                let keep = match m.get_mut(&k) {
                    Some(Value::String(s)) if reader => {
                        let base_reader =
                            s.replace("+yaml_metadata_block", "").replace("-yaml_metadata_block", "");
                        *s = format!("{base_reader}-yaml_metadata_block");
                        true
                    }
                    Some(Value::String(s)) => match guard_string(base, s) {
                        Some(g) => {
                            *s = g;
                            true
                        }
                        None => false,
                    },
                    Some(v) => {
                        guard_value(base, v);
                        true
                    }
                    None => true,
                };
                if !keep {
                    tracing::warn!(key = ?k, "render: a path out of the project dropped");
                    m.remove(&k);
                }
            }
        }
        Value::Sequence(items) => {
            items.retain_mut(|v| match v {
                Value::String(s) => match guard_string(base, s) {
                    Some(g) => {
                        *s = g;
                        true
                    }
                    None => false,
                },
                v => {
                    guard_value(base, v);
                    true
                }
            });
        }
        Value::Tagged(t) => guard_value(base, &mut t.value),
        _ => {}
    }
}

/// A YAML file from the vault (a `_metadata.yml`, the `_quarto.yml`, a brand file) through
/// [`guard_value`]; `None` if it is not YAML we can read — then it is not laid out at all.
fn guard_yaml(base: &str, text: &str) -> Option<serde_yaml_ng::Value> {
    let mut value: serde_yaml_ng::Value = serde_yaml_ng::from_str(text).ok()?;
    value.apply_merge().ok()?;
    guard_value(base, &mut value);
    Some(value)
}

/// The note with every YAML block Quarto reads — the front matter, and any further one at the
/// top level — through [`guard_value`]. A block left as it was is kept byte for byte; one that
/// changed is written anew. Front matter that is not YAML we can read fails the render (Quarto
/// would fail it too); a later block that is not becomes a rule and text, as pandoc reads it then.
/// Fenced code is skipped, as Quarto skips it.
fn guard_metadata(note_path: &str, text: &str) -> Result<String> {
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let bare = |l: &str| l.trim_end_matches(['\n', '\r']).trim_end().to_owned();
    let mut out = String::with_capacity(text.len());
    let mut fence: Option<(char, usize)> = None;
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let blank_before = i == 0 || lines[i - 1].trim().is_empty();
        let opens =
            fence.is_none() && bare(line) == "---" && lines.get(i + 1).is_some_and(|l| !l.trim().is_empty());
        let end = if opens {
            (i + 1..lines.len()).find(|&j| matches!(bare(lines[j]).as_str(), "---" | "..."))
        } else {
            None
        };
        if let Some(end) = end {
            let yaml: String = lines[i + 1..end].concat();
            let parsed = serde_yaml_ng::from_str::<serde_yaml_ng::Value>(&yaml)
                .ok()
                .filter(|v| v.is_mapping())
                .and_then(|mut v| v.apply_merge().ok().map(|()| v));
            match parsed {
                Some(v) => {
                    let mut guarded = v.clone();
                    guard_value(note_path, &mut guarded);
                    if guarded == v {
                        out.push_str(&lines[i..=end].concat());
                    } else if guarded.as_mapping().is_some_and(|m| !m.is_empty()) {
                        let yaml =
                            serde_yaml_ng::to_string(&guarded).map_err(|e| Error::Export(e.to_string()))?;
                        out.push_str(&format!("---\n{yaml}---\n"));
                    } // else nothing was left of it: no block at all (`{}` is not one to Quarto).
                    i = end + 1;
                    continue;
                }
                None if i == 0 => {
                    return Err(Error::Export("the note's front matter is not valid YAML".into()));
                }
                None if blank_before => {
                    out.push_str("* * *\n");
                    i += 1;
                    continue;
                }
                None => {}
            }
        }
        match fence {
            Some((ch, n)) if crate::import::closes_fence(line, ch, n) => fence = None,
            Some(_) => {}
            None => fence = crate::import::fence_marker(line),
        }
        out.push_str(line);
        i += 1;
    }
    Ok(out)
}

/// Shortcodes that read files or the environment — `include`, `embed`, `env` — escaped
/// (`{{{< … >}}}`), so the page shows them rather than doing them. Everywhere in the text:
/// Quarto expands them in code blocks too.
fn defuse_shortcodes(text: &str) -> String {
    const READS: &[&str] = &["include", "embed", "env"];
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("{{<") {
        let escaped = rest[..at].ends_with('{');
        let after = &rest[at + 3..];
        let name: String = after
            .trim_start()
            .chars()
            .take_while(|c| c.is_alphanumeric() || matches!(c, '-' | '_'))
            .collect();
        let close = after.find(">}}");
        match close {
            Some(end) if !escaped && READS.contains(&name.to_ascii_lowercase().as_str()) => {
                out.push_str(&rest[..at]);
                out.push_str("{{{<");
                out.push_str(&after[..end]);
                out.push_str(">}}}");
                rest = &after[end + 3..];
            }
            _ => {
                out.push_str(&rest[..at + 3]);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// A stylesheet from the vault at `base` with every `url(…)`, `@import`, `@use` or `@forward`
/// target — and any quoted string — that leads out of the project emptied: Sass would read the
/// file, and `embed-resources` put it in the page. Every quote is taken for the start of a
/// string (one in a comment too): more is checked, nothing is skipped.
fn guard_stylesheet(base: &str, css: &str) -> String {
    let lower = css.to_ascii_lowercase();
    let mut cut: Vec<std::ops::Range<usize>> = Vec::new();
    for mark in ["url(", "@import", "@use", "@forward", "\"", "'"] {
        for (at, _) in lower.match_indices(mark) {
            let from = at + mark.len();
            let range = if let Some(q) = mark.chars().next().filter(|c| matches!(c, '"' | '\'')) {
                from..css[from..].find(q).map_or(css.len(), |e| from + e)
            } else {
                let from = from + (css[from..].len() - css[from..].trim_start().len());
                if css[from..].starts_with(['"', '\'']) {
                    continue; // a string: checked as one
                }
                let end = css[from..].find(|c: char| c.is_whitespace() || matches!(c, ')' | ';' | ','));
                from..end.map_or(css.len(), |e| from + e)
            };
            let target = css[range.clone()].trim();
            // A CSS string ends at its line; past that this was no string at all.
            if !target.is_empty() && !target.contains('\n') && escapes(base, target) {
                cut.push(range);
            }
        }
    }
    if cut.is_empty() {
        return css.to_owned();
    }
    tracing::warn!(stylesheet = base, "render: a reference out of the project dropped");
    cut.sort_by_key(|r| r.start);
    let mut out = String::with_capacity(css.len());
    let mut at = 0;
    for r in cut {
        if r.start >= at {
            out.push_str(&css[at..r.start]);
        }
        at = at.max(r.end);
    }
    out.push_str(&css[at..]);
    out
}

/// The filter every render runs through, last before the page is written (`at: post-render`):
/// an image, raw HTML, or an attribute (a slide's `data-background-image`, say) that names a
/// file out of the project is dropped — `embed-resources` would read it into the page. The same
/// tests as [`escapes`] and [`raw_escapes`]; `__DEPTH__` is how many folders down the note is.
const GUARD: &str = r#"
local DEPTH = __DEPTH__

local function escapes(t)
  t = t:match('^%s*(.-)%s*$')
  t = t:gsub('%%(%x%x)', function(h) return string.char(tonumber(h, 16)) end)
  t = t:gsub('\\', '/')
  local lower = t:lower()
  if lower:find('^file:') or t:find('^[/~]') or t:find('^%a:') then return true end
  if lower:find('^[%w+.-]+:') then return false end
  local depth = DEPTH
  for seg in t:gmatch('[^/]+') do
    if seg == '..' then
      depth = depth - 1
      if depth < 0 then return true end
    elseif seg ~= '.' then
      depth = depth + 1
    end
  end
  return false
end

local function unsafe(value)
  if value:find('&', 1, true) then return true end
  for tok in value:gmatch('[^%s,]+') do
    if escapes(tok) then return true end
  end
  return false
end

local function unsafe_raw(text)
  local lower = text:lower()
  for v in lower:gmatch('=%s*"([^"]*)') do if unsafe(v) then return true end end
  for v in lower:gmatch("=%s*'([^']*)") do if unsafe(v) then return true end end
  for v in lower:gmatch('=%s*([^%s>"\';)]+)') do if unsafe(v) then return true end end
  for v in lower:gmatch('url%(%s*["\']?([^"\')]*)') do if unsafe(v) then return true end end
  for v in lower:gmatch('@import%s*["\']?([^"\';]*)') do if unsafe(v) then return true end end
  return false
end

local function clean(el)
  local drop = {}
  for k, v in pairs(el.attributes) do
    if unsafe(v) then drop[#drop + 1] = k end
  end
  for _, k in ipairs(drop) do el.attributes[k] = nil end
  return el
end

local function raw(el)
  if (el.format:find('html') or el.format:find('revealjs')) and unsafe_raw(el.text) then return {} end
end

return {
  {
    RawInline = raw,
    RawBlock = raw,
    Image = function(img)
      if escapes(img.src) then return img.caption end
      return clean(img)
    end,
    Link = clean,
    Div = clean,
    Span = clean,
    Header = clean,
    CodeBlock = clean,
    Code = clean,
    Figure = clean,
    Table = clean,
  },
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn quarto() -> Option<PathBuf> {
        std::env::var_os("LEMMATE_TEST_QUARTO").map(PathBuf::from).filter(|p| p.is_file())
    }

    const ATTACHMENTS: &[&str] = &["attachments/pic.png", "dir/local.png"];

    fn atts() -> Vec<String> {
        ATTACHMENTS.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn formats_parse() {
        assert_eq!(Format::parse("HTML"), Some(Format::Html));
        assert_eq!(Format::parse("pdf"), Some(Format::Pdf));
        assert_eq!(Format::parse("slides"), Some(Format::RevealJs));
        assert_eq!(Format::parse("beamer"), None);
        assert_eq!(Format::Pdf.quarto_name(), "typst");
        assert_eq!(Format::RevealJs.extension(), "html");
    }

    #[test]
    fn the_preview_renders_what_the_note_declares() {
        let deck =
            "---\ntitle: T\nformat:\n  revealjs:\n    theme: [default, cern.scss]\n  pdf: default\n---\nx";
        assert_eq!(preview_format(deck), Format::RevealJs);
        assert_eq!(preview_format("---\nformat: revealjs\n---\n"), Format::RevealJs);
        assert_eq!(preview_format("---\nformat: [pdf, revealjs+code]\n---\n"), Format::RevealJs);
        assert_eq!(
            preview_format("---\nformat:\n  pdf: default\n  html: default\n  revealjs: default\n---\n"),
            Format::Html
        );
        assert_eq!(preview_format("---\nformat: docx\n---\n"), Format::Html, "not a page: a plain one then");
        assert_eq!(preview_format("# no front matter"), Format::Html);
        assert_eq!(preview_format("---\nformat: [\n---\n"), Format::Html);
    }

    #[test]
    fn auto_renders_the_first_format_the_note_declares() {
        assert_eq!(
            declared_format("---\nformat:\n  pdf:\n    toc: true\n  revealjs: default\n---\n"),
            Format::Pdf
        );
        assert_eq!(declared_format("---\nformat: docx\n---\n"), Format::Docx);
        assert_eq!(
            declared_format("---\nformat: [beamer, typst]\n---\n"),
            Format::Pdf,
            "beamer is not ours to make"
        );
        assert_eq!(
            declared_format("---\nformat:\n  revealjs:\n    theme: [default, cern.scss]\n---\n"),
            Format::RevealJs
        );
        assert_eq!(declared_format("---\ntitle: x\n---\n"), Format::Html);
        assert_eq!(
            preview_format("---\nformat: [pdf, revealjs]\n---\n"),
            Format::RevealJs,
            "a preview is a page"
        );
    }

    #[test]
    fn a_page_for_viewing_gets_storage_first() {
        let page = with_storage(
            b"<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><script>x()</script>".to_vec(),
        );
        let page = String::from_utf8(page).unwrap();
        let stand_in = page.find("sessionStorage").unwrap();
        assert!(page.find("<head>").unwrap() < stand_in && stand_in < page.find("x()").unwrap());
        assert!(String::from_utf8(with_storage(b"<p>no head</p>".to_vec())).unwrap().starts_with("<script>"));
    }

    #[test]
    fn a_deck_sends_its_speaker_view_to_lemmate() {
        let page = String::from_utf8(with_speaker(
            b"<html><head></head><body></body></html>".to_vec(),
            "V1",
            "N2",
            "R3",
        ))
        .unwrap();
        assert!(page.contains("/speaker.html?") && page.contains("vault=V1&note=N2&render=R3"), "{page}");
        let odd = with_speaker(b"<head></head>".to_vec(), "V1", "N\"2", "R3");
        assert_eq!(odd, b"<head></head>", "anything but plain ids adds nothing");
    }

    #[test]
    fn a_kept_render_is_had_again_only_with_its_note() {
        let cache = RenderCache::new();
        let id = cache.put("note-a", b"<p>page</p>", "text/html", "inline");
        assert_eq!(cache.get(&id, "note-a").unwrap().0, b"<p>page</p>");
        assert!(cache.get(&id, "note-b").is_none(), "an id is only good with its note");
        assert!(cache.get("nonsense", "note-a").is_none());
        for i in 0..RenderCache::KEEP {
            cache.put("note-a", format!("{i}").as_bytes(), "text/html", "inline");
        }
        assert!(cache.get(&id, "note-a").is_none(), "the oldest goes first");
    }

    #[test]
    fn served_inline_under_the_notes_name() {
        assert_eq!(disposition("dir/Talk.qmd", Format::Pdf), "inline; filename=\"Talk.pdf\"");
        assert_eq!(disposition("a \"b\".md", Format::Html), "inline; filename=\"a b.html\"");
        assert_eq!(disposition("", Format::Docx), "inline; filename=\"note.docx\"");
    }

    #[test]
    fn wikilinks_become_labels_and_image_embeds_images() {
        let md = "See [[Other Note]], [[Plan#Goals]], [[Plan|the plan]] and [[Plan#^goal]].\n\
                  ![[pic.png]] ![[pic.png|300]] ![[local.png]] ![[Some Note]]\n";
        let out = prepare(md, "dir/note.qmd", &atts());
        assert_eq!(
            out,
            "See [Other Note]{.wikilink}, [Plan › Goals]{.wikilink}, [the plan]{.wikilink} and \
             [Plan › goal]{.wikilink}.\n\
             ![](<../attachments/pic.png>) ![](<../attachments/pic.png>){width=300px} \
             ![](<../dir/local.png>) [Some Note]{.wikilink}\n"
        );
        // At the vault root there is nothing to climb out of.
        assert_eq!(prepare("![[pic.png]]", "note.md", &atts()), "![](<attachments/pic.png>)");
    }

    #[test]
    fn code_is_left_alone() {
        let md = "```python\nx = [[1, 2]]\n```\nInline `[[not a link]]` and ``a [[b]] ``, then [[Real]].\n\
                  ~~~~\n[[inside tilde]]\n~~~~\n";
        let out = prepare(md, "n.qmd", &[]);
        assert_eq!(
            out,
            "```python\nx = [[1, 2]]\n```\nInline `[[not a link]]` and ``a [[b]] ``, then [Real]{.wikilink}.\n\
             ~~~~\n[[inside tilde]]\n~~~~\n"
        );
    }

    #[test]
    fn labels_are_escaped_and_table_pipes_understood() {
        assert_eq!(prepare("[[a_b*c]]", "n.md", &[]), "[a\\_b\\*c]{.wikilink}");
        assert_eq!(prepare("| [[Plan\\|label]] |", "n.md", &[]), "| [label]{.wikilink} |");
        assert_eq!(prepare("unclosed [[link", "n.md", &[]), "unclosed [[link");
    }

    #[test]
    fn project_paths_stay_inside_the_project() {
        assert_eq!(source_path("dir/Note.md"), PathBuf::from("dir/Note.qmd"));
        assert_eq!(source_path("Talk.qmd"), PathBuf::from("Talk.qmd"));
        assert_eq!(source_path("../escape.md"), PathBuf::from("note.qmd"));
        assert_eq!(safe_relative("a/../b"), None);
        assert_eq!(safe_relative("/abs/x.png"), Some(PathBuf::from("abs/x.png")));
    }

    #[test]
    fn project_yaml_names_the_bibliography() {
        let y = project_yaml(
            None,
            &[("bibliography", PathBuf::from("/tmp/it's/references.bib"))],
            Path::new("/g.lua"),
        )
        .unwrap();
        let v: serde_yaml_ng::Value = serde_yaml_ng::from_str(&y).unwrap();
        assert_eq!(v["project"]["type"], "default");
        assert_eq!(v["format"]["html"]["embed-resources"], true);
        assert_eq!(v["format"]["revealjs"]["embed-resources"], true);
        assert_eq!(v["bibliography"], "/tmp/it's/references.bib");
    }

    #[test]
    fn the_vaults_project_file_is_the_base_but_not_the_boss() {
        let vault = "project:\n  type: website\n  output-dir: _site\n  pre-render: rm -rf /\n\
                     format:\n  html:\n    theme: [cosmo, styles/custom.scss]\n    embed-resources: false\n\
                     bibliography: refs/mine.bib\nauthor: Juan\n";
        let extra = [
            ("bibliography", PathBuf::from("/p/export/references.bib")),
            ("csl", PathBuf::from("/p/export/style.csl")),
        ];
        let v: serde_yaml_ng::Value =
            serde_yaml_ng::from_str(&project_yaml(Some(vault), &extra, Path::new("/g.lua")).unwrap())
                .unwrap();
        assert_eq!(v["project"].as_mapping().unwrap().len(), 1, "only `type` survives: {v:?}");
        assert_eq!(v["project"]["type"], "default");
        assert_eq!(v["format"]["html"]["theme"][1], "styles/custom.scss", "the shared theme stays");
        assert_eq!(v["format"]["html"]["embed-resources"], true, "the pane needs one page");
        assert_eq!(v["bibliography"], "refs/mine.bib", "the vault's own bibliography wins over export/");
        assert_eq!(v["csl"], "/p/export/style.csl");
        assert_eq!(v["author"], "Juan");
        // A bare format name still gets its options.
        let v: serde_yaml_ng::Value =
            serde_yaml_ng::from_str(&project_yaml(Some("format: html\n"), &[], Path::new("/g.lua")).unwrap())
                .unwrap();
        assert_eq!(v["format"]["html"]["embed-resources"], true);
        assert!(
            project_yaml(Some("format: [\n"), &[], Path::new("/g.lua"))
                .unwrap_err()
                .to_string()
                .contains("not valid YAML")
        );
    }

    #[test]
    fn stderr_is_trimmed_to_the_message() {
        let err = "Starting render\n\u{1b}[91mERROR: YAMLException: unexpected end (4:1)\n\n 2 | title: \"Broken\n-----^\n\nStack trace:\n    at Object.x (file:///q.js:1:2)\n\u{1b}[39m";
        assert_eq!(tail(err), "quarto: YAMLException: unexpected end (4:1)\n\n 2 | title: \"Broken\n-----^");
        assert_eq!(
            tail("pandoc\n  to: html\nsomething went wrong\n"),
            "quarto: pandoc\n  to: html\nsomething went wrong"
        );
        assert_eq!(tail(""), "quarto failed without saying why");
    }

    #[test]
    fn missing_quarto_is_a_clear_error() {
        let opts = RenderOptions { quarto: Some(PathBuf::from("/nonexistent/quarto")), ..Default::default() };
        let err = render("n.md", "# x", Format::Html, &[], |_| None, &opts).unwrap_err().to_string();
        assert!(err.contains("running /nonexistent/quarto"), "{err}");
        assert!(!quarto_available(Some(Path::new("/nonexistent/quarto"))));
    }

    /// Runs only when LEMMATE_TEST_QUARTO points at a quarto binary.
    #[test]
    fn renders_html_pdf_and_docx_without_running_code() {
        let Some(bin) = quarto() else {
            eprintln!("skipped: set LEMMATE_TEST_QUARTO");
            return;
        };
        let opts = RenderOptions { quarto: Some(bin), ..Default::default() };
        // A 1×1 PNG, referenced both ways a note can.
        let png: &[u8] = &[
            0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 13, 0x49, 0x48, 0x44, 0x52, 0, 0, 0, 1,
            0, 0, 0, 1, 8, 6, 0, 0, 0, 0x1f, 0x15, 0xc4, 0x89, 0, 0, 0, 13, 0x49, 0x44, 0x41, 0x54, 0x78,
            0x9c, 0x63, 0xf8, 0xcf, 0xc0, 0xf0, 0x1f, 0, 0x05, 0, 0x01, 0xff, 0x89, 0x99, 0x3d, 0x1d, 0, 0,
            0, 0, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
        ];
        let read = |p: &str| (p == "attachments/pic.png").then(|| png.to_vec());
        let md =
            "---\ntitle: Rendered\n---\n\nSee [[Other]].\n\n![[pic.png]]\n\n```{python}\nprint(6 * 7)\n```\n";
        let atts = vec!["attachments/pic.png".to_owned()];
        let (html, mime) = render("dir/Talk.qmd", md, Format::Html, &atts, read, &opts).unwrap();
        let html = String::from_utf8(html).unwrap();
        assert!(mime.starts_with("text/html"));
        assert!(html.contains("Rendered") && html.contains("wikilink"), "title and link label");
        assert!(html.contains("data:image/png"), "the image is embedded");
        assert!(!html.contains("note_files") && !html.contains("Talk_files"), "self-contained");
        assert!(!html.contains(">42<"), "code cells are not executed");
        let (pdf, mime) = render("dir/Talk.qmd", md, Format::Pdf, &atts, read, &opts).unwrap();
        assert_eq!(mime, "application/pdf");
        assert!(pdf.starts_with(b"%PDF"));
        let (docx, _) = render("dir/Talk.qmd", md, Format::Docx, &atts, read, &opts).unwrap();
        assert!(docx.starts_with(b"PK"));
    }

    /// Runs only when LEMMATE_TEST_QUARTO points at a quarto binary. A theme named in front
    /// matter, the partial it imports, and the vault's `_quarto.yml` with a stylesheet of its
    /// own all reach Quarto — and the `pre-render` script that file asks for does not run.
    #[test]
    fn companion_files_and_the_vaults_project_file_reach_quarto() {
        let Some(bin) = quarto() else {
            eprintln!("skipped: set LEMMATE_TEST_QUARTO");
            return;
        };
        let opts = RenderOptions { quarto: Some(bin), ..Default::default() };
        let files: std::collections::HashMap<&str, &str> = [
            ("_quarto.yml", "project:\n  pre-render: does-not-exist.sh\nformat:\n  html:\n    css: shared/site.css\n"),
            ("shared/site.css", ".from-vault { color: #123456; }\n"),
            (
                "dir/custom.scss",
                "/*-- scss:defaults --*/\n@import 'vars';\n$body-color: $marker;\n/*-- scss:rules --*/\n.from-theme { color: #abcdef; }\n",
            ),
            ("dir/_vars.scss", "$marker: #fedcba;\n"),
        ]
        .into_iter()
        .collect();
        let atts: Vec<String> = files.keys().map(|k| (*k).to_owned()).collect();
        let read = |p: &str| files.get(p).map(|c| c.as_bytes().to_vec());
        let md = "---\ntitle: Themed\nformat:\n  html:\n    theme: [cosmo, custom.scss]\n---\n\nBody.\n";
        let (html, _) = render("dir/Talk.qmd", md, Format::Html, &atts, read, &opts).unwrap();
        let html = String::from_utf8(html).unwrap().to_ascii_lowercase();
        // Embedded stylesheets arrive as `data:text/css,…` URLs, where `#` is `%23`.
        let has =
            |colour: &str| html.contains(&format!("#{colour}")) || html.contains(&format!("%23{colour}"));
        assert!(has("abcdef"), "the theme's rules");
        assert!(has("fedcba"), "a variable from the partial it imports");
        assert!(has("123456"), "the stylesheet the vault's _quarto.yml names");
    }

    #[test]
    fn what_reaches_out_of_the_project() {
        for out in ["/etc/passwd", "~/x", "file:///etc/x", "C:\\x", "../x", "a/../../x", "%2e%2e/x", "..\\x"]
        {
            assert!(escapes("n.md", out), "{out}");
        }
        for inside in
            ["x.png", "./a/b.css", "https://example.com/x.png", "data:image/png;base64,AA", "a/../b"]
        {
            assert!(!escapes("n.md", inside), "{inside}");
        }
        assert!(!escapes("dir/n.md", "../x.png") && escapes("dir/n.md", "../../x.png"));
        assert!(raw_escapes("n.md", "<script src=\"/etc/hostname\"></script>"));
        assert!(raw_escapes("n.md", "<img src='../../x'>") && raw_escapes("n.md", "<img src=/x>"));
        assert!(raw_escapes("n.md", "<img src=\"&#47;etc&#47;x\">"), "a character reference could spell one");
        assert!(raw_escapes("n.md", "<style>p { background: url(/etc/x) }</style>"));
        assert!(raw_escapes("n.md", "<img srcset=\"a.png 1x, /etc/x 2x\">"));
        assert!(!raw_escapes("n.md", "<img src=\"pic.png\" width=\"30\"><script src=\"https://x/y.js\">"));
    }

    #[test]
    fn metadata_can_neither_run_code_nor_reach_out() {
        let note = "---\ntitle: Kept\nfilters: [evil.lua]\nshortcodes: [s.lua]\ninclude-in-header: /etc/hostname\n\
                    bibliography: /refs.bib\ncsl: ../../../etc/x.csl\nheader-includes: <script src=\"/etc/x\"></script>\n\
                    format:\n  html:\n    filters: [evil.lua]\n    theme: [cosmo, custom.scss]\n    from: markdown+emoji\n  \
                    pdf:\n    latex-auto-install: true\n---\n\nBody.\n\n---\nlua_filters: [x.lua]\nfilters: [evil.lua]\n---\n\n\
                    ```\n---\nfilters: shown, not run\n---\n```\n\n---\nkept: as it was # a comment\n---\n";
        let out = guard_metadata("dir/n.qmd", note).unwrap();
        let front: serde_yaml_ng::Value = serde_yaml_ng::from_str(&note_front(&out)).unwrap();
        assert_eq!(front["title"], "Kept");
        for gone in ["filters", "shortcodes", "include-in-header", "csl", "header-includes"] {
            assert!(front.get(gone).is_none(), "{gone}: {out}");
        }
        assert_eq!(front["bibliography"], "../refs.bib", "from the vault root, as Quarto reads it");
        assert!(front["format"]["html"].get("filters").is_none());
        assert_eq!(front["format"]["html"]["theme"][1], "custom.scss");
        assert_eq!(front["format"]["html"]["from"], "markdown+emoji-yaml_metadata_block");
        assert!(front["format"]["pdf"].get("latex-auto-install").is_none());
        assert!(!out.contains("evil.lua") && !out.contains("x.lua"), "{out}");
        assert!(out.contains("filters: shown, not run"), "code is code: {out}");
        assert!(out.contains("kept: as it was # a comment"), "a harmless block stays as written");
        // A block we cannot read is no metadata to anyone; front matter we cannot read fails.
        assert_eq!(guard_metadata("n.md", "x\n\n---\na: [\n---\n").unwrap(), "x\n\n* * *\na: [\n---\n");
        assert!(guard_metadata("n.md", "---\na: [\n---\n").is_err());
        assert_eq!(
            guard_metadata("n.md", "x\n\n---\nfilters: [a.lua]\n---\ny\n").unwrap(),
            "x\n\ny\n",
            "emptied: gone"
        );
        assert_eq!(guard_metadata("n.md", "Title\n---\n\ntext\n").unwrap(), "Title\n---\n\ntext\n");
    }

    fn note_front(text: &str) -> String {
        let (range, _) = crate::frontmatter::block(text).unwrap();
        text[range].to_owned()
    }

    #[test]
    fn shortcodes_that_read_files_are_shown_not_run() {
        assert_eq!(
            defuse_shortcodes(
                "{{< include /etc/x >}} {{<env HOME>}} {{< EMBED a.ipynb >}} {{< pagebreak >}}"
            ),
            "{{{< include /etc/x >}}} {{{<env HOME>}}} {{{< EMBED a.ipynb >}}} {{< pagebreak >}}"
        );
        assert_eq!(defuse_shortcodes("{{{< include x >}}}"), "{{{< include x >}}}", "escaped already");
        assert_eq!(defuse_shortcodes("{{< include x"), "{{< include x", "unclosed: nothing to do");
    }

    #[test]
    fn stylesheets_keep_to_the_project() {
        let css = "@import '/etc/x';\n@use \"../../../y\";\n@import vars;\n.a { background: url(../../../z.png); }\n\
                   .b { background: url(img/b.png); } /* don't */ .c { background: url( /etc/w ) }\n";
        let out = guard_stylesheet("styles/site.scss", css);
        assert_eq!(
            out,
            "@import '';\n@use \"\";\n@import vars;\n.a { background: url(); }\n\
             .b { background: url(img/b.png); } /* don't */ .c { background: url(  ) }\n"
        );
        assert_eq!(guard_stylesheet("a.css", ".x { color: red }"), ".x { color: red }");
    }

    #[test]
    fn the_project_runs_only_our_filter() {
        let vault = "filters: [evil.lua]\nformat:\n  html:\n    include-in-header: /etc/hostname\n";
        let v: serde_yaml_ng::Value =
            serde_yaml_ng::from_str(&project_yaml(Some(vault), &[], Path::new("/w/guard.lua")).unwrap())
                .unwrap();
        assert_eq!(v["filters"].as_sequence().unwrap().len(), 1);
        assert_eq!(v["filters"][0]["path"], "/w/guard.lua");
        assert_eq!(v["filters"][0]["at"], "post-render");
        assert!(v["format"]["html"].get("include-in-header").is_none());
        for f in ["html", "revealjs", "typst", "docx"] {
            assert_eq!(v["format"][f]["from"], READER, "{f}");
        }
    }

    /// A stand-in for quarto: a shell script.
    #[cfg(unix)]
    fn fake_quarto(dir: &Path, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let bin = dir.join("quarto");
        std::fs::write(&bin, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        bin
    }

    /// A render that runs too long is killed with everything it started, and Quarto never sees
    /// the environment it was started from beyond the basics.
    #[cfg(unix)]
    #[test]
    fn a_stuck_render_is_killed_with_what_it_started() {
        let dir = crate::pandoc::tempdir().unwrap();
        let (env, pid) = (dir.join("env"), dir.join("pid"));
        let bin = fake_quarto(
            &dir,
            &format!(
                "env > {env:?}; sleep 60 & echo $! > {pid:?}; wait",
                env = env.display().to_string(),
                pid = pid.display().to_string()
            ),
        );
        let opts =
            RenderOptions { quarto: Some(bin), timeout: Duration::from_millis(500), ..Default::default() };
        let err = render("n.md", "# x", Format::Html, &[], |_| None, &opts).unwrap_err().to_string();
        assert!(err.contains("did not finish"), "{err}");
        let env = std::fs::read_to_string(&env).unwrap();
        assert!(env.contains("PATH=") && !env.contains("CARGO_MANIFEST_DIR"), "{env}");
        let pid: i32 = std::fs::read_to_string(&pid).unwrap().trim().parse().unwrap();
        let gone = (0..50).any(|_| {
            let state = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
            if state.is_empty() || state.split(' ').nth(2) == Some("Z") {
                return true;
            }
            std::thread::sleep(Duration::from_millis(100));
            false
        });
        assert!(gone, "what quarto started was killed too");
    }

    /// Runs only when LEMMATE_TEST_QUARTO points at a quarto binary. A note — and the vault's
    /// `_metadata.yml` — cannot run a Lua filter, and nothing it does reads a file outside the
    /// project or the environment the render was started from.
    #[test]
    fn a_render_runs_no_filter_and_reads_nothing_from_the_host() {
        let Some(bin) = quarto() else {
            eprintln!("skipped: set LEMMATE_TEST_QUARTO");
            return;
        };
        let opts = RenderOptions { quarto: Some(bin), ..Default::default() };
        let outside = crate::pandoc::tempdir().unwrap();
        let secret = outside.join("secret.txt");
        std::fs::write(&secret, "HOST-SECRET-7731\n").unwrap();
        let marker = outside.join("ran");
        let lua = format!("io.open({:?}, 'w'):write('x')\n", marker.display().to_string());
        let deep = "../".repeat(30);
        let s = secret.display().to_string();
        let files: std::collections::HashMap<String, String> = [
            ("dir/evil.lua".to_owned(), lua.clone()),
            ("dir/_metadata.yml".to_owned(), "filters: [evil.lua]\n".to_owned()),
            ("dir/site.css".to_owned(), format!("@import url({deep}{s});\n")),
        ]
        .into_iter()
        .collect();
        let atts: Vec<String> = files.keys().cloned().collect();
        let read = |p: &str| files.get(p).map(|c| c.as_bytes().to_vec());
        let md = format!(
            "---\ntitle: Guarded\nfilters: [evil.lua]\ninclude-in-header: {s}\ncss: site.css\n---\n\n\
             {{{{< include {deep}{s} >}}}}\n\nHome: {{{{< env CARGO_MANIFEST_DIR >}}}}\n\n\
             <img src=\"{deep}{s}\">\n\n![x]({deep}{s})\n\n---\nfilters: [evil.lua]\n---\n"
        );
        for format in [Format::Html, Format::RevealJs] {
            let (bytes, _) = render("dir/Talk.qmd", &md, format, &atts, read, &opts).unwrap();
            assert!(!marker.exists(), "{format:?}: no filter ran");
            let page = String::from_utf8_lossy(&bytes);
            assert!(!page.contains("HOST-SECRET") && !page.contains("SE9TVC1TRUNSRVQ"), "{format:?}");
            assert!(
                !page.contains(env!("CARGO_MANIFEST_DIR")),
                "{format:?}: the environment is not the page's"
            );
            if format == Format::Html {
                assert!(page.contains("Guarded"), "and it still renders");
            }
        }
    }
}

//! Rendering a note through Quarto (SPEC §5.6, §12): "Render with Quarto" for `.qmd` notes —
//! and any other note, which Quarto reads just as well.
//!
//! A render never runs code: `--no-execute` is always passed, so a `{python}` cell is shown,
//! not run (SPEC §14). The note's own front matter *is* honoured — that is the point of Quarto,
//! and it can name Lua filters and files to include. On a server that is code and file reads
//! on the host at an editor's say-so, which is why the server can switch rendering off.
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

/// What the preview pane renders a note as: the first page-like format its front matter
/// declares — so a deck (`format: revealjs`, or a `revealjs:` block ahead of `pdf:`) previews
/// as slides, with the theme and options written for it — else a plain page. A render asked
/// for as `"preview"` resolves through here.
pub fn preview_format(text: &str) -> Format {
    use serde_yaml_ng::Value;
    let Some((yaml, _)) = crate::frontmatter::block(text) else { return Format::Html };
    let Ok(Value::Mapping(front)) = serde_yaml_ng::from_str::<Value>(&text[yaml]) else {
        return Format::Html;
    };
    let declared: Vec<String> = match front.get("format") {
        Some(Value::String(f)) => vec![f.clone()],
        Some(Value::Sequence(fs)) => fs.iter().filter_map(|f| f.as_str().map(str::to_owned)).collect(),
        Some(Value::Mapping(m)) => m.keys().filter_map(|k| k.as_str().map(str::to_owned)).collect(),
        _ => Vec::new(),
    };
    declared
        .iter()
        .find_map(|f| match f.split('+').next().unwrap_or(f) {
            "revealjs" => Some(Format::RevealJs),
            "html" => Some(Format::Html),
            _ => None,
        })
        .unwrap_or(Format::Html)
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
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self { quarto: None, timeout: Duration::from_secs(120) }
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
    let result = render_in(&work, note_path, text, format, attachments, read, opts);
    let _ = std::fs::remove_dir_all(&work);
    result
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
        let (Some(safe), Some(bytes)) = (safe_relative(path), read(path)) else { continue };
        if matches!(path.as_str(), "_quarto.yml" | "_quarto.yaml") {
            // Merged into the project file below rather than written as it is.
            vault_project = Some(String::from_utf8_lossy(&bytes).into_owned());
            continue;
        }
        write(&project.join(safe), &bytes)?;
    }
    let extra: Vec<(&str, PathBuf)> = [(BIBLIOGRAPHY, "bibliography"), (CSL, "csl")]
        .into_iter()
        .filter(|(path, _)| files.iter().any(|f| f == path))
        .map(|(path, key)| (key, project.join(path)))
        .collect();
    write(&project.join("_quarto.yml"), project_yaml(vault_project.as_deref(), &extra)?.as_bytes())?;
    write(&project.join(&rel), prepare(text, note_path, attachments).as_bytes())?;

    let bin = quarto_bin(opts.quarto.as_deref());
    let log = work.join("quarto.log");
    let mut child = Command::new(&bin)
        .current_dir(&project)
        .arg("render")
        .arg(&rel)
        // Not `--quiet`: that silences the error along with the progress, and the error is
        // the one part of the log a failed render is read for.
        .args(["--to", format.quarto_name(), "--no-execute"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(std::fs::File::create(&log)?)
        .spawn()
        .map_err(|e| Error::Export(format!("running {}: {e}", bin.display())))?;
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if started.elapsed() > opts.timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Error::Export(format!("quarto did not finish within {}s", opts.timeout.as_secs())));
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    // Named by Quarto after the source, beside it: asking for another name with `--output`
    // quietly stops HTML from embedding its resources.
    let out = project.join(rel.with_extension(format.extension()));
    if !status.success() || !out.is_file() {
        let stderr = std::fs::read_to_string(&log).unwrap_or_default();
        return Err(Error::Export(tail(&stderr)));
    }
    Ok((std::fs::read(out)?, format.mime()))
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
fn project_yaml(vault: Option<&str>, extra: &[(&str, PathBuf)]) -> Result<String> {
    use serde_yaml_ng::{Mapping, Value};
    let mut root = match vault {
        Some(text) if !text.trim().is_empty() => match serde_yaml_ng::from_str::<Value>(text) {
            Ok(Value::Mapping(m)) => m,
            Ok(_) => Mapping::new(),
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
    for name in ["html", "revealjs"] {
        let mut options = match format.remove(name) {
            Some(Value::Mapping(m)) => m,
            _ => Mapping::new(),
        };
        options.insert(key("embed-resources"), Value::Bool(true));
        format.insert(key(name), Value::Mapping(options));
    }
    root.insert(key("format"), Value::Mapping(format));

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
        let y = project_yaml(None, &[("bibliography", PathBuf::from("/tmp/it's/references.bib"))]).unwrap();
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
            serde_yaml_ng::from_str(&project_yaml(Some(vault), &extra).unwrap()).unwrap();
        assert_eq!(v["project"].as_mapping().unwrap().len(), 1, "only `type` survives: {v:?}");
        assert_eq!(v["project"]["type"], "default");
        assert_eq!(v["format"]["html"]["theme"][1], "styles/custom.scss", "the shared theme stays");
        assert_eq!(v["format"]["html"]["embed-resources"], true, "the pane needs one page");
        assert_eq!(v["bibliography"], "refs/mine.bib", "the vault's own bibliography wins over export/");
        assert_eq!(v["csl"], "/p/export/style.csl");
        assert_eq!(v["author"], "Juan");
        // A bare format name still gets its options.
        let v: serde_yaml_ng::Value =
            serde_yaml_ng::from_str(&project_yaml(Some("format: html\n"), &[]).unwrap()).unwrap();
        assert_eq!(v["format"]["html"]["embed-resources"], true);
        assert!(project_yaml(Some("format: [\n"), &[]).unwrap_err().to_string().contains("not valid YAML"));
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
}

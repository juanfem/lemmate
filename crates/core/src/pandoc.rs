//! Export through pandoc (SPEC §12). The app's own parsers never render exports; real pandoc
//! does, with the pandoc extensions that match the SPEC §5 dialect (wikilinks included).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use crate::error::{Error, Result};

/// Output formats the API accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Html,
    Pdf,
    Docx,
    RevealJs,
    Beamer,
    Markdown,
}

impl Format {
    pub fn parse(s: &str) -> Option<Format> {
        Some(match s.to_ascii_lowercase().as_str() {
            "html" => Format::Html,
            "pdf" => Format::Pdf,
            "docx" => Format::Docx,
            "revealjs" | "slides" => Format::RevealJs,
            "beamer" => Format::Beamer,
            "md" | "markdown" | "commonmark" => Format::Markdown,
            _ => return None,
        })
    }
    pub fn pandoc_name(self) -> &'static str {
        match self {
            Format::Html => "html5",
            Format::Pdf => "pdf",
            Format::Docx => "docx",
            Format::RevealJs => "revealjs",
            Format::Beamer => "beamer",
            Format::Markdown => "commonmark_x",
        }
    }
    pub fn mime(self) -> &'static str {
        match self {
            Format::Html | Format::RevealJs => "text/html; charset=utf-8",
            Format::Pdf | Format::Beamer => "application/pdf",
            Format::Docx => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            Format::Markdown => "text/markdown; charset=utf-8",
        }
    }
    pub fn extension(self) -> &'static str {
        match self {
            Format::Html | Format::RevealJs => "html",
            Format::Pdf | Format::Beamer => "pdf",
            Format::Docx => "docx",
            Format::Markdown => "md",
        }
    }
    /// Formats pandoc writes as bytes to a file rather than text on stdout.
    fn binary(self) -> bool {
        matches!(self, Format::Pdf | Format::Beamer | Format::Docx)
    }
}

/// The pandoc reader for SPEC §5: pandoc markdown plus wikilinks (`[[target|title]]`).
pub const READER: &str = "markdown+wikilinks_title_after_pipe+tex_math_dollars+fenced_divs+bracketed_spans";

#[derive(Debug, Clone)]
pub struct ExportOptions {
    /// `pandoc` binary; `None` → `pandoc` on `PATH`.
    pub pandoc: Option<PathBuf>,
    /// The vault folder: images are embedded from it (and nowhere else), and its
    /// `export/defaults.yaml` is honoured as far as [`render`] lets it.
    pub resource_dir: Option<PathBuf>,
    pub standalone: bool,
    /// Bibliography files to cite from (`--citeproc`), on disk; see [`citation_files`].
    pub bibliography: Vec<PathBuf>,
    /// Citation style, on disk.
    pub csl: Option<PathBuf>,
    /// An export that takes longer is killed.
    pub timeout: Duration,
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self {
            pandoc: None,
            resource_dir: None,
            standalone: true,
            bibliography: Vec::new(),
            csl: None,
            timeout: Duration::from_secs(120),
        }
    }
}

/// The vault's bibliography and citation style (SPEC §12), for notes that name none.
pub const VAULT_BIBLIOGRAPHY: &str = "export/references.bib";
pub const VAULT_CSL: &str = "export/style.csl";

/// What an export of one note cites from, as vault paths.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Citations {
    pub bibliography: Vec<String>,
    pub csl: Option<String>,
}

/// The citation files for exporting the note at `note_path` (SPEC §12): the note's own
/// `bibliography:` — one path or a list — and `csl:` from its front matter, relative to the
/// note as Quarto reads them (a leading `/` is the vault root); failing that, the vault's
/// `export/references.bib` and `export/style.csl`. Only files `exists` knows are returned, and
/// nothing outside the vault: this is the one way a note names a file for its export, and the
/// rest of what pandoc might read on a note's say-so [`render`] keeps it from reading.
pub fn citation_files(note_path: &str, markdown: &str, exists: impl Fn(&str) -> bool) -> Citations {
    use crate::projection::Projection;
    let meta: serde_yaml_ng::Value = crate::frontmatter::block(markdown)
        .and_then(|(range, _)| serde_yaml_ng::from_str(&markdown[range]).ok())
        .unwrap_or(serde_yaml_ng::Value::Null);
    let resolve = |v: &serde_yaml_ng::Value| {
        v.as_str().and_then(|p| Projection::normalize_relative(note_path, p.trim())).filter(|p| exists(p))
    };
    let own: Vec<String> = match meta.get("bibliography") {
        Some(serde_yaml_ng::Value::Sequence(items)) => items.iter().filter_map(resolve).collect(),
        Some(v) => resolve(v).into_iter().collect(),
        None => Vec::new(),
    };
    let csl = meta.get("csl").and_then(resolve);
    if !own.is_empty() {
        return Citations {
            bibliography: own,
            csl: csl.or_else(|| exists(VAULT_CSL).then(|| VAULT_CSL.to_owned())),
        };
    }
    if exists(VAULT_BIBLIOGRAPHY) {
        return Citations {
            bibliography: vec![VAULT_BIBLIOGRAPHY.to_owned()],
            csl: csl.or_else(|| exists(VAULT_CSL).then(|| VAULT_CSL.to_owned())),
        };
    }
    Citations::default()
}

pub fn pandoc_available(pandoc: Option<&Path>) -> bool {
    Command::new(pandoc.unwrap_or(Path::new("pandoc")))
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Render markdown to `format`; returns the bytes and their MIME type.
///
/// The note is somebody's text, and on a server or a relay that may not be the person whose
/// machine runs pandoc, so pandoc reads nothing on the note's say-so:
///
/// - It runs `--sandbox`: its readers and writers read only the files on its command line.
///   An image is read instead by [`GUARD`], a filter of ours, and only from `resource_dir`.
/// - The note cannot set metadata (`-yaml_metadata_block`): `csl:` and `bibliography:` are
///   read by citeproc, which the sandbox does not cover. Those come from [`citation_files`].
/// - The vault's `export/defaults.yaml` is cut down to [`DEFAULTS_ALLOWED`] first.
/// - LaTeX, for a PDF, may open no file outside its own scratch directory and run nothing.
/// - It runs in a scratch directory of its own, removed afterwards, for `opts.timeout` at most.
pub fn render(markdown: &str, format: Format, opts: &ExportOptions) -> Result<(Vec<u8>, &'static str)> {
    let input = match crate::frontmatter::block(markdown) {
        Some((_, end)) => &markdown[end..],
        None => markdown,
    };
    let tmp = tempdir()?;
    std::fs::write(tmp.join("guard.lua"), GUARD)?;
    let bin = opts.pandoc.clone().unwrap_or_else(|| PathBuf::from("pandoc"));
    let mut cmd = Command::new(&bin);
    cmd.current_dir(&*tmp);
    if let Some(dir) = &opts.resource_dir {
        let defaults = dir.join("export").join("defaults.yaml");
        if defaults.is_file() {
            let safe = sanitize_defaults(&std::fs::read_to_string(&defaults)?, &dir.join("export"))?;
            std::fs::write(tmp.join("defaults.yaml"), safe)?;
            // First, so that everything after it on the command line wins.
            cmd.arg("--defaults").arg(tmp.join("defaults.yaml"));
        }
        cmd.env("LEMMATE_EXPORT_ROOT", dir);
    } else {
        cmd.env_remove("LEMMATE_EXPORT_ROOT");
    }
    cmd.arg("--sandbox");
    cmd.arg("-f").arg(format!("{READER}-yaml_metadata_block")).arg("-t").arg(format.pandoc_name());
    if opts.standalone {
        cmd.arg("--standalone");
    }
    if matches!(format, Format::Html | Format::RevealJs) {
        cmd.arg("--mathjax");
    }
    // Before `--citeproc`: filters run in command-line order.
    cmd.arg("--lua-filter").arg(tmp.join("guard.lua"));
    if !opts.bibliography.is_empty() {
        cmd.arg("--citeproc");
        for b in &opts.bibliography {
            cmd.arg("--bibliography").arg(b);
        }
        if let Some(csl) = &opts.csl {
            cmd.arg("--csl").arg(csl);
        }
    }
    if matches!(format, Format::Pdf | Format::Beamer) {
        // kpathsea reads these from the environment ahead of texmf.cnf: TeX may open files
        // only below its working directory (pandoc's own scratch directory, TEXMFOUTPUT) —
        // so raw `\input{/etc/passwd}` in a note reads nothing — and run no shell commands.
        cmd.env("openin_any", "p").env("openout_any", "p").env("shell_escape", "f");
    }
    let out_path = tmp.join(format!("out.{}", format.extension()));
    if format.binary() {
        cmd.arg("-o").arg(&out_path);
    }
    cmd.stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
    let output = run_bounded(&mut cmd, Some(input.as_bytes()), opts.timeout)
        .map_err(|e| Error::Export(format!("running {}: {e}", bin.display())))?
        .ok_or_else(|| Error::Export(format!("pandoc did not finish within {}s", opts.timeout.as_secs())))?;
    if !output.status.success() {
        return Err(Error::Export(format!(
            "pandoc failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    let bytes = if format.binary() { std::fs::read(&out_path)? } else { output.stdout };
    Ok((bytes, format.mime()))
}

/// The filter every export runs through (see [`render`]). pandoc's sandbox keeps its writers
/// from reading an image, so this one reads them, for the formats that embed them: only a
/// relative path that stays inside the vault folder (`$LEMMATE_EXPORT_ROOT`) and passes no
/// hidden folder (`.lemmate/` is the relay's own) is read and put in the media bag; any other
/// local image becomes its description. `data:` and web images are left to pandoc, and the
/// sandbox fetches nothing from the web either.
const GUARD: &str = r#"
local root = os.getenv('LEMMATE_EXPORT_ROOT')
local embeds = FORMAT == 'docx' or FORMAT == 'latex' or FORMAT == 'beamer'
local count = 0

local function vault_path(src)
  if src:find('^%a[%w+.-]*:') then return nil end
  local path = src:gsub('[?#].*$', '')
  path = path:gsub('%%(%x%x)', function(h) return string.char(tonumber(h, 16)) end)
  if path == '' or path:find('^/') or path:find('\\', 1, true) or path:find('%z') then return nil end
  local parts = {}
  for seg in path:gmatch('[^/]+') do
    if seg ~= '.' then
      if seg:sub(1, 1) == '.' then return nil end
      parts[#parts + 1] = seg
    end
  end
  if #parts == 0 then return nil end
  return table.concat(parts, '/')
end

function Image(img)
  if not embeds or img.src:find('^data:') or img.src:find('^[Hh][Tt][Tt][Pp][Ss]?://') then
    return nil
  end
  local path = vault_path(img.src)
  local file = path and root and io.open(root .. '/' .. path, 'rb')
  if not file then return img.caption end
  local bytes = file:read('a')
  file:close()
  if not bytes then return img.caption end
  count = count + 1
  local ext = path:match('%.(%w+)$')
  local name = 'lemmate-' .. count .. (ext and ('.' .. ext) or '')
  pandoc.mediabag.insert(name, nil, bytes)
  img.src = name
  return img
end
"#;

/// What a vault's `export/defaults.yaml` may set. It is a vault file — any collaborator can
/// write it, and the relay hands it to pandoc on the machine of whoever exports — so it gets
/// a list of harmless options rather than pandoc's whole command line: no `filters` (programs),
/// no `pdf-engine-opts`, no `data-dir`, `output-file`, `extract-media`, `resource-path`,
/// `include-*` or nested `defaults` (files from anywhere, or written anywhere).
const DEFAULTS_ALLOWED: &[&str] = &[
    "standalone",
    "table-of-contents",
    "toc",
    "toc-depth",
    "number-sections",
    "number-offset",
    "top-level-division",
    "shift-heading-level-by",
    "section-divs",
    "variables",
    "metadata",
    "dpi",
    "wrap",
    "columns",
    "tab-stop",
    "preserve-tabs",
    "indented-code-classes",
    "ascii",
    "reference-links",
    "reference-location",
    "markdown-headings",
    "list-tables",
    "listings",
    "incremental",
    "slide-level",
    "email-obfuscation",
    "identifier-prefix",
    "title-prefix",
    "strip-comments",
    "html-q-tags",
    "html-math-method",
    "cite-method",
    "track-changes",
    "eol",
    "figure-caption-position",
    "table-caption-position",
    "link-images",
];

/// Options naming a file, honoured for a file in the vault's `export/` folder only.
const DEFAULTS_FILES: &[&str] = &["template", "reference-doc"];

/// The PDF engines the restrictions in [`render`] hold for (kpathsea's).
const PDF_ENGINES: &[&str] = &["pdflatex", "xelatex", "lualatex"];

/// Highlighting styles pandoc has built in; a path to a `.theme` file is a file read.
const HIGHLIGHT_STYLES: &[&str] = &[
    "pygments",
    "tango",
    "espresso",
    "zenburn",
    "kate",
    "monochrome",
    "breezedark",
    "haddock",
    "none",
    "default",
    "idiomatic",
];

/// Metadata citeproc reads a file for; an export's come from [`citation_files`].
const METADATA_FILES: &[&str] = &["bibliography", "csl", "citation-abbreviations"];

/// `defaults.yaml` cut down to what [`DEFAULTS_ALLOWED`] lets through, with the files it names
/// as absolute paths into `export_dir`. What is left out is logged, not an error: an export
/// still works, just without the option.
fn sanitize_defaults(yaml: &str, export_dir: &Path) -> Result<String> {
    use serde_yaml_ng::{Mapping, Value};
    let given = match serde_yaml_ng::from_str::<Value>(yaml) {
        Ok(Value::Mapping(m)) => m,
        Ok(_) => Mapping::new(),
        Err(e) => return Err(Error::Export(format!("export/defaults.yaml is not valid YAML: {e}"))),
    };
    let mut kept = Mapping::new();
    for (key, value) in given {
        let Some(name) = key.as_str() else { continue };
        let value = match (name, value) {
            ("metadata", Value::Mapping(mut meta)) => {
                meta.retain(|k, _| !k.as_str().is_some_and(|k| METADATA_FILES.contains(&k)));
                Some(Value::Mapping(meta))
            }
            (n, v) if DEFAULTS_ALLOWED.contains(&n) => Some(v),
            (n, Value::String(f)) if DEFAULTS_FILES.contains(&n) => {
                export_file(export_dir, &f).map(|p| Value::String(p.to_string_lossy().into_owned()))
            }
            ("pdf-engine", Value::String(e)) if PDF_ENGINES.contains(&e.as_str()) => Some(Value::String(e)),
            ("highlight-style" | "syntax-highlighting", Value::String(s))
                if HIGHLIGHT_STYLES.contains(&s.as_str()) =>
            {
                Some(Value::String(s))
            }
            _ => None,
        };
        match value {
            Some(v) => {
                kept.insert(key, v);
            }
            None => tracing::warn!(option = name, "export/defaults.yaml: option ignored"),
        }
    }
    serde_yaml_ng::to_string(&Value::Mapping(kept)).map_err(|e| Error::Export(e.to_string()))
}

/// `name` — as `defaults.yaml` writes it, plain or `${.}/`-relative — as a file inside
/// `export_dir`, or `None` if it is anywhere else (or missing).
fn export_file(export_dir: &Path, name: &str) -> Option<PathBuf> {
    let name = name.strip_prefix("${.}/").unwrap_or(name);
    let mut rel = PathBuf::new();
    for seg in name.split('/') {
        match seg {
            "" | "." => {}
            s if s.starts_with('.') || s.contains(['\\', ':']) => return None,
            s => rel.push(s),
        }
    }
    if name.starts_with('/') || rel.as_os_str().is_empty() {
        return None;
    }
    let file = export_dir.join(rel).canonicalize().ok()?;
    (file.starts_with(export_dir.canonicalize().ok()?) && file.is_file()).then_some(file)
}

/// A scratch directory only its owner can enter, removed when dropped — on every way out.
pub(crate) struct TempDir(PathBuf);

impl std::ops::Deref for TempDir {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}

impl AsRef<Path> for TempDir {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub(crate) fn tempdir() -> Result<TempDir> {
    let dir = std::env::temp_dir().join(format!("lemmate-export-{}", crate::ids::NoteId::new()));
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(&dir)?;
    Ok(TempDir(dir))
}

/// Run `cmd` with `input` on its stdin (or none), collecting whatever of stdout and stderr the
/// caller piped. `Ok(None)` when it ran past `timeout`: it is killed then, together with
/// everything it started — on unix it leads a process group of its own, so pandoc under
/// Quarto, or LaTeX under pandoc, goes too. (On Windows only the process itself is killed.)
pub(crate) fn run_bounded(
    cmd: &mut Command,
    input: Option<&[u8]>,
    timeout: Duration,
) -> std::io::Result<Option<std::process::Output>> {
    use std::io::{Read, Write};
    use std::process::Stdio;
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(cmd, 0);
    cmd.stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() });
    let mut child = cmd.spawn()?;
    let drain = |pipe: Option<Box<dyn Read + Send>>| {
        pipe.map(|mut p| {
            std::thread::spawn(move || {
                let mut buf = Vec::new();
                let _ = p.read_to_end(&mut buf);
                buf
            })
        })
    };
    let stdout = drain(child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let stderr = drain(child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    if let (Some(mut stdin), Some(input)) = (child.stdin.take(), input) {
        let input = input.to_vec();
        // Its own thread: a child that writes a lot before reading all of stdin must not
        // block on us while we block on it.
        std::thread::spawn(move || {
            let _ = stdin.write_all(&input);
        });
    }
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if started.elapsed() > timeout {
            #[cfg(unix)]
            if let Ok(pid) = libc::pid_t::try_from(child.id()) {
                // SAFETY: kill(2) on the process group this child leads; no memory involved.
                unsafe { libc::kill(-pid, libc::SIGKILL) };
            }
            let _ = child.kill();
            let _ = child.wait();
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let collect =
        |t: Option<std::thread::JoinHandle<Vec<u8>>>| t.and_then(|t| t.join().ok()).unwrap_or_default();
    Ok(Some(std::process::Output { status, stdout: collect(stdout), stderr: collect(stderr) }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pandoc() -> Option<PathBuf> {
        std::env::var_os("LEMMATE_TEST_PANDOC").map(PathBuf::from).filter(|p| p.is_file())
    }

    #[test]
    fn formats_parse() {
        assert_eq!(Format::parse("HTML"), Some(Format::Html));
        assert_eq!(Format::parse("slides"), Some(Format::RevealJs));
        assert_eq!(Format::parse("nope"), None);
    }

    #[test]
    fn missing_pandoc_is_a_clear_error() {
        let opts = ExportOptions { pandoc: Some(PathBuf::from("/nonexistent/pandoc")), ..Default::default() };
        let err = render("# x", Format::Html, &opts).unwrap_err().to_string();
        assert!(err.contains("running /nonexistent/pandoc"), "{err}");
        assert!(!pandoc_available(Some(Path::new("/nonexistent/pandoc"))));
    }

    /// Runs only when LEMMATE_TEST_PANDOC points at a pandoc binary.
    #[test]
    fn renders_html_and_docx_with_wikilinks_and_math() {
        let Some(bin) = pandoc() else {
            eprintln!("skipped: set LEMMATE_TEST_PANDOC");
            return;
        };
        let opts = ExportOptions { pandoc: Some(bin), ..Default::default() };
        let md = "---\nid: 01X\n---\n# Title\n\nSee [[Other Note|the other]] and $E=mc^2$.\n\n::: {.callout-note}\nhi\n:::\n";
        let (html, mime) = render(md, Format::Html, &opts).unwrap();
        let html = String::from_utf8(html).unwrap();
        assert!(mime.starts_with("text/html"));
        assert!(html.contains("<h1"), "{html}");
        assert!(html.contains("the other") && html.contains("Other Note"), "wikilink rendered: {html}");
        assert!(!html.contains("01X"), "front matter stripped");
        let (docx, mime) = render(md, Format::Docx, &opts).unwrap();
        assert!(mime.contains("wordprocessingml") && docx.starts_with(b"PK"));
    }

    #[test]
    fn a_note_names_its_own_bibliography_or_gets_the_vaults() {
        let files =
            ["export/references.bib", "export/style.csl", "Papers/refs.bib", "Papers/more.bib", "apa.csl"];
        let exists = |p: &str| files.contains(&p);
        let vault = Citations {
            bibliography: vec!["export/references.bib".into()],
            csl: Some("export/style.csl".into()),
        };
        assert_eq!(citation_files("Papers/a.md", "# no front matter\n", exists), vault);
        assert_eq!(
            citation_files("Papers/a.md", "---\nbibliography: refs.bib\n---\n", exists).bibliography,
            ["Papers/refs.bib"]
        );
        let both = citation_files(
            "Papers/a.md",
            "---\nbibliography: [refs.bib, more.bib]\ncsl: /apa.csl\n---\n",
            exists,
        );
        assert_eq!(both.bibliography, ["Papers/refs.bib", "Papers/more.bib"]);
        assert_eq!(both.csl.as_deref(), Some("apa.csl"));
        // Missing files, and paths out of the vault, fall back to the vault's.
        assert_eq!(citation_files("Papers/a.md", "---\nbibliography: ../../etc/x.bib\n---\n", exists), vault);
        assert_eq!(citation_files("Papers/a.md", "---\nbibliography: gone.bib\n---\n", exists), vault);
        assert_eq!(citation_files("a.md", "", |_| false), Citations::default());
    }

    #[test]
    fn citations_render_from_the_given_bibliography() {
        let Some(bin) = pandoc() else {
            eprintln!("skipped: set LEMMATE_TEST_PANDOC");
            return;
        };
        let dir = tempdir().unwrap();
        let bib = dir.join("refs.bib");
        std::fs::write(&bib, "@book{knuth84, author={Donald Knuth}, title={The TeXbook}, year={1984}}\n")
            .unwrap();
        let opts = ExportOptions { pandoc: Some(bin), bibliography: vec![bib], ..Default::default() };
        let (html, _) =
            render("---\nbibliography: refs.bib\n---\nAs shown [@knuth84].\n", Format::Html, &opts).unwrap();
        let html = String::from_utf8(html).unwrap();
        drop(dir);
        assert!(html.contains("Knuth") && html.contains("1984"), "{html}");
        assert!(html.contains("TeXbook"), "a reference list: {html}");
    }

    /// A 1×1 PNG, and the same with a byte appended, to tell two images apart.
    const PNG: &[u8] = &[
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 13, 0x49, 0x48, 0x44, 0x52, 0, 0, 0, 1, 0,
        0, 0, 1, 8, 6, 0, 0, 0, 0x1f, 0x15, 0xc4, 0x89, 0, 0, 0, 13, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c,
        0x63, 0xf8, 0xcf, 0xc0, 0xf0, 0x1f, 0, 0x05, 0, 0x01, 0xff, 0x89, 0x99, 0x3d, 0x1d, 0, 0, 0, 0, 0x49,
        0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ];

    /// The files under `word/media/` of a DOCX.
    fn docx_media(docx: &[u8]) -> Vec<Vec<u8>> {
        use std::io::Read;
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(docx)).unwrap();
        let mut out = Vec::new();
        for i in 0..zip.len() {
            let mut f = zip.by_index(i).unwrap();
            if f.name().starts_with("word/media/") {
                let mut bytes = Vec::new();
                f.read_to_end(&mut bytes).unwrap();
                out.push(bytes);
            }
        }
        out
    }

    /// Runs only when LEMMATE_TEST_PANDOC points at a pandoc binary. Every way a note can name
    /// a file outside the vault folder — absolute, climbing out, a `file:` URL, the relay's
    /// own hidden folder — embeds nothing; the vault's own image still does.
    #[test]
    fn an_export_embeds_vault_images_and_nothing_else() {
        let Some(bin) = pandoc() else {
            eprintln!("skipped: set LEMMATE_TEST_PANDOC");
            return;
        };
        let base = tempdir().unwrap();
        let vault = base.join("vault");
        std::fs::create_dir_all(vault.join(".lemmate")).unwrap();
        std::fs::create_dir_all(vault.join("img")).unwrap();
        std::fs::write(vault.join("img/pic one.png"), PNG).unwrap();
        let other = [PNG, b"!"].concat();
        std::fs::write(vault.join(".lemmate/secret.png"), &other).unwrap();
        std::fs::write(base.join("outside.png"), &other).unwrap();
        let md = format!(
            "![ok](<img/pic one.png>) ![host](/etc/hostname) ![up](../outside.png) \
             ![abs]({}) ![url](file:///etc/hostname) ![hidden](.lemmate/secret.png) \
             ![enc](img/%2e%2e/%2e%2e/outside.png) ![[/etc/hostname]]\n",
            base.join("outside.png").display()
        );
        let opts = ExportOptions { pandoc: Some(bin), resource_dir: Some(vault), ..Default::default() };
        let (docx, _) = render(&md, Format::Docx, &opts).unwrap();
        assert_eq!(docx_media(&docx), [PNG.to_vec()], "only the vault's image");
        // Without a vault folder (the server's export) nothing local is embedded at all.
        let opts = ExportOptions { resource_dir: None, ..opts };
        let (docx, _) = render(&md, Format::Docx, &opts).unwrap();
        assert!(docx_media(&docx).is_empty());
        let (docx, _) = render("![x](/etc/hostname)\n", Format::Docx, &opts).unwrap();
        assert!(docx_media(&docx).is_empty(), "the server's own files stay put");
    }

    /// Runs only when LEMMATE_TEST_PANDOC points at a pandoc binary. A Lua filter named in the
    /// vault's `defaults.yaml` does not run; the harmless options there still apply.
    #[test]
    fn a_vault_defaults_file_cannot_run_a_filter() {
        let Some(bin) = pandoc() else {
            eprintln!("skipped: set LEMMATE_TEST_PANDOC");
            return;
        };
        let vault = tempdir().unwrap();
        std::fs::create_dir_all(vault.join("export")).unwrap();
        let marker = vault.join("ran");
        std::fs::write(
            vault.join("export/x.lua"),
            format!("io.open({:?}, 'w'):write('x')\n", marker.display().to_string()),
        )
        .unwrap();
        std::fs::write(
            vault.join("export/defaults.yaml"),
            "filters: ['${.}/x.lua']\nlua-filters: ['${.}/x.lua']\ntoc: true\n",
        )
        .unwrap();
        let opts = ExportOptions {
            pandoc: Some(bin),
            resource_dir: Some(vault.to_path_buf()),
            ..Default::default()
        };
        let (html, _) = render("# One\n\n# Two\n", Format::Html, &opts).unwrap();
        assert!(!marker.exists(), "the filter did not run");
        assert!(String::from_utf8(html).unwrap().contains("id=\"TOC\""), "toc: true still applies");
    }

    /// Runs only when LEMMATE_TEST_PANDOC points at a pandoc binary. A metadata block further
    /// down the note is not metadata, so it cannot point citeproc at a file.
    #[test]
    fn a_note_cannot_name_files_through_metadata() {
        let Some(bin) = pandoc() else {
            eprintln!("skipped: set LEMMATE_TEST_PANDOC");
            return;
        };
        let dir = tempdir().unwrap();
        let bib = dir.join("refs.bib");
        std::fs::write(&bib, "@book{k, author={Don Knuth}, title={TeXbook}, year={1984}}\n").unwrap();
        let opts = ExportOptions { pandoc: Some(bin), bibliography: vec![bib], ..Default::default() };
        let md = "As [@k].\n\n---\ncsl: /etc/hostname\ncitation-abbreviations: /etc/hostname\n---\n";
        let (html, _) = render(md, Format::Html, &opts).unwrap();
        assert!(String::from_utf8(html).unwrap().contains("Knuth"));
    }

    #[test]
    fn defaults_are_cut_down_to_harmless_options() {
        let vault = tempdir().unwrap();
        let export = vault.join("export");
        std::fs::create_dir_all(export.join("sub")).unwrap();
        std::fs::write(export.join("template.tex"), "$body$").unwrap();
        std::fs::write(export.join("sub/ref.docx"), "PK").unwrap();
        std::fs::write(vault.join("outside.tex"), "$body$").unwrap();
        let yaml = "filters: [x.lua]\ninclude-in-header: /etc/passwd\ndata-dir: /\nresource-path: [/]\n\
                    output-file: /tmp/x\ndefaults: other.yaml\npdf-engine-opts: ['-shell-escape']\n\
                    toc: true\nvariables: {geometry: margin=1in}\n\
                    metadata: {title: T, csl: /etc/passwd, bibliography: /etc/passwd}\n\
                    template: ${.}/template.tex\nreference-doc: sub/ref.docx\n\
                    highlight-style: /etc/x.theme\n";
        let v: serde_yaml_ng::Value =
            serde_yaml_ng::from_str(&sanitize_defaults(yaml, &export).unwrap()).unwrap();
        let keys: Vec<&str> = v.as_mapping().unwrap().keys().filter_map(|k| k.as_str()).collect();
        assert_eq!(keys, ["toc", "variables", "metadata", "template", "reference-doc"]);
        assert_eq!(v["metadata"].as_mapping().unwrap().len(), 1, "only the title: {v:?}");
        let canon = export.canonicalize().unwrap();
        assert_eq!(v["template"].as_str(), Some(canon.join("template.tex").to_str().unwrap()));
        assert_eq!(v["reference-doc"].as_str(), Some(canon.join("sub/ref.docx").to_str().unwrap()));
        for outside in ["../outside.tex", "/etc/passwd", "${.}/../outside.tex", "missing.tex", ".hidden"] {
            let v = sanitize_defaults(&format!("template: '{outside}'\n"), &export).unwrap();
            assert!(!v.contains("template"), "{outside}: {v}");
        }
        let v = sanitize_defaults("pdf-engine: tectonic\nsyntax-highlighting: kate\n", &export).unwrap();
        assert!(!v.contains("tectonic") && v.contains("kate"), "{v}");
        let v = sanitize_defaults("pdf-engine: xelatex\n", &export).unwrap();
        assert!(v.contains("xelatex"));
        assert!(sanitize_defaults("[", &export).unwrap_err().to_string().contains("not valid YAML"));
    }

    /// A stand-in for pandoc: a shell script that records how it was run.
    #[cfg(unix)]
    fn fake_pandoc(dir: &Path, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let bin = dir.join("pandoc");
        std::fs::write(&bin, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        bin
    }

    #[cfg(unix)]
    #[test]
    fn a_pdf_is_made_sandboxed_in_a_private_scratch_directory() {
        let dir = tempdir().unwrap();
        let log = dir.join("log");
        let bin = fake_pandoc(
            &dir,
            &format!(
                "log={log:?}\npwd > \"$log.cwd\"; stat -c %a . > \"$log.mode\"; env > \"$log.env\"; echo \"$@\" > \"$log.args\"\n\
                 while [ $# -gt 0 ]; do [ \"$1\" = -o ] && echo pdf > \"$2\"; shift; done",
                log = log.display().to_string()
            ),
        );
        let opts = ExportOptions { pandoc: Some(bin), ..Default::default() };
        let (bytes, _) = render("\\input{/etc/passwd}\n", Format::Pdf, &opts).unwrap();
        assert_eq!(bytes, b"pdf\n");
        let read = |ext: &str| std::fs::read_to_string(format!("{}.{ext}", log.display())).unwrap();
        let env = read("env");
        assert!(env.contains("openin_any=p\n") && env.contains("openout_any=p\n"), "{env}");
        assert!(env.contains("shell_escape=f\n"), "{env}");
        let args = read("args");
        assert!(args.contains("--sandbox") && args.contains("-yaml_metadata_block"), "{args}");
        assert_eq!(read("mode").trim(), "700");
        assert!(!Path::new(read("cwd").trim()).exists(), "the scratch directory is gone");
    }

    #[cfg(unix)]
    #[test]
    fn a_stuck_export_is_killed_with_what_it_started() {
        let dir = tempdir().unwrap();
        let pid = dir.join("pid");
        let cwd = dir.join("cwd");
        let bin = fake_pandoc(
            &dir,
            &format!(
                "pwd > {cwd:?}; sleep 60 & echo $! > {pid:?}; wait",
                cwd = cwd.display().to_string(),
                pid = pid.display().to_string()
            ),
        );
        let opts =
            ExportOptions { pandoc: Some(bin), timeout: Duration::from_millis(500), ..Default::default() };
        let started = Instant::now();
        let err = render("x", Format::Docx, &opts).unwrap_err().to_string();
        assert!(err.contains("did not finish"), "{err}");
        assert!(started.elapsed() < Duration::from_secs(10));
        let pid: i32 = std::fs::read_to_string(&pid).unwrap().trim().parse().unwrap();
        // The grandchild was killed too (it may linger a moment as a zombie of the reaper).
        let gone = (0..50).any(|_| {
            let state = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
            if state.is_empty() || state.split(' ').nth(2) == Some("Z") {
                return true;
            }
            std::thread::sleep(Duration::from_millis(100));
            false
        });
        assert!(gone, "the child's own child was killed");
        assert!(
            !Path::new(std::fs::read_to_string(&cwd).unwrap().trim()).exists(),
            "scratch removed on timeout"
        );
    }
}

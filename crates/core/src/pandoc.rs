//! Export through pandoc (SPEC §12). The app's own parsers never render exports; real pandoc
//! does, with the pandoc extensions that match the SPEC §5 dialect (wikilinks included).

use std::path::{Path, PathBuf};
use std::process::Command;

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
    /// Vault directory, used to resolve images/attachments and `export/defaults.yaml`.
    pub resource_dir: Option<PathBuf>,
    /// Drop the front matter block before rendering (ids are not for readers).
    pub strip_front_matter: bool,
    pub standalone: bool,
    /// Bibliography files to cite from (`--citeproc`), on disk; see [`citation_files`].
    pub bibliography: Vec<PathBuf>,
    /// Citation style, on disk.
    pub csl: Option<PathBuf>,
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self {
            pandoc: None,
            resource_dir: None,
            strip_front_matter: true,
            standalone: true,
            bibliography: Vec::new(),
            csl: None,
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
/// nothing outside the vault, so an export cannot be pointed at other files on the host.
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
pub fn render(markdown: &str, format: Format, opts: &ExportOptions) -> Result<(Vec<u8>, &'static str)> {
    let input = if opts.strip_front_matter {
        match crate::frontmatter::block(markdown) {
            Some((_, end)) => &markdown[end..],
            None => markdown,
        }
    } else {
        markdown
    };
    let bin = opts.pandoc.clone().unwrap_or_else(|| PathBuf::from("pandoc"));
    let mut cmd = Command::new(&bin);
    cmd.arg("-f").arg(READER).arg("-t").arg(format.pandoc_name());
    if opts.standalone {
        cmd.arg("--standalone");
    }
    if matches!(format, Format::Html | Format::RevealJs) {
        cmd.arg("--mathjax");
    }
    let tmp = tempdir()?;
    if let Some(dir) = &opts.resource_dir {
        cmd.arg("--resource-path").arg(dir);
        let export = dir.join("export");
        if export.join("defaults.yaml").is_file() {
            cmd.arg("--defaults").arg(export.join("defaults.yaml"));
        }
    }
    if !opts.bibliography.is_empty() {
        cmd.arg("--citeproc");
        for b in &opts.bibliography {
            cmd.arg("--bibliography").arg(b);
        }
        if let Some(csl) = &opts.csl {
            cmd.arg("--csl").arg(csl);
        }
    }
    let out_path = tmp.join(format!("out.{}", format.extension()));
    if format.binary() {
        cmd.arg("-o").arg(&out_path);
    }
    cmd.stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| Error::Export(format!("running {}: {e}", bin.display())))?;
    {
        use std::io::Write;
        let mut stdin = child.stdin.take().expect("piped");
        stdin.write_all(input.as_bytes()).map_err(|e| Error::Export(e.to_string()))?;
    }
    let output = child.wait_with_output().map_err(|e| Error::Export(e.to_string()))?;
    if !output.status.success() {
        return Err(Error::Export(format!(
            "pandoc failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    let bytes = if format.binary() { std::fs::read(&out_path)? } else { output.stdout };
    let _ = std::fs::remove_dir_all(&tmp);
    Ok((bytes, format.mime()))
}

pub(crate) fn tempdir() -> Result<PathBuf> {
    let dir = std::env::temp_dir().join(format!("notes-export-{}", crate::ids::NoteId::new()));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
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
        let _ = std::fs::remove_dir_all(&dir);
        assert!(html.contains("Knuth") && html.contains("1984"), "{html}");
        assert!(html.contains("TeXbook"), "a reference list: {html}");
    }
}

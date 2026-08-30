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
    /// Vault directory, used to resolve images/attachments and the optional `export/` folder
    /// (`defaults.yaml`, `references.bib`, `style.csl`, `template.*`).
    pub resource_dir: Option<PathBuf>,
    /// Drop the front matter block before rendering (ids are not for readers).
    pub strip_front_matter: bool,
    pub standalone: bool,
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self { pandoc: None, resource_dir: None, strip_front_matter: true, standalone: true }
    }
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
        if export.join("references.bib").is_file() {
            cmd.arg("--citeproc").arg("--bibliography").arg(export.join("references.bib"));
            if export.join("style.csl").is_file() {
                cmd.arg("--csl").arg(export.join("style.csl"));
            }
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

fn tempdir() -> Result<PathBuf> {
    let dir = std::env::temp_dir().join(format!("notes-export-{}", crate::ids::NoteId::new()));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pandoc() -> Option<PathBuf> {
        std::env::var_os("NOTES_TEST_PANDOC").map(PathBuf::from).filter(|p| p.is_file())
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

    /// Runs only when NOTES_TEST_PANDOC points at a pandoc binary.
    #[test]
    fn renders_html_and_docx_with_wikilinks_and_math() {
        let Some(bin) = pandoc() else {
            eprintln!("skipped: set NOTES_TEST_PANDOC");
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
}

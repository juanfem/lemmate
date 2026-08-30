//! Vault export (SPEC §12).
//!
//! The one export that never needs pandoc or quarto: a zip of the vault's markdown and
//! attachments. Entries are stored under vault-relative paths with forward slashes in a
//! deterministic (sorted) order, so exporting an unchanged vault twice yields the same archive.
//! The sidecar (`.lemmate/`) and hidden entries are left out — they are local state, not content.

use std::fmt;
use std::fs;
use std::io;
use std::path::Path;

use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

use crate::error::{Error, Result};
use crate::projection::Projection;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExportReport {
    /// Files written into the archive.
    pub files: usize,
    /// Total uncompressed bytes.
    pub bytes: u64,
}

impl fmt::Display for ExportReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "exported {} file{} ({} bytes uncompressed)",
            self.files,
            if self.files == 1 { "" } else { "s" },
            self.bytes
        )
    }
}

/// Zip every note and attachment under `vault` into `out`.
pub fn export_zip(vault: &Path, out: &Path) -> Result<ExportReport> {
    if !vault.is_dir() {
        return Err(Error::Export(format!("{} is not a directory", vault.display())));
    }
    let proj = Projection::new(vault);
    // Walk before creating the archive, so an `out` path inside the vault cannot include itself.
    let mut paths = proj.walk_notes()?;
    paths.extend(proj.walk_files()?);
    paths.sort();
    paths.dedup();

    if let Some(parent) = out.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    let file = fs::File::create(out)?;
    let mut zip = ZipWriter::new(io::BufWriter::new(file));
    // `SimpleFileOptions::default()` has a fixed timestamp here (the `time` feature is off), so
    // the archive depends only on its contents.
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);

    let mut report = ExportReport::default();
    for rel in &paths {
        let abs = proj.resolve(rel)?;
        let mut src = fs::File::open(&abs)?;
        zip.start_file(rel, options).map_err(zip_err)?;
        report.bytes += io::copy(&mut src, &mut zip)?;
        report.files += 1;
    }
    zip.finish().map_err(zip_err)?;
    Ok(report)
}

fn zip_err(e: zip::result::ZipError) -> Error {
    match e {
        zip::result::ZipError::Io(e) => Error::Io(e),
        other => Error::Export(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    #[test]
    fn zips_notes_and_attachments_skipping_sidecar_and_hidden() {
        let tmp = tempfile::tempdir().unwrap();
        let vault = tmp.path().join("vault");
        write(&vault.join("Root.md"), "# Root\n");
        write(&vault.join("a/Nested.qmd"), "nested\n");
        write(&vault.join("a/img.png"), "PNG-BYTES");
        write(&vault.join(".lemmate/local.db"), "sqlite");
        write(&vault.join(".hidden/secret.md"), "no");
        write(&vault.join(".dotfile"), "no");

        let out = tmp.path().join("dist/vault.zip");
        let report = export_zip(&vault, &out).unwrap();
        assert_eq!(report, ExportReport { files: 3, bytes: 7 + 7 + 9 });

        let mut archive = zip::ZipArchive::new(fs::File::open(&out).unwrap()).unwrap();
        let names: Vec<String> = archive.file_names().map(str::to_owned).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, vec!["Root.md", "a/Nested.qmd", "a/img.png"]);
        assert_eq!(names, sorted, "entries are stored in sorted order");

        let mut body = String::new();
        archive.by_name("a/Nested.qmd").unwrap().read_to_string(&mut body).unwrap();
        assert_eq!(body, "nested\n");
    }

    #[test]
    fn export_is_deterministic() {
        let tmp = tempfile::tempdir().unwrap();
        let vault = tmp.path().join("vault");
        write(&vault.join("One.md"), "one\n");
        write(&vault.join("two/Two.md"), "two\n");
        let a = tmp.path().join("a.zip");
        let b = tmp.path().join("b.zip");
        export_zip(&vault, &a).unwrap();
        export_zip(&vault, &b).unwrap();
        assert_eq!(fs::read(&a).unwrap(), fs::read(&b).unwrap());
    }

    #[test]
    fn missing_vault_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let err = export_zip(&tmp.path().join("nope"), &tmp.path().join("o.zip")).unwrap_err();
        assert!(matches!(err, Error::Export(_)));
    }
}

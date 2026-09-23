//! Content-addressed attachment storage (SPEC §4.4, §6.1, §7). Files are identified by the
//! blake3 hash of their bytes; filenames are hints kept alongside for projection.

use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::ids::VaultId;

/// Upper bound accepted by the server and requested by clients.
pub const MAX_ATTACHMENT_BYTES: u64 = 64 * 1024 * 1024;

pub fn hash_bytes(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

pub fn is_valid_hash(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

pub fn mime_for_path(path: &str) -> String {
    mime_guess::from_path(path).first_or_octet_stream().essence_str().to_owned()
}

/// Map a link target to an attachment path: relative to the note, relative to the root, under
/// `attachments/`, and (for `![[wikilinks]]`) by basename among `candidates()`. `exists` says
/// whether a vault-relative path is a known attachment (a file on disk for clients, a vault-doc
/// entry on the server).
pub fn resolve_reference(
    note_rel: &str,
    target: &str,
    wiki: bool,
    exists: impl Fn(&str) -> bool,
    candidates: impl FnOnce() -> Vec<String>,
) -> Option<String> {
    use crate::projection::Projection;
    let t = target.split(['#', '?']).next().unwrap_or("").trim().replace("%20", " ");
    if t.is_empty() || t.contains("://") || t.starts_with("mailto:") || t.starts_with("data:") {
        return None;
    }
    let name = t.rsplit('/').next().unwrap_or(&t).to_owned();
    let mut tries = Vec::new();
    tries.extend(Projection::normalize_relative(note_rel, &t));
    tries.extend(Projection::normalize_relative("", &t));
    tries.push(format!("attachments/{name}"));
    if let Some(hit) = tries.iter().find(|c| exists(c)) {
        return Some(hit.clone());
    }
    if wiki {
        return candidates().into_iter().find(|f| f.rsplit('/').next() == Some(name.as_str()));
    }
    None
}

/// The attachments a note depends on (see [`dependencies`]), given the vault's attachment paths
/// as a list — what the server and a render have — and a way to read one.
pub fn referenced(
    note_path: &str,
    text: &str,
    attachments: &[String],
    read: impl Fn(&str) -> Option<Vec<u8>>,
) -> Result<Vec<String>> {
    let ix = crate::markdown::index(text)?;
    Ok(dependencies(
        note_path,
        text,
        &ix,
        |c| attachments.iter().any(|e| e == c),
        || attachments.to_vec(),
        read,
    ))
}

/// How many files one note (or the vault's resources) may pull in, all hops together: a
/// stylesheet that imports itself in a circle, or a front matter naming half the vault, stops
/// here rather than walking on.
const MAX_DEPENDENCIES: usize = 256;

/// Everything a note depends on, as vault paths, each once, in the order they are first named:
///
/// - what its text embeds and links to;
/// - the files its front matter names — `css:`, `theme: [cosmo, custom.scss]`, `filters:`,
///   `include-in-header:`, `reference-doc:`, `bibliography:` … any value that is the path of a
///   file in the vault, relative to the note, the vault root, or `attachments/`;
/// - and, followed through, what those name in turn: a stylesheet's `@import`/`@use`/`@forward`,
///   a YAML file's own paths.
///
/// This is what keeps a companion file synced and alive (the engine records every dependency in
/// the vault doc, and drops what no note depends on) and what a Quarto render lays out beside
/// the note. `exists` says whether a vault path is a file other than a note, `all` lists those
/// (for a bare `![[name]]`), `read` fetches one.
pub fn dependencies(
    note_path: &str,
    text: &str,
    ix: &crate::markdown::NoteIndex,
    exists: impl Fn(&str) -> bool,
    all: impl Fn() -> Vec<String>,
    read: impl Fn(&str) -> Option<Vec<u8>>,
) -> Vec<String> {
    let listed = std::cell::OnceCell::new();
    let all = || listed.get_or_init(&all).clone();
    let mut out: Vec<String> = Vec::new();
    let mut add = |p: Option<String>| {
        if let Some(p) = p
            && !out.contains(&p)
        {
            out.push(p);
        }
    };
    let targets = ix
        .wikilinks
        .iter()
        .filter(|w| w.embed)
        .map(|w| (w.target.clone(), true))
        .chain(ix.links.iter().map(|l| (l.clone(), false)));
    for (target, wiki) in targets {
        add(resolve_reference(note_path, &target, wiki, &exists, all));
    }
    if let Some((yaml, _)) = crate::frontmatter::block(text) {
        for target in yaml_paths(&text[yaml]) {
            add(resolve_reference(note_path, &target, false, &exists, all));
        }
    }
    follow(&mut out, &exists, &all, &read);
    out
}

/// Files that belong to the vault rather than to a note: Quarto's project file and its
/// per-folder `_metadata.yml`, and the `export/` folder (SPEC §12). They are kept and synced
/// whether or not a note links to them, and a Quarto render has them all.
pub fn is_vault_resource(path: &str) -> bool {
    matches!(path, "_quarto.yml" | "_quarto.yaml")
        || path.rsplit('/').next() == Some("_metadata.yml")
        || path.starts_with("export/")
}

/// Whether a file's content can name more files — so that changing it can change what a note
/// depends on.
pub fn names_files(path: &str) -> bool {
    is_stylesheet(path) || is_yaml(path) || is_vault_resource(path)
}

/// The vault's resources ([`is_vault_resource`]), with what they name followed through.
pub fn vault_resources(
    exists: impl Fn(&str) -> bool,
    all: impl Fn() -> Vec<String>,
    read: impl Fn(&str) -> Option<Vec<u8>>,
) -> Vec<String> {
    let listed = std::cell::OnceCell::new();
    let all = || listed.get_or_init(&all).clone();
    let mut out: Vec<String> = all().into_iter().filter(|p| is_vault_resource(p)).collect();
    follow(&mut out, &exists, &all, &read);
    out
}

fn extension(path: &str) -> String {
    path.rsplit('/')
        .next()
        .and_then(|b| b.rsplit_once('.'))
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default()
}

fn is_stylesheet(path: &str) -> bool {
    matches!(extension(path).as_str(), "scss" | "sass" | "css")
}

fn is_yaml(path: &str) -> bool {
    matches!(extension(path).as_str(), "yml" | "yaml")
}

/// Walk `out` from the start, appending what each stylesheet or YAML file in it names.
fn follow(
    out: &mut Vec<String>,
    exists: &impl Fn(&str) -> bool,
    all: &impl Fn() -> Vec<String>,
    read: &impl Fn(&str) -> Option<Vec<u8>>,
) {
    let mut i = 0;
    while i < out.len() && out.len() < MAX_DEPENDENCIES {
        let path = out[i].clone();
        i += 1;
        let named: Vec<String> = if is_stylesheet(&path) {
            let Some(bytes) = read(&path) else { continue };
            stylesheet_imports(&String::from_utf8_lossy(&bytes))
                .into_iter()
                .filter_map(|choices| {
                    choices
                        .into_iter()
                        .filter_map(|c| crate::projection::Projection::normalize_relative(&path, &c))
                        .find(|c| exists(c))
                })
                .collect()
        } else if is_yaml(&path) {
            let Some(bytes) = read(&path) else { continue };
            yaml_paths(&String::from_utf8_lossy(&bytes))
                .into_iter()
                .filter_map(|t| resolve_reference(&path, &t, false, exists, all))
                .collect()
        } else {
            continue;
        };
        for p in named {
            if !out.contains(&p) && out.len() < MAX_DEPENDENCIES {
                out.push(p);
            }
        }
    }
}

/// Every string in a YAML document that could be the path of a file: values, never keys, with
/// an extension of letters and digits, and not a URL. `theme: [cosmo, custom.scss]` names
/// `custom.scss`; `cosmo`, being no file, is left for Quarto. Unreadable YAML names nothing.
pub fn yaml_paths(yaml: &str) -> Vec<String> {
    fn walk(v: &serde_yaml_ng::Value, out: &mut Vec<String>) {
        match v {
            serde_yaml_ng::Value::String(s) if looks_like_path(s) => out.push(s.trim().to_owned()),
            serde_yaml_ng::Value::Sequence(items) => items.iter().for_each(|i| walk(i, out)),
            serde_yaml_ng::Value::Mapping(m) => m.values().for_each(|i| walk(i, out)),
            serde_yaml_ng::Value::Tagged(t) => walk(&t.value, out),
            _ => {}
        }
    }
    let mut out = Vec::new();
    if let Ok(v) = serde_yaml_ng::from_str::<serde_yaml_ng::Value>(yaml) {
        walk(&v, &mut out);
    }
    out
}

fn looks_like_path(s: &str) -> bool {
    let s = s.trim();
    if s.is_empty() || s.len() > 512 || s.contains('\n') || s.contains("://") || s.starts_with("data:") {
        return false;
    }
    let Some((stem, ext)) = s.rsplit('/').next().and_then(|b| b.rsplit_once('.')) else { return false };
    !stem.is_empty()
        && (1..=8).contains(&ext.len())
        && ext.chars().all(|c| c.is_ascii_alphanumeric())
        && ext.chars().any(|c| c.is_ascii_alphabetic())
}

/// What a stylesheet imports — `@import`, `@use` and `@forward` — as the files Sass would try
/// for each, relative to the stylesheet: `"vars"` is `_vars.scss`, `vars.scss`, the `.sass`
/// pair, `vars.css`, then `vars/_index.scss`. Built-in modules (`sass:math`) and URLs name no
/// file. Comments are skipped, so an import commented out is not followed.
pub fn stylesheet_imports(css: &str) -> Vec<Vec<String>> {
    let text = strip_css_comments(css);
    let mut out = Vec::new();
    let mut rest = text.as_str();
    while let Some(at) = rest.find('@') {
        rest = &rest[at + 1..];
        let keyword = rest.split(|c: char| !c.is_ascii_alphabetic()).next().unwrap_or("");
        if !matches!(keyword, "import" | "use" | "forward") {
            continue;
        }
        rest = &rest[keyword.len()..];
        // `@import "a", "b";` lists several; `@use "a" as b;` names one.
        loop {
            rest = rest.trim_start();
            let Some(q) = rest.chars().next().filter(|c| *c == '"' || *c == '\'') else { break };
            let Some(end) = rest[1..].find(q) else { break };
            let target = &rest[1..1 + end];
            rest = &rest[end + 2..];
            if !target.contains("://") && !target.starts_with("sass:") && !target.is_empty() {
                out.push(sass_candidates(target));
            }
            let after = rest.trim_start();
            if keyword == "import" && after.starts_with(',') {
                rest = &after[1..];
            } else {
                break;
            }
        }
    }
    out
}

fn sass_candidates(target: &str) -> Vec<String> {
    let (dir, base) = match target.rsplit_once('/') {
        Some((d, b)) => (format!("{d}/"), b),
        None => (String::new(), target),
    };
    if matches!(extension(base).as_str(), "scss" | "sass" | "css") {
        return vec![target.to_owned(), format!("{dir}_{base}")];
    }
    vec![
        format!("{dir}_{base}.scss"),
        format!("{dir}{base}.scss"),
        format!("{dir}_{base}.sass"),
        format!("{dir}{base}.sass"),
        format!("{dir}{base}.css"),
        format!("{target}/_index.scss"),
        format!("{target}/index.scss"),
    ]
}

fn strip_css_comments(css: &str) -> String {
    let mut out = String::with_capacity(css.len());
    let mut rest = css;
    loop {
        let block = rest.find("/*");
        let line = rest.find("//").filter(|&l| {
            // `url(https://…)` is not a comment.
            !rest[..l].ends_with(':')
        });
        match (block, line) {
            (Some(b), l) if l.is_none_or(|l| b < l) => {
                out.push_str(&rest[..b]);
                match rest[b + 2..].find("*/") {
                    Some(e) => rest = &rest[b + 2 + e + 2..],
                    None => return out,
                }
            }
            (_, Some(l)) => {
                out.push_str(&rest[..l]);
                match rest[l..].find('\n') {
                    Some(e) => rest = &rest[l + e..],
                    None => return out,
                }
            }
            _ => {
                out.push_str(rest);
                return out;
            }
        }
    }
}

/// Server-side blob store: `<root>/<vault>/<hh>/<hash>`.
#[derive(Debug, Clone)]
pub struct AttachmentStore {
    root: PathBuf,
}

impl AttachmentStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn path_for(&self, vault: VaultId, hash: &str) -> Result<PathBuf> {
        if !is_valid_hash(hash) {
            return Err(Error::Sync(format!("invalid attachment hash {hash:?}")));
        }
        Ok(self.root.join(vault.to_string()).join(&hash[..2]).join(hash))
    }

    pub fn exists(&self, vault: VaultId, hash: &str) -> bool {
        self.path_for(vault, hash).is_ok_and(|p| p.is_file())
    }

    /// Store bytes; returns their hash and whether the blob was newly written.
    pub fn put(&self, vault: VaultId, bytes: &[u8]) -> Result<(String, bool)> {
        let hash = hash_bytes(bytes);
        let target = self.path_for(vault, &hash)?;
        if target.is_file() {
            return Ok((hash, false));
        }
        let parent = target.parent().expect("hash path has a parent");
        fs::create_dir_all(parent)?;
        let tmp = parent.join(format!(".{hash}.tmp"));
        fs::write(&tmp, bytes)?;
        fs::rename(&tmp, &target)?;
        Ok((hash, true))
    }

    pub fn get(&self, vault: VaultId, hash: &str) -> Result<Option<Vec<u8>>> {
        let path = self.path_for(vault, hash)?;
        match fs::read(&path) {
            Ok(b) => Ok(Some(b)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Drop a whole vault's blobs, for a vault that has stopped existing (SPEC §3.2: merged
    /// into another). Missing is fine — a vault that never had an attachment has no directory.
    pub fn remove_vault(&self, vault: VaultId) -> Result<()> {
        match fs::remove_dir_all(self.root.join(vault.to_string())) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    pub fn remove(&self, vault: VaultId, hash: &str) -> Result<()> {
        match fs::remove_file(self.path_for(vault, hash)?) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A vault as a map of path → content, with `.md`/`.qmd` counting as notes, not files.
    fn vault(files: &[(&str, &str)]) -> std::collections::HashMap<String, String> {
        files.iter().map(|(p, c)| ((*p).to_owned(), (*c).to_owned())).collect()
    }

    fn deps_in(v: &std::collections::HashMap<String, String>, note: &str) -> Vec<String> {
        let text = &v[note];
        let ix = crate::markdown::index(text).unwrap();
        let is_file = |p: &str| v.contains_key(p) && !p.ends_with(".md") && !p.ends_with(".qmd");
        let mut all: Vec<String> = v.keys().filter(|p| is_file(p)).cloned().collect();
        all.sort();
        dependencies(note, text, &ix, is_file, || all.clone(), |p| v.get(p).map(|c| c.as_bytes().to_vec()))
    }

    #[test]
    fn front_matter_names_files_and_stylesheets_are_followed() {
        let v = vault(&[
            (
                "talks/deck.qmd",
                "---\ntitle: A talk v1.2\nformat:\n  html:\n    theme: [cosmo, custom.scss]\n    css: /shared/extra.css\n    \
                 include-in-header: header.html\nfilters: [ ../filters/wordcount.lua ]\nlogo: https://example.org/logo.png\n\
                 reference-doc: missing.docx\n---\n![](img/pic.png)\n",
            ),
            (
                "talks/custom.scss",
                "/*-- scss:defaults --*/\n@import 'partials/vars', \"mixins\";\n@use \"sass:math\";\n// @import \"commented\";\n",
            ),
            ("talks/partials/_vars.scss", "@forward '../../theme/base';\n$x: 1;"),
            ("talks/_mixins.scss", "@import \"partials/vars\"; // a cycle, back to vars"),
            ("theme/base/_index.scss", "body { background: url(https://example.org/x.png); }"),
            ("talks/commented.scss", ""),
            ("shared/extra.css", "@import url(\"https://fonts.example.org/f.css\");"),
            ("talks/header.html", "<meta>"),
            ("filters/wordcount.lua", "return {}"),
            ("talks/img/pic.png", "png"),
            ("talks/unrelated.png", "png"),
        ]);
        assert_eq!(
            deps_in(&v, "talks/deck.qmd"),
            [
                "talks/img/pic.png",
                "talks/custom.scss",
                "shared/extra.css",
                "talks/header.html",
                "filters/wordcount.lua",
                "talks/partials/_vars.scss",
                "talks/_mixins.scss",
                "theme/base/_index.scss",
            ]
        );
    }

    #[test]
    fn yaml_values_that_look_like_files() {
        let y = "title: Notes on v1.2\ntheme: [cosmo, a.scss]\nnested: {deep: [x/y.lua]}\nurl: https://a.b/c.css\n\
                 key.scss: not-a-path\nn: 3.14\nmulti: |\n  a.css\n  b.css\n";
        assert_eq!(yaml_paths(y), ["a.scss", "x/y.lua"]);
        assert!(yaml_paths("format: [\n").is_empty(), "unreadable YAML names nothing");
    }

    #[test]
    fn sass_resolution_candidates() {
        assert_eq!(
            stylesheet_imports("@use 'a/b' as c; @import \"d.css\";"),
            [
                vec![
                    "a/_b.scss",
                    "a/b.scss",
                    "a/_b.sass",
                    "a/b.sass",
                    "a/b.css",
                    "a/b/_index.scss",
                    "a/b/index.scss"
                ],
                vec!["d.css", "_d.css"],
            ]
        );
        assert!(stylesheet_imports("/* @import 'x'; */ a { b: c }").is_empty());
    }

    #[test]
    fn vault_resources_and_what_they_name() {
        let v = vault(&[
            ("_quarto.yml", "format:\n  html:\n    theme: styles/site.scss\n"),
            ("styles/site.scss", "@import 'vars';"),
            ("styles/_vars.scss", ""),
            ("notes/_metadata.yml", "css: notes.css\n"),
            ("notes/notes.css", ""),
            ("export/references.bib", "@book{x}"),
            ("attachments/pic.png", ""),
        ]);
        let mut all: Vec<String> = v.keys().cloned().collect();
        all.sort();
        let got = vault_resources(
            |p| v.contains_key(p),
            || all.clone(),
            |p| v.get(p).map(|c| c.as_bytes().to_vec()),
        );
        assert_eq!(
            got,
            [
                "_quarto.yml",
                "export/references.bib",
                "notes/_metadata.yml",
                "styles/site.scss",
                "notes/notes.css",
                "styles/_vars.scss",
            ]
        );
        assert!(
            names_files("a/b.SCSS") && names_files("x/_metadata.yml") && names_files("export/references.bib")
        );
        assert!(!names_files("attachments/pic.png"));
    }

    #[test]
    fn resolves_references() {
        let files = ["attachments/a.png".to_owned(), "sub/img/b.bin".to_owned()];
        let exists = |p: &str| files.contains(&p.to_owned());
        let all = || files.to_vec();
        assert_eq!(
            resolve_reference("sub/n.md", "img/b.bin", false, exists, all).as_deref(),
            Some("sub/img/b.bin")
        );
        assert_eq!(
            resolve_reference("sub/n.md", "../attachments/a.png", false, exists, all).as_deref(),
            Some("attachments/a.png")
        );
        assert_eq!(
            resolve_reference("n.md", "a.png", true, exists, all).as_deref(),
            Some("attachments/a.png")
        );
        assert_eq!(resolve_reference("n.md", "b.bin", true, exists, all).as_deref(), Some("sub/img/b.bin"));
        assert_eq!(resolve_reference("n.md", "b.bin", false, exists, all), None);
        assert_eq!(resolve_reference("n.md", "https://x/a.png", true, exists, all), None);
        assert_eq!(
            resolve_reference("n.md", "a.png#frag", true, exists, all).as_deref(),
            Some("attachments/a.png")
        );
    }

    #[test]
    fn put_get_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let store = AttachmentStore::new(dir.path());
        let v = VaultId::new();
        let (h, created) = store.put(v, b"hello").unwrap();
        assert!(created && is_valid_hash(&h));
        assert_eq!(store.put(v, b"hello").unwrap(), (h.clone(), false));
        assert_eq!(store.get(v, &h).unwrap().as_deref(), Some(&b"hello"[..]));
        assert_eq!(store.get(VaultId::new(), &h).unwrap(), None);
        assert!(store.path_for(v, "../x").is_err());
        assert!(store.exists(v, &h));
        store.remove(v, &h).unwrap();
        store.remove(v, &h).unwrap();
        assert!(!store.exists(v, &h));
        assert_eq!(mime_for_path("a/b.png"), "image/png");
        assert_eq!(mime_for_path("weird.zzz"), "application/octet-stream");
    }
}

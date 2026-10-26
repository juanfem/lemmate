//! Web fonts a vault stylesheet loads from the network, fetched by Lemmate before a render and
//! written into the stylesheet itself (SPEC §5.6).
//!
//! A theme that says `@import url("https://cdn…/inter.css")` renders fine where Quarto can reach
//! the CDN. A render that runs approved Lua filters cannot — it is confined with no network
//! ([`crate::sandbox`]) — and there pandoc's `embed-resources`, failing the fetch, writes the
//! `@import` back without its `;`, so the browser reads the theme's next rule as part of it and
//! drops it: the whole `:root` block of reveal.js variables, fonts, sizes and colours with it.
//!
//! So the fetching happens here, outside the render: each `@import` of an `https:` stylesheet is
//! replaced by that stylesheet's text, and each font it (or the vault's own stylesheet) names by
//! `https:` URL becomes a `data:` URI. Quarto then has nothing to fetch, confined or not, and a
//! device that fetched a font once renders it offline from the cache. An import that cannot be
//! fetched is commented out, so the rest of the theme survives and the font falls back.
//!
//! Only `https:`, only stylesheets and font files (by content type and extension), bounded in
//! size, and never a local or private host named literally. That is no wider than what an
//! unconfined Quarto render already fetches by itself.

use std::collections::HashMap;
use std::fmt;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::Arc;

use base64::Engine as _;
use url::Url;

/// What a fetched URL is expected to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Stylesheet,
    Font,
}

/// A stylesheet fetched by an `@import`, at most.
const MAX_STYLESHEET: u64 = 2 << 20;
/// One font file, at most.
const MAX_FONT: u64 = 16 << 20;
/// Everything one stylesheet pulls in, at most: Inter's 18 static faces are about 2 MB.
const MAX_TOTAL: usize = 48 << 20;
/// `@import`s within `@import`s, at most.
const MAX_DEPTH: usize = 3;
/// Fonts fetched at once.
const PARALLEL: usize = 8;
/// Google Fonts answers by user agent; this one is sent `woff2`.
const USER_AGENT: &str =
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36";

type Fetch = dyn Fn(&Url, Kind) -> Option<Vec<u8>> + Send + Sync;

/// How a render fetches what a stylesheet loads from the network.
#[derive(Clone)]
pub struct WebFonts {
    fetch: Arc<Fetch>,
}

impl fmt::Debug for WebFonts {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("WebFonts")
    }
}

impl WebFonts {
    /// Fetch over HTTPS, keeping what was fetched in `cache` (when there is one) for good: a
    /// font's URL names a version, and the next render — perhaps offline — finds it there.
    pub fn online(cache: Option<PathBuf>) -> Self {
        let agent = crate::tls::http_agent(None).ok();
        Self::with(move |url, kind| {
            let key =
                cache.as_ref().map(|dir| dir.join(crate::attachments::hash_bytes(url.as_str().as_bytes())));
            if let Some(bytes) = key.as_ref().and_then(|k| std::fs::read(k).ok()) {
                return Some(bytes);
            }
            let bytes = get(agent.as_ref()?, url, kind)?;
            if let (Some(dir), Some(key)) = (&cache, &key) {
                let tmp = key.with_extension("part");
                let saved = std::fs::create_dir_all(dir)
                    .and_then(|_| std::fs::write(&tmp, &bytes))
                    .and_then(|_| std::fs::rename(&tmp, key));
                if let Err(e) = saved {
                    tracing::warn!(dir = %dir.display(), "render: web font cache not written: {e}");
                }
            }
            Some(bytes)
        })
    }

    /// Fetch with `fetch`: `None` is a fetch that failed.
    pub fn with(fetch: impl Fn(&Url, Kind) -> Option<Vec<u8>> + Send + Sync + 'static) -> Self {
        Self { fetch: Arc::new(fetch) }
    }

    /// `css` (a vault `.css` or `.scss` file) with its remote imports and fonts written in.
    pub fn localize(&self, css: &str) -> String {
        let mut budget = MAX_TOTAL;
        self.inline(css, None, 0, &mut budget)
    }

    /// `css`, read from `base` (`None`: a vault file, whose relative URLs are vault paths).
    fn inline(&self, css: &str, base: Option<&Url>, depth: usize, budget: &mut usize) -> String {
        let mut out = String::with_capacity(css.len());
        let mut at = 0;
        for import in imports(css) {
            out.push_str(&self.fonts(&css[at..import.span.start], base, budget));
            at = import.span.end;
            let statement = &css[import.span.clone()];
            let Some(url) = resolve(base, &import.target) else {
                // A vault file, or nothing fetchable: as it was.
                out.push_str(&absolute(base, statement, &import.target));
                continue;
            };
            let fetched = (depth < MAX_DEPTH)
                .then(|| (self.fetch)(&url, Kind::Stylesheet))
                .flatten()
                .filter(|b| b.len() <= *budget)
                .and_then(|b| String::from_utf8(b).ok());
            match fetched {
                Some(text) => {
                    *budget -= text.len();
                    let text = self.inline(&text, Some(&url), depth + 1, budget);
                    let media = import.media.trim();
                    out.push_str(&format!("/* {} */\n", comment_safe(url.as_str())));
                    if media.is_empty() || media.starts_with("layer") || media.starts_with("supports") {
                        out.push_str(&text);
                    } else {
                        out.push_str(&format!("@media {media} {{\n{text}\n}}"));
                    }
                    out.push('\n');
                }
                None => {
                    tracing::warn!(url = %url, "render: a stylesheet import could not be fetched");
                    out.push_str(&format!("/* not fetched: {} */", comment_safe(url.as_str())));
                }
            }
        }
        out.push_str(&self.fonts(&css[at..], base, budget));
        out
    }

    /// `css` with each `https:` font it names as a `data:` URI, and — read from `base` — every
    /// other relative URL made absolute, since it means nothing in the vault.
    fn fonts(&self, css: &str, base: Option<&Url>, budget: &mut usize) -> String {
        let refs = urls(css);
        let wanted: Vec<Url> = refs
            .iter()
            .filter_map(|r| resolve(base, &r.target))
            .filter(|u| font_mime(u).is_some())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        let mut got: HashMap<Url, Vec<u8>> = HashMap::new();
        for chunk in wanted.chunks(PARALLEL) {
            let fetched: Vec<_> = std::thread::scope(|s| {
                let handles: Vec<_> =
                    chunk.iter().map(|u| s.spawn(|| (u.clone(), (self.fetch)(u, Kind::Font)))).collect();
                handles.into_iter().filter_map(|h| h.join().ok()).collect()
            });
            for (url, bytes) in fetched {
                match bytes {
                    Some(b) if b.len() <= *budget => {
                        *budget -= b.len();
                        got.insert(url, b);
                    }
                    _ => tracing::warn!(url = %url, "render: a web font could not be fetched"),
                }
            }
        }
        let mut out = String::with_capacity(css.len());
        let mut at = 0;
        for r in refs {
            let replacement = match resolve(base, &r.target) {
                Some(url) => match (got.get(&url), font_mime(&url)) {
                    (Some(bytes), Some(mime)) => Some(format!(
                        "url(\"data:{mime};base64,{}\")",
                        base64::engine::general_purpose::STANDARD.encode(bytes)
                    )),
                    _ if base.is_some() => Some(format!("url(\"{url}\")")),
                    _ => None,
                },
                None => base.and_then(|b| b.join(&r.target).ok()).map(|u| format!("url(\"{u}\")")),
            };
            if let Some(replacement) = replacement {
                out.push_str(&css[at..r.span.start]);
                out.push_str(&replacement);
                at = r.span.end;
            }
        }
        out.push_str(&css[at..]);
        out
    }
}

/// One HTTPS GET of a stylesheet or a font: `None` unless it is what was asked for.
fn get(agent: &ureq::Agent, url: &Url, kind: Kind) -> Option<Vec<u8>> {
    if !fetchable(url) {
        return None;
    }
    let mut resp = agent.get(url.as_str()).header("User-Agent", USER_AGENT).call().ok()?;
    let mime = resp.body().mime_type().unwrap_or_default().to_ascii_lowercase();
    let (fits, limit) = match kind {
        Kind::Stylesheet => (mime == "text/css", MAX_STYLESHEET),
        Kind::Font => (
            mime.starts_with("font/")
                || mime.starts_with("application/font")
                || mime.starts_with("application/x-font")
                || matches!(mime.as_str(), "application/vnd.ms-fontobject" | "application/octet-stream"),
            MAX_FONT,
        ),
    };
    if !fits {
        tracing::warn!(url = %url, mime, "render: not fetched, not a {kind:?}");
        return None;
    }
    resp.body_mut().with_config().limit(limit).read_to_vec().ok()
}

/// Worth fetching: `https:`, to a host that is not this machine or its network by name.
fn fetchable(url: &Url) -> bool {
    if url.scheme() != "https" {
        return false;
    }
    match url.host() {
        Some(url::Host::Domain(d)) => {
            let d = d.trim_end_matches('.').to_ascii_lowercase();
            !(d == "localhost"
                || [".localhost", ".local", ".internal", ".lan", ".home.arpa"].iter().any(|s| d.ends_with(s)))
        }
        Some(url::Host::Ipv4(ip)) => public(IpAddr::V4(ip)),
        Some(url::Host::Ipv6(ip)) => public(IpAddr::V6(ip)),
        None => false,
    }
}

fn public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            !(v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.octets()[0] == 100 && (64..128).contains(&v4.octets()[1]))
        }
        IpAddr::V6(v6) => {
            let seg = v6.segments()[0];
            !(v6.is_loopback()
                || v6.is_unspecified()
                || (seg & 0xfe00) == 0xfc00
                || (seg & 0xffc0) == 0xfe80
                || v6.to_ipv4_mapped().is_some_and(|v4| !public(IpAddr::V4(v4))))
        }
    }
}

/// The URL `target` names, if it is one to fetch: `https:`, absolute or (read from `base`)
/// relative. A relative one in a vault file is a vault path.
fn resolve(base: Option<&Url>, target: &str) -> Option<Url> {
    let target = target.trim();
    if target.is_empty() || target.starts_with('#') || target.to_ascii_lowercase().starts_with("data:") {
        return None;
    }
    let url = match base {
        Some(b) => b.join(target).ok()?,
        None if target.starts_with("//") => Url::parse(&format!("https:{target}")).ok()?,
        None => Url::parse(target).ok()?,
    };
    (url.scheme() == "https").then_some(url)
}

/// An import left as it was: in fetched text, with its target made absolute.
fn absolute(base: Option<&Url>, statement: &str, target: &str) -> String {
    match base.and_then(|b| b.join(target).ok()) {
        Some(u) => statement.replacen(target, u.as_str(), 1),
        None => statement.to_owned(),
    }
}

/// The MIME type of a font file, by its extension: `None` for anything else.
fn font_mime(url: &Url) -> Option<&'static str> {
    let ext = url.path().rsplit_once('.')?.1.to_ascii_lowercase();
    Some(match ext.as_str() {
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "eot" => "application/vnd.ms-fontobject",
        _ => return None,
    })
}

/// A URL inside a `/* … */` comment: one that cannot close it early.
fn comment_safe(s: &str) -> String {
    s.replace("*/", "*%2F")
}

/// An `@import` statement: where it is, what it names, and any media list after that.
#[derive(Debug, PartialEq)]
struct Import {
    span: std::ops::Range<usize>,
    target: String,
    media: String,
}

/// A `url(…)`: where it is, and what it names.
#[derive(Debug, PartialEq)]
struct UrlRef {
    span: std::ops::Range<usize>,
    target: String,
}

/// Where `css` is a comment: `/* … */` and, as SCSS has them, `//` to the end of a line — but not
/// the `//` of a URL (`https://`), a string's or one in a `url(…)`.
fn comments(css: &str) -> Vec<std::ops::Range<usize>> {
    let b = css.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    let mut quote: Option<u8> = None;
    let mut in_url = false;
    while i < b.len() {
        let c = b[i];
        if let Some(q) = quote {
            if c == b'\\' {
                i += 1;
            } else if c == q || c == b'\n' {
                quote = None;
            }
        } else if c == b'/' && b.get(i + 1) == Some(&b'*') {
            let end = css[i + 2..].find("*/").map_or(b.len(), |e| i + 2 + e + 2);
            out.push(i..end);
            i = end;
            continue;
        } else if c == b'/' && b.get(i + 1) == Some(&b'/') && !in_url && (i == 0 || b[i - 1] != b':') {
            let end = css[i..].find('\n').map_or(b.len(), |e| i + e);
            out.push(i..end);
            i = end;
            continue;
        } else if c == b'"' || c == b'\'' {
            quote = Some(c);
        } else if b.len() >= i + 4 && b[i..i + 4].eq_ignore_ascii_case(b"url(") {
            in_url = true;
        } else if c == b')' {
            in_url = false;
        }
        i += 1;
    }
    out
}

fn in_any(ranges: &[std::ops::Range<usize>], at: usize) -> bool {
    ranges.iter().any(|r| r.contains(&at))
}

/// A quoted string or a `url(…)` at the start of `s`: what it names and how long it is.
fn target_at(s: &str) -> Option<(String, usize)> {
    if let Some(q) = s.chars().next().filter(|c| matches!(c, '"' | '\'')) {
        let end = s[1..].find([q, '\n'])? + 1;
        return (s.as_bytes()[end] == q as u8).then(|| (s[1..end].to_owned(), end + 1));
    }
    if s.len() >= 4 && s.as_bytes()[..4].eq_ignore_ascii_case(b"url(") {
        let inner = &s[4..];
        let lead = inner.len() - inner.trim_start().len();
        let rest = &inner[lead..];
        let (target, used) = match rest.chars().next() {
            Some('"' | '\'') => target_at(rest)?,
            _ => {
                let end = rest.find(')')?;
                (rest[..end].trim_end().to_owned(), end)
            }
        };
        let after = &rest[used..];
        let close = after.find(')')?;
        if !after[..close].trim().is_empty() {
            return None;
        }
        return Some((target, 4 + lead + used + close + 1));
    }
    None
}

/// Every `@import` statement in `css` outside comments.
fn imports(css: &str) -> Vec<Import> {
    let skip = comments(css);
    let lower = css.to_ascii_lowercase();
    let mut out: Vec<Import> = Vec::new();
    for (at, _) in lower.match_indices("@import") {
        if in_any(&skip, at) || out.last().is_some_and(|i| i.span.end > at) {
            continue;
        }
        let from = at + "@import".len();
        let lead = css[from..].len() - css[from..].trim_start().len();
        let Some((target, used)) = target_at(&css[from + lead..]) else { continue };
        let rest_at = from + lead + used;
        // A statement ends at its `;`; a Sass `@import "a", "b"` list is Sass's, not ours.
        let Some(semi) = css[rest_at..].find([';', '\n', '{', '}']) else { continue };
        if css.as_bytes()[rest_at + semi] != b';' {
            continue;
        }
        let media = css[rest_at..rest_at + semi].to_owned();
        if media.trim_start().starts_with(',') {
            continue;
        }
        out.push(Import { span: at..rest_at + semi + 1, target, media });
    }
    out
}

/// Every `url(…)` in `css` outside comments.
fn urls(css: &str) -> Vec<UrlRef> {
    let skip = comments(css);
    let lower = css.to_ascii_lowercase();
    let mut out: Vec<UrlRef> = Vec::new();
    for (at, _) in lower.match_indices("url(") {
        if in_any(&skip, at) || out.last().is_some_and(|u| u.span.end > at) {
            continue;
        }
        // `url(` as a word, not the end of another one.
        if at > 0 && (css.as_bytes()[at - 1].is_ascii_alphanumeric() || css.as_bytes()[at - 1] == b'-') {
            continue;
        }
        if let Some((target, used)) = target_at(&css[at..]) {
            out.push(UrlRef { span: at..at + used, target });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    const INTER: &str = "@font-face {\n  font-family: \"Inter\";\n  font-weight: 400;\n  src: url(\"web/Inter-Regular.woff2?v=4.1\") format(\"woff2\");\n}\n.x { background: url(img/bg.png) }\n";

    /// A web of two hosts, counting what is asked of it.
    fn web(asked: Arc<Mutex<Vec<String>>>) -> WebFonts {
        WebFonts::with(move |url, kind| {
            asked.lock().unwrap().push(url.to_string());
            match (url.as_str(), kind) {
                ("https://cdn.example/inter@4/inter.css", Kind::Stylesheet) => Some(INTER.into()),
                ("https://cdn.example/inter@4/web/Inter-Regular.woff2?v=4.1", Kind::Font) => {
                    Some(b"WOFF2".to_vec())
                }
                ("https://fonts.example/css?family=X", Kind::Stylesheet) => Some(
                    b"@import url('nested.css') screen;\n@font-face { src: url(//fonts.example/x.ttf) }"
                        .to_vec(),
                ),
                ("https://fonts.example/nested.css", Kind::Stylesheet) => Some(b".n { color: red }".to_vec()),
                ("https://fonts.example/x.ttf", Kind::Font) => Some(b"TTF".to_vec()),
                _ => None,
            }
        })
    }

    fn b64(s: &str) -> String {
        base64::engine::general_purpose::STANDARD.encode(s)
    }

    #[test]
    fn a_remote_import_is_written_in_with_its_fonts() {
        let asked = Arc::new(Mutex::new(Vec::new()));
        let css = "/*-- scss:rules --*/\n// the full Inter\n@import url(\"https://cdn.example/inter@4/inter.css\");\n\n.reveal { color: $ink; }\n";
        let out = web(asked.clone()).localize(css);
        assert!(!out.contains("@import"), "{out}");
        assert!(out.contains(&format!("url(\"data:font/woff2;base64,{}\")", b64("WOFF2"))), "{out}");
        assert!(
            out.contains("url(\"https://cdn.example/inter@4/img/bg.png\")"),
            "the rest made absolute: {out}"
        );
        assert!(out.contains("// the full Inter\n") && out.ends_with(".reveal { color: $ink; }\n"), "{out}");
        assert_eq!(asked.lock().unwrap().len(), 2, "the stylesheet and its one font");
    }

    #[test]
    fn imports_nest_and_keep_their_media() {
        let out = web(Default::default()).localize("@import 'https://fonts.example/css?family=X';\nbody{}");
        assert!(out.contains("@media screen {\n.n { color: red }\n}"), "{out}");
        assert!(out.contains(&format!("data:font/ttf;base64,{}", b64("TTF"))), "{out}");
        assert!(out.ends_with("body{}"));
    }

    #[test]
    fn what_cannot_be_fetched_is_commented_out_and_the_rest_kept() {
        let css = "@import url(\"https://cdn.example/gone.css\");\n:root { --x: 1 }\n";
        let out = web(Default::default()).localize(css);
        assert_eq!(out, "/* not fetched: https://cdn.example/gone.css */\n:root { --x: 1 }\n");
    }

    #[test]
    fn vault_files_comments_and_other_schemes_are_left_alone() {
        let asked = Arc::new(Mutex::new(Vec::new()));
        let css = "@import \"partials/base.css\";\n@import url(http://cdn.example/x.css);\n\
                   /* @import url(\"https://cdn.example/inter@4/inter.css\"); */\n\
                   // @import url(\"https://cdn.example/inter@4/inter.css\");\n\
                   @font-face { src: url(fonts/local.woff2), url(\"data:font/woff2;base64,AA==\") }\n\
                   @import \"a\", \"b\";\n";
        let out = web(asked.clone()).localize(css);
        assert_eq!(out, css);
        assert!(asked.lock().unwrap().is_empty());
    }

    #[test]
    fn text_outside_ascii_is_read_as_text() {
        let css = ".a::after { content: \"→ ↔\"; } /* ⁻ */ .b { background: url(é.png) }\n\
                   @import url(\"https://cdn.example/inter@4/inter.css\"); .c::before { content: '→' }";
        let out = web(Default::default()).localize(css);
        assert!(
            out.starts_with(".a::after { content: \"→ ↔\"; } /* ⁻ */ .b { background: url(é.png) }"),
            "{out}"
        );
        assert!(out.contains("data:font/woff2") && out.ends_with(" .c::before { content: '→' }"), "{out}");
    }

    #[test]
    fn a_vault_font_by_url_is_fetched() {
        let css = "@font-face { src: url('https://fonts.example/x.ttf') format('truetype'); }";
        let out = web(Default::default()).localize(css);
        assert_eq!(
            out,
            format!("@font-face {{ src: url(\"data:font/ttf;base64,{}\") format('truetype'); }}", b64("TTF"))
        );
    }

    #[test]
    fn only_public_https_hosts_are_fetched() {
        let yes = ["https://cdn.jsdelivr.net/a.css", "https://8.8.8.8/a.css", "https://[2606:4700::1]/a"];
        let no = [
            "http://cdn.jsdelivr.net/a.css",
            "https://localhost/a",
            "https://nas.local/a",
            "https://127.0.0.1/a",
            "https://10.1.2.3/a",
            "https://192.168.1.1/a",
            "https://169.254.169.254/latest",
            "https://100.64.0.1/a",
            "https://[::1]/a",
            "https://[fd00::1]/a",
            "https://[::ffff:192.168.0.1]/a",
        ];
        for u in yes {
            assert!(fetchable(&Url::parse(u).unwrap()), "{u}");
        }
        for u in no {
            assert!(!fetchable(&Url::parse(u).unwrap()), "{u}");
        }
    }

    #[test]
    fn the_cache_serves_a_second_render_offline() {
        let dir = tempfile::tempdir().unwrap();
        let url = Url::parse("https://cdn.example/f.woff2").unwrap();
        let key = dir.path().join(crate::attachments::hash_bytes(url.as_str().as_bytes()));
        std::fs::write(&key, b"CACHED").unwrap();
        let fonts = WebFonts::online(Some(dir.path().to_path_buf()));
        assert_eq!((fonts.fetch)(&url, Kind::Font).as_deref(), Some(&b"CACHED"[..]));
    }
}

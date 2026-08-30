//! Blocking REST client for a `notes-server` (SPEC §13.1), shared by the remote CLI commands
//! (SPEC §13.2) and the MCP server (SPEC §13.3).
//!
//! Everything speaks JSON over `ureq` and authenticates with `Authorization: Bearer <token>`,
//! taking the token from `--token`/`NOTES_TOKEN` or, failing that, from the file `notes login`
//! writes. Non-2xx answers become [`HttpError`]s that carry the status, so callers (and the
//! MCP tools) can tell "no such note" from "not signed in".

use std::fmt;
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use notes_core::{NoteId, VaultId, credentials, tls};
use serde::{Deserialize, Serialize};

/// Response bodies are notes and note lists; a very large vault still fits comfortably.
const BODY_LIMIT: u64 = 64 * 1024 * 1024;

// ---- Wire types ------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VaultSummary {
    pub id: String,
    #[serde(default)]
    pub notes: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoteSummary {
    pub id: String,
    pub path: String,
    #[serde(default)]
    pub title: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Note {
    pub id: String,
    pub path: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub content: String,
}

impl Note {
    pub fn summary(&self) -> NoteSummary {
        NoteSummary { id: self.id.clone(), path: self.path.clone(), title: self.title.clone() }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchHit {
    pub note_id: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub snippet: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TagCount {
    pub tag: String,
    #[serde(default)]
    pub count: u32,
}

// ---- Errors ----------------------------------------------------------------------------------

/// A non-2xx answer, keeping the status so callers can branch on it (404 → create instead).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpError {
    pub status: u16,
    /// `METHOD /path` of the request that failed, for the message.
    pub request: String,
}

impl HttpError {
    fn explain(&self) -> &'static str {
        match self.status {
            400 => "the server rejected the request",
            401 => "not signed in: run `notes login`",
            403 => "no access: you need editor rights on this vault",
            404 => "not found or no access",
            409 => "conflict: a note already exists at that path",
            413 => "too large",
            501 => "the server does not support this operation",
            s if s >= 500 => "the server failed",
            _ => "the request failed",
        }
    }
}

impl fmt::Display for HttpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} (HTTP {} from {})", self.explain(), self.status, self.request)
    }
}

impl std::error::Error for HttpError {}

/// The HTTP status behind an error, when there is one.
pub fn status_of(err: &anyhow::Error) -> Option<u16> {
    err.downcast_ref::<HttpError>().map(|e| e.status)
}

/// True when the error means "the server has no such thing".
pub fn is_not_found(err: &anyhow::Error) -> bool {
    status_of(err) == Some(404)
}

fn transport_error(err: ureq::Error, request: &str) -> anyhow::Error {
    match err {
        ureq::Error::StatusCode(status) => {
            anyhow::Error::from(HttpError { status, request: request.to_owned() })
        }
        other => anyhow!("{request} failed: {other}"),
    }
}

// ---- Client ----------------------------------------------------------------------------------

pub struct Remote {
    agent: ureq::Agent,
    base: String,
    token: Option<String>,
}

impl Remote {
    /// Build a client from the common `--server/--token/--ca-cert` options. The token falls back
    /// to the one `notes login` saved for this server.
    pub fn from_args(server: &str, token: Option<String>, ca_cert: Option<&Path>) -> Result<Self> {
        tls::install_crypto_provider();
        let base = credentials::key(server);
        if !(base.starts_with("http://") || base.starts_with("https://")) {
            bail!("--server must be an http:// or https:// URL (got {server:?})");
        }
        let agent = tls::http_agent(ca_cert).map_err(|e| anyhow!("{e}"))?;
        let token = token.filter(|t| !t.is_empty()).or_else(|| credentials::load(&base));
        Ok(Self { agent, base, token })
    }

    /// The normalised server base URL (no trailing slash).
    pub fn base(&self) -> &str {
        &self.base
    }

    fn url(&self, path: &str) -> String {
        format!("{}/api/v1{path}", self.base)
    }

    fn auth<B>(&self, req: ureq::RequestBuilder<B>) -> ureq::RequestBuilder<B> {
        match &self.token {
            Some(t) => req.header("authorization", format!("Bearer {t}")),
            None => req,
        }
    }

    fn get_text(&self, path: &str, query: &[(&str, String)]) -> Result<String> {
        let mut req = self.auth(self.agent.get(self.url(path)));
        for (k, v) in query {
            req = req.query(*k, v);
        }
        let what = format!("GET {path}");
        let mut resp = req.call().map_err(|e| transport_error(e, &what))?;
        resp.body_mut()
            .with_config()
            .limit(BODY_LIMIT)
            .read_to_string()
            .with_context(|| format!("reading the response to {what}"))
    }

    fn get_json<T: serde::de::DeserializeOwned>(&self, path: &str, query: &[(&str, String)]) -> Result<T> {
        let text = self.get_text(path, query)?;
        serde_json::from_str(&text).with_context(|| format!("parsing the response to GET {path}"))
    }

    /// POST/PUT/PATCH with a JSON body; returns the raw response text (empty for 204).
    fn send_json(&self, method: &str, path: &str, body: &serde_json::Value) -> Result<String> {
        let url = self.url(path);
        let req = match method {
            "POST" => self.agent.post(&url),
            "PUT" => self.agent.put(&url),
            "PATCH" => self.agent.patch(&url),
            other => bail!("unsupported method {other}"),
        };
        let what = format!("{method} {path}");
        let mut resp = self
            .auth(req)
            .header("content-type", "application/json")
            .send(body.to_string().as_bytes())
            .map_err(|e| transport_error(e, &what))?;
        resp.body_mut()
            .with_config()
            .limit(BODY_LIMIT)
            .read_to_string()
            .with_context(|| format!("reading the response to {what}"))
    }

    fn send_note(&self, method: &str, path: &str, body: &serde_json::Value) -> Result<Note> {
        let text = self.send_json(method, path, body)?;
        serde_json::from_str(&text).with_context(|| format!("parsing the response to {method} {path}"))
    }
}

// ---- The API the CLI and the MCP tools are written against -----------------------------------

/// What both the real [`Remote`] and test doubles provide. Object-safe on purpose: the MCP
/// dispatcher holds a `&dyn NotesApi` so it can be driven without a server.
pub trait NotesApi {
    fn vaults(&self) -> Result<Vec<VaultSummary>>;
    fn notes(&self, vault: &str) -> Result<Vec<NoteSummary>>;
    fn note(&self, vault: &str, id: &str) -> Result<Note>;
    fn create(&self, vault: &str, path: &str, content: &str) -> Result<Note>;
    fn replace(&self, vault: &str, id: &str, content: &str) -> Result<Note>;
    fn rename(&self, vault: &str, id: &str, path: &str) -> Result<()>;
    fn delete(&self, vault: &str, id: &str) -> Result<()>;
    fn daily(&self, vault: &str, date: &str) -> Result<Note>;
    fn search(&self, vault: &str, q: &str, limit: u32) -> Result<Vec<SearchHit>>;
    fn backlinks(&self, vault: &str, id: &str) -> Result<Vec<NoteSummary>>;
    fn tags(&self, vault: &str) -> Result<Vec<TagCount>>;
    fn tagged(&self, vault: &str, tag: &str) -> Result<Vec<NoteSummary>>;

    /// Look a note up by ULID or by vault-relative path (with or without the `.md`).
    /// `Ok(None)` means "no such note", which is not an error for `write_note`.
    fn lookup_note(&self, vault: &str, path_or_id: &str) -> Result<Option<NoteSummary>> {
        let key = path_or_id.trim();
        if key.is_empty() {
            bail!("empty note path or id");
        }
        if key.parse::<NoteId>().is_ok() {
            return match self.note(vault, key) {
                Ok(n) => Ok(Some(n.summary())),
                Err(e) if is_not_found(&e) => Ok(None),
                Err(e) => Err(e),
            };
        }
        let wanted = normalize_path(key);
        let mut notes = self.notes(vault)?;
        notes.sort_by(|a, b| a.path.cmp(&b.path));
        let exact = notes.iter().find(|n| n.path == wanted);
        let ci = || notes.iter().find(|n| n.path.eq_ignore_ascii_case(&wanted));
        // A bare file name matches anywhere in the tree, the way the quick switcher does.
        let leaf = || {
            (!wanted.contains('/'))
                .then(|| notes.iter().find(|n| n.path.rsplit('/').next() == Some(wanted.as_str())))
                .flatten()
        };
        Ok(exact.or_else(ci).or_else(leaf).cloned())
    }

    /// Like [`NotesApi::lookup_note`], but "no such note" is an error.
    fn resolve_note(&self, vault: &str, path_or_id: &str) -> Result<NoteSummary> {
        self.lookup_note(vault, path_or_id)?
            .ok_or_else(|| anyhow!("no note matching {path_or_id:?} in vault {vault}"))
    }
}

/// Vault-relative path as the server stores it: no leading slash, `.md` unless already
/// `.md`/`.qmd` (mirrors the server's own normalisation).
pub fn normalize_path(path: &str) -> String {
    let p = path.trim().trim_start_matches("./").trim_start_matches('/');
    if p.ends_with(".md") || p.ends_with(".qmd") { p.to_owned() } else { format!("{p}.md") }
}

/// The vault to work in: the one given, else the account's only vault.
pub fn resolve_vault(api: &dyn NotesApi, arg: Option<&str>) -> Result<String> {
    if let Some(v) = arg.map(str::trim).filter(|v| !v.is_empty()) {
        v.parse::<VaultId>().with_context(|| format!("--vault {v:?} is not a vault id"))?;
        return Ok(v.to_owned());
    }
    let vaults = api.vaults()?;
    match vaults.as_slice() {
        [only] => Ok(only.id.clone()),
        [] => bail!("this account has no vaults yet — create one with `notes sync`"),
        many => {
            let list =
                many.iter().map(|v| format!("  {} ({} notes)", v.id, v.notes)).collect::<Vec<_>>().join("\n");
            bail!("several vaults are available; pass --vault (or set NOTES_VAULT):\n{list}")
        }
    }
}

impl NotesApi for Remote {
    fn vaults(&self) -> Result<Vec<VaultSummary>> {
        self.get_json("/vaults", &[])
    }

    fn notes(&self, vault: &str) -> Result<Vec<NoteSummary>> {
        self.get_json(&format!("/vaults/{vault}/notes"), &[])
    }

    fn note(&self, vault: &str, id: &str) -> Result<Note> {
        self.get_json(&format!("/vaults/{vault}/notes/{id}"), &[])
    }

    fn create(&self, vault: &str, path: &str, content: &str) -> Result<Note> {
        let body = serde_json::json!({ "path": normalize_path(path), "content": content });
        self.send_note("POST", &format!("/vaults/{vault}/notes"), &body)
    }

    fn replace(&self, vault: &str, id: &str, content: &str) -> Result<Note> {
        let body = serde_json::json!({ "content": content });
        self.send_note("PUT", &format!("/vaults/{vault}/notes/{id}"), &body)
    }

    fn rename(&self, vault: &str, id: &str, path: &str) -> Result<()> {
        let body = serde_json::json!({ "path": normalize_path(path) });
        self.send_json("PATCH", &format!("/vaults/{vault}/notes/{id}"), &body).map(|_| ())
    }

    fn delete(&self, vault: &str, id: &str) -> Result<()> {
        let path = format!("/vaults/{vault}/notes/{id}");
        let what = format!("DELETE {path}");
        self.auth(self.agent.delete(self.url(&path))).call().map_err(|e| transport_error(e, &what))?;
        Ok(())
    }

    fn daily(&self, vault: &str, date: &str) -> Result<Note> {
        self.get_json(&format!("/vaults/{vault}/daily/{date}"), &[])
    }

    fn search(&self, vault: &str, q: &str, limit: u32) -> Result<Vec<SearchHit>> {
        let query = [("q", q.to_owned()), ("limit", limit.clamp(1, 100).to_string())];
        self.get_json(&format!("/vaults/{vault}/search"), &query)
    }

    fn backlinks(&self, vault: &str, id: &str) -> Result<Vec<NoteSummary>> {
        self.get_json(&format!("/vaults/{vault}/notes/{id}/backlinks"), &[])
    }

    fn tags(&self, vault: &str) -> Result<Vec<TagCount>> {
        self.get_json(&format!("/vaults/{vault}/tags"), &[])
    }

    fn tagged(&self, vault: &str, tag: &str) -> Result<Vec<NoteSummary>> {
        self.get_json(&format!("/vaults/{vault}/tagged"), &[("tag", tag.to_owned())])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_normalised_like_the_server_does() {
        assert_eq!(normalize_path("Projects/plan"), "Projects/plan.md");
        assert_eq!(normalize_path("/Projects/plan.md"), "Projects/plan.md");
        assert_eq!(normalize_path("./notes.qmd"), "notes.qmd");
        assert_eq!(normalize_path("  spaced out  "), "spaced out.md");
    }

    #[test]
    fn http_errors_explain_themselves_and_keep_the_status() {
        let e = anyhow::Error::from(HttpError { status: 401, request: "GET /vaults".into() });
        assert_eq!(status_of(&e), Some(401));
        assert!(e.to_string().contains("notes login"), "{e}");
        let e = anyhow::Error::from(HttpError { status: 404, request: "GET /vaults/x/notes/y".into() });
        assert!(is_not_found(&e));
        assert!(e.to_string().contains("not found or no access"), "{e}");
    }

    #[test]
    fn a_bad_server_url_is_rejected_before_any_request() {
        assert!(Remote::from_args("notes.example.org", None, None).is_err());
        assert!(Remote::from_args("http://127.0.0.1:1/", None, None).is_ok());
    }
}

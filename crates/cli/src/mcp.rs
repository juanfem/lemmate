//! `notes mcp` — a Model Context Protocol server over stdio (SPEC §13.3).
//!
//! JSON-RPC 2.0, one message per line on stdin/stdout; **stdout carries protocol traffic only**,
//! everything else goes to stderr. The dispatcher is a pure function over `serde_json::Value`
//! parameterised by [`NotesApi`], so it is exercised in tests without a server.

use std::io::{BufRead, Write};

use anyhow::{Result, bail};
use serde_json::{Map, Value, json};

use crate::remote::{NotesApi, is_not_found};

/// Protocol revisions we know how to speak; the newest is the default.
pub const SUPPORTED_PROTOCOLS: [&str; 3] = ["2024-11-05", "2025-03-26", "2025-06-18"];
pub const DEFAULT_PROTOCOL: &str = "2025-06-18";

const TOOL_NAMES: [&str; 8] = [
    "search_notes",
    "read_note",
    "write_note",
    "append_to_note",
    "list_notes",
    "get_daily_note",
    "get_backlinks",
    "list_tags",
];

/// Today in the machine's own time zone, `YYYY-MM-DD` — what a person means by "the daily note".
pub fn today() -> String {
    jiff::Zoned::now().date().to_string()
}

/// Cheap `YYYY-MM-DD` check, so a bad date never reaches the server.
pub fn valid_date(s: &str) -> bool {
    s.len() == 10
        && s.as_bytes()[4] == b'-'
        && s.as_bytes()[7] == b'-'
        && s.bytes().enumerate().all(|(i, b)| if i == 4 || i == 7 { b == b'-' } else { b.is_ascii_digit() })
}

// ---- JSON-RPC plumbing -------------------------------------------------------------------------

fn result(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn error(id: Value, code: i64, message: impl Into<String>) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message.into() } })
}

fn text_result(text: impl Into<String>, is_error: bool) -> Value {
    json!({ "content": [{ "type": "text", "text": text.into() }], "isError": is_error })
}

/// Percent-encode a note path for a `note://` URI, keeping the separators readable.
fn encode_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for b in path.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn decode_path(path: &str) -> String {
    let bytes = path.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(b) = u8::from_str_radix(&path[i + 1..i + 3], 16)
        {
            out.push(b);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn str_arg<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    match args.get(key).and_then(Value::as_str).map(str::trim) {
        Some(s) if !s.is_empty() => Ok(s),
        _ => bail!("missing required argument {key:?}"),
    }
}

// ---- The server ---------------------------------------------------------------------------------

/// One MCP session: a vault plus the client that talks to the server.
pub struct Server<'a> {
    api: &'a dyn NotesApi,
    vault: String,
}

impl<'a> Server<'a> {
    pub fn new(api: &'a dyn NotesApi, vault: impl Into<String>) -> Self {
        Self { api, vault: vault.into() }
    }

    pub fn vault(&self) -> &str {
        &self.vault
    }

    /// One line of input → the message to write back, or `None` when nothing is owed
    /// (notifications, blank lines).
    pub fn handle_line(&self, line: &str) -> Option<Value> {
        let line = line.trim();
        if line.is_empty() {
            return None;
        }
        match serde_json::from_str::<Value>(line) {
            Ok(msg) => self.dispatch(&msg),
            Err(e) => Some(error(Value::Null, -32700, format!("parse error: {e}"))),
        }
    }

    /// Dispatch one message, or a batch of them (JSON-RPC 2.0 / MCP 2025-03-26).
    pub fn dispatch(&self, msg: &Value) -> Option<Value> {
        if let Some(batch) = msg.as_array() {
            if batch.is_empty() {
                return Some(error(Value::Null, -32600, "invalid request: empty batch"));
            }
            let out: Vec<Value> = batch.iter().filter_map(|m| self.dispatch_one(m)).collect();
            return (!out.is_empty()).then_some(Value::Array(out));
        }
        self.dispatch_one(msg)
    }

    fn dispatch_one(&self, msg: &Value) -> Option<Value> {
        let id = msg.get("id").cloned();
        let method = msg.get("method").and_then(Value::as_str);
        let Some(method) = method else {
            // A response or junk: only answer if it claimed to be a request.
            return id.map(|id| error(id, -32600, "invalid request: no method"));
        };
        // Notifications get no answer at all, not even for unknown methods (JSON-RPC 2.0).
        let id = id?;
        let params = msg.get("params").cloned().unwrap_or_else(|| json!({}));
        match self.call(method, &params) {
            Ok(value) => Some(result(id, value)),
            Err((code, message)) => Some(error(id, code, message)),
        }
    }

    /// The method table. `Err` is a JSON-RPC error; failures *inside* a tool are `Ok` results
    /// with `isError: true`, which is what MCP clients expect.
    fn call(&self, method: &str, params: &Value) -> std::result::Result<Value, (i64, String)> {
        match method {
            "initialize" => {
                let asked = params.get("protocolVersion").and_then(Value::as_str).unwrap_or_default();
                let version = if SUPPORTED_PROTOCOLS.contains(&asked) { asked } else { DEFAULT_PROTOCOL };
                Ok(json!({
                    "protocolVersion": version,
                    "capabilities": { "tools": {}, "resources": {} },
                    "serverInfo": { "name": "notes", "version": env!("CARGO_PKG_VERSION") },
                }))
            }
            "ping" => Ok(json!({})),
            // Only reached when a client sends a notification *as* a request; be forgiving.
            m if m.starts_with("notifications/") => Ok(json!({})),
            "tools/list" => Ok(json!({ "tools": tools() })),
            "tools/call" => self.tools_call(params),
            "resources/list" => self.resources_list(),
            "resources/read" => self.resources_read(params),
            other => Err((-32601, format!("method not found: {other}"))),
        }
    }

    fn tools_call(&self, params: &Value) -> std::result::Result<Value, (i64, String)> {
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .ok_or((-32602, "tools/call needs a tool name".to_owned()))?;
        if !TOOL_NAMES.contains(&name) {
            return Err((-32602, format!("unknown tool: {name}")));
        }
        let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
        Ok(match self.run_tool(name, &args) {
            Ok(text) => text_result(text, false),
            Err(e) => text_result(format!("{e:#}"), true),
        })
    }

    fn run_tool(&self, name: &str, args: &Value) -> Result<String> {
        let v = self.vault.as_str();
        match name {
            "search_notes" => {
                let query = str_arg(args, "query")?;
                let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(20).clamp(1, 100) as u32;
                let hits = self.api.search(v, query, limit)?;
                if hits.is_empty() {
                    return Ok(format!("No notes match {query:?}."));
                }
                let paths = self.path_index()?;
                let mut out = format!("{} note(s) match {query:?}:\n", hits.len());
                for h in &hits {
                    let path = paths.get(&h.note_id).cloned().unwrap_or_else(|| h.note_id.clone());
                    let snippet = h.snippet.replace('\n', " ");
                    out.push_str(&format!("- {path} ({})\n  {}\n", h.note_id, snippet.trim()));
                }
                Ok(out)
            }
            "read_note" => {
                let target = str_arg(args, "path_or_id")?;
                let found = self.api.resolve_note(v, target)?;
                Ok(self.api.note(v, &found.id)?.content)
            }
            "write_note" => {
                let target = str_arg(args, "path_or_id")?;
                let content = args.get("content").and_then(Value::as_str).unwrap_or_default();
                let note = match self.api.lookup_note(v, target)? {
                    Some(found) => self.api.replace(v, &found.id, content)?,
                    None => {
                        if target.parse::<notes_core::NoteId>().is_ok() {
                            bail!("no note with id {target} in this vault");
                        }
                        self.api.create(v, target, content)?
                    }
                };
                Ok(format!("Wrote {} ({}).", note.path, note.id))
            }
            "append_to_note" => {
                let target = str_arg(args, "path_or_id")?;
                let addition = args.get("content").and_then(Value::as_str).unwrap_or_default();
                let note = match self.api.lookup_note(v, target)? {
                    Some(found) => {
                        let current = self.api.note(v, &found.id)?.content;
                        let mut text = current;
                        if !text.is_empty() && !text.ends_with('\n') {
                            text.push('\n');
                        }
                        text.push_str(addition);
                        if !text.ends_with('\n') {
                            text.push('\n');
                        }
                        self.api.replace(v, &found.id, &text)?
                    }
                    None => {
                        if target.parse::<notes_core::NoteId>().is_ok() {
                            bail!("no note with id {target} in this vault");
                        }
                        self.api.create(v, target, addition)?
                    }
                };
                Ok(format!("Appended to {} ({}).", note.path, note.id))
            }
            "list_notes" => {
                let folder = args
                    .get("folder")
                    .and_then(Value::as_str)
                    .map(|f| f.trim().trim_start_matches('/').trim_end_matches('/'))
                    .filter(|f| !f.is_empty());
                let mut notes = self.api.notes(v)?;
                if let Some(folder) = folder {
                    let prefix = format!("{folder}/");
                    notes.retain(|n| n.path.starts_with(&prefix));
                }
                notes.sort_by(|a, b| a.path.cmp(&b.path));
                if notes.is_empty() {
                    return Ok(match folder {
                        Some(f) => format!("No notes under {f}/."),
                        None => "This vault has no notes yet.".to_owned(),
                    });
                }
                let mut out = format!("{} note(s):\n", notes.len());
                for n in &notes {
                    out.push_str(&format!("- {} ({})\n", n.path, n.id));
                }
                Ok(out)
            }
            "get_daily_note" => {
                let date = args
                    .get("date")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|d| !d.is_empty())
                    .map(str::to_owned)
                    .unwrap_or_else(today);
                if !valid_date(&date) {
                    bail!("date must be YYYY-MM-DD (got {date:?})");
                }
                Ok(self.api.daily(v, &date)?.content)
            }
            "get_backlinks" => {
                let target = str_arg(args, "path_or_id")?;
                let found = self.api.resolve_note(v, target)?;
                let links = self.api.backlinks(v, &found.id)?;
                if links.is_empty() {
                    return Ok(format!("Nothing links to {}.", found.path));
                }
                let mut out = format!("{} note(s) link to {}:\n", links.len(), found.path);
                for n in &links {
                    out.push_str(&format!("- {} ({})\n", n.path, n.id));
                }
                Ok(out)
            }
            "list_tags" => {
                let mut tags = self.api.tags(v)?;
                if tags.is_empty() {
                    return Ok("This vault has no tags yet.".to_owned());
                }
                tags.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.tag.cmp(&b.tag)));
                let mut out = format!("{} tag(s):\n", tags.len());
                for t in &tags {
                    out.push_str(&format!("- #{} ({})\n", t.tag, t.count));
                }
                Ok(out)
            }
            other => bail!("unknown tool: {other}"),
        }
    }

    fn path_index(&self) -> Result<std::collections::HashMap<String, String>> {
        Ok(self.api.notes(&self.vault)?.into_iter().map(|n| (n.id, n.path)).collect())
    }

    fn resources_list(&self) -> std::result::Result<Value, (i64, String)> {
        let mut notes = self.api.notes(&self.vault).map_err(internal)?;
        notes.sort_by(|a, b| a.path.cmp(&b.path));
        let resources: Vec<Value> = notes
            .iter()
            .map(|n| {
                json!({
                    "uri": format!("note://{}/{}", self.vault, encode_path(&n.path)),
                    "name": n.title.clone().unwrap_or_else(|| n.path.clone()),
                    "description": n.path,
                    "mimeType": "text/markdown",
                })
            })
            .collect();
        Ok(json!({ "resources": resources }))
    }

    fn resources_read(&self, params: &Value) -> std::result::Result<Value, (i64, String)> {
        let uri = params
            .get("uri")
            .and_then(Value::as_str)
            .ok_or((-32602, "resources/read needs a uri".to_owned()))?;
        let rest =
            uri.strip_prefix("note://").ok_or_else(|| (-32602, format!("not a note:// uri: {uri}")))?;
        let (vault, path) =
            rest.split_once('/').ok_or_else(|| (-32602, format!("uri has no note path: {uri}")))?;
        if vault != self.vault {
            return Err((-32602, format!("uri names vault {vault}, this server serves {}", self.vault)));
        }
        let path = decode_path(path);
        let found = self.api.resolve_note(&self.vault, &path).map_err(|e| {
            if is_not_found(&e) { (-32002, format!("resource not found: {uri}")) } else { internal(e) }
        })?;
        let note = self.api.note(&self.vault, &found.id).map_err(internal)?;
        Ok(json!({
            "contents": [{
                "uri": uri,
                "mimeType": "text/markdown",
                "text": note.content,
            }],
        }))
    }
}

fn internal(e: anyhow::Error) -> (i64, String) {
    (-32603, format!("{e:#}"))
}

// ---- Tool catalogue ------------------------------------------------------------------------------

fn schema(props: Value, required: &[&str]) -> Value {
    let mut map = Map::new();
    map.insert("type".into(), json!("object"));
    map.insert("properties".into(), props);
    map.insert("required".into(), json!(required));
    map.insert("additionalProperties".into(), json!(false));
    Value::Object(map)
}

fn tool(name: &str, description: &str, input_schema: Value) -> Value {
    json!({ "name": name, "description": description, "inputSchema": input_schema })
}

const NOTE_REF: &str =
    "The note, either its vault-relative path (`Projects/plan.md`, the `.md` optional) or its ULID.";

/// The eight tools of SPEC §13.3, in the order the spec lists them.
pub fn tools() -> Vec<Value> {
    vec![
        tool(
            "search_notes",
            "Full-text search across the vault. Returns the matching notes' paths, ids, and a \
             snippet each. Use it to find notes before reading them.",
            schema(
                json!({
                    "query": { "type": "string", "description": "Full-text query, e.g. `roadmap` or `tax invoice`." },
                    "limit": { "type": "integer", "description": "Maximum number of hits (1-100, default 20).", "minimum": 1, "maximum": 100 },
                }),
                &["query"],
            ),
        ),
        tool(
            "read_note",
            "Read one note's full markdown source, front matter included.",
            schema(json!({ "path_or_id": { "type": "string", "description": NOTE_REF } }), &["path_or_id"]),
        ),
        tool(
            "write_note",
            "Replace a note's entire content, creating the note when the path does not exist yet. \
             The server applies the new text as a diff, so concurrent editors are merged rather \
             than clobbered. To add to a note without resending it, use `append_to_note`.",
            schema(
                json!({
                    "path_or_id": { "type": "string", "description": NOTE_REF },
                    "content": { "type": "string", "description": "The complete new markdown content of the note." },
                }),
                &["path_or_id", "content"],
            ),
        ),
        tool(
            "append_to_note",
            "Append markdown to the end of a note, keeping what is already there (a newline is \
             inserted if needed). Creates the note when the path does not exist yet.",
            schema(
                json!({
                    "path_or_id": { "type": "string", "description": NOTE_REF },
                    "content": { "type": "string", "description": "Markdown to add at the end of the note." },
                }),
                &["path_or_id", "content"],
            ),
        ),
        tool(
            "list_notes",
            "List the notes in the vault as `path (id)` lines, optionally only those in one folder.",
            schema(
                json!({
                    "folder": { "type": "string", "description": "Vault-relative folder to list, e.g. `Projects`. Omit for the whole vault." },
                }),
                &[],
            ),
        ),
        tool(
            "get_daily_note",
            "Get the daily note for a date (today by default), creating `Daily/<date>.md` if it \
             does not exist yet. Returns its markdown content.",
            schema(
                json!({
                    "date": { "type": "string", "description": "Calendar date as YYYY-MM-DD. Defaults to today.", "pattern": "^\\d{4}-\\d{2}-\\d{2}$" },
                }),
                &[],
            ),
        ),
        tool(
            "get_backlinks",
            "List the notes that link to a note ([[wikilinks]] and markdown links alike).",
            schema(json!({ "path_or_id": { "type": "string", "description": NOTE_REF } }), &["path_or_id"]),
        ),
        tool(
            "list_tags",
            "List every tag used in the vault with how many notes carry it, most used first.",
            schema(json!({}), &[]),
        ),
    ]
}

// ---- stdio loop --------------------------------------------------------------------------------

/// Serve MCP on stdin/stdout until end of input.
pub fn serve_stdio(api: &dyn NotesApi, vault: String) -> Result<()> {
    let server = Server::new(api, vault);
    eprintln!("notes mcp: serving vault {} (protocol {DEFAULT_PROTOCOL})", server.vault());
    let mut stdin = std::io::stdin().lock();
    let mut stdout = std::io::stdout().lock();
    let mut line = String::new();
    loop {
        line.clear();
        if stdin.read_line(&mut line)? == 0 {
            return Ok(());
        }
        if let Some(response) = server.handle_line(&line) {
            serde_json::to_writer(&mut stdout, &response)?;
            stdout.write_all(b"\n")?;
            stdout.flush()?;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::{Note, NoteSummary, SearchHit, TagCount, VaultSummary, normalize_path};
    use std::cell::RefCell;

    /// An in-memory vault, enough to drive the dispatcher without a server.
    #[derive(Default)]
    struct Fake {
        notes: RefCell<Vec<Note>>,
    }

    impl Fake {
        fn with(paths: &[(&str, &str, &str)]) -> Self {
            let notes = paths
                .iter()
                .map(|(id, path, content)| Note {
                    id: (*id).to_owned(),
                    path: (*path).to_owned(),
                    title: None,
                    content: (*content).to_owned(),
                })
                .collect();
            Self { notes: RefCell::new(notes) }
        }
    }

    impl NotesApi for Fake {
        fn vaults(&self) -> Result<Vec<VaultSummary>> {
            Ok(vec![VaultSummary { id: "V".into(), notes: self.notes.borrow().len() as u32 }])
        }
        fn notes(&self, _v: &str) -> Result<Vec<NoteSummary>> {
            Ok(self.notes.borrow().iter().map(Note::summary).collect())
        }
        fn note(&self, _v: &str, id: &str) -> Result<Note> {
            self.notes.borrow().iter().find(|n| n.id == id).cloned().ok_or_else(|| anyhow::anyhow!("gone"))
        }
        fn create(&self, _v: &str, path: &str, content: &str) -> Result<Note> {
            let note = Note {
                id: format!("N{}", self.notes.borrow().len()),
                path: normalize_path(path),
                title: None,
                content: content.to_owned(),
            };
            self.notes.borrow_mut().push(note.clone());
            Ok(note)
        }
        fn replace(&self, _v: &str, id: &str, content: &str) -> Result<Note> {
            let mut notes = self.notes.borrow_mut();
            let note = notes.iter_mut().find(|n| n.id == id).ok_or_else(|| anyhow::anyhow!("gone"))?;
            note.content = content.to_owned();
            Ok(note.clone())
        }
        fn rename(&self, _v: &str, _id: &str, _path: &str) -> Result<()> {
            Ok(())
        }
        fn delete(&self, _v: &str, _id: &str) -> Result<()> {
            Ok(())
        }
        fn daily(&self, v: &str, date: &str) -> Result<Note> {
            let path = format!("Daily/{date}.md");
            if let Some(n) = self.notes.borrow().iter().find(|n| n.path == path) {
                return Ok(n.clone());
            }
            self.create(v, &path, &format!("# {date}\n\n"))
        }
        fn search(&self, _v: &str, q: &str, _limit: u32) -> Result<Vec<SearchHit>> {
            Ok(self
                .notes
                .borrow()
                .iter()
                .filter(|n| n.content.contains(q))
                .map(|n| SearchHit {
                    note_id: n.id.clone(),
                    title: None,
                    snippet: n.content.lines().next().unwrap_or_default().to_owned(),
                })
                .collect())
        }
        fn backlinks(&self, _v: &str, _id: &str) -> Result<Vec<NoteSummary>> {
            Ok(vec![])
        }
        fn tags(&self, _v: &str) -> Result<Vec<TagCount>> {
            Ok(vec![TagCount { tag: "project".into(), count: 2 }])
        }
        fn tagged(&self, _v: &str, _tag: &str) -> Result<Vec<NoteSummary>> {
            Ok(vec![])
        }
    }

    fn ask(server: &Server<'_>, msg: Value) -> Value {
        server.dispatch(&msg).expect("expected a response")
    }

    #[test]
    fn initialize_echoes_a_known_protocol_and_falls_back_otherwise() {
        let api = Fake::default();
        let s = Server::new(&api, "V");
        let r = ask(
            &s,
            json!({"jsonrpc":"2.0","id":1,"method":"initialize",
                   "params":{"protocolVersion":"2024-11-05","clientInfo":{"name":"t","version":"1"}}}),
        );
        assert_eq!(r["jsonrpc"], "2.0");
        assert_eq!(r["id"], 1);
        assert_eq!(r["result"]["protocolVersion"], "2024-11-05");
        assert_eq!(r["result"]["serverInfo"]["name"], "notes");
        assert_eq!(r["result"]["serverInfo"]["version"], env!("CARGO_PKG_VERSION"));
        assert!(r["result"]["capabilities"]["tools"].is_object());
        assert!(r["result"]["capabilities"]["resources"].is_object());

        let r = ask(
            &s,
            json!({"jsonrpc":"2.0","id":2,"method":"initialize","params":{"protocolVersion":"1999-01-01"}}),
        );
        assert_eq!(r["result"]["protocolVersion"], DEFAULT_PROTOCOL);
    }

    #[test]
    fn tools_list_advertises_the_eight_spec_tools_with_schemas() {
        let api = Fake::default();
        let s = Server::new(&api, "V");
        let r = ask(&s, json!({"jsonrpc":"2.0","id":"t","method":"tools/list"}));
        let tools = r["result"]["tools"].as_array().expect("tools array");
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(names, TOOL_NAMES);
        for t in tools {
            assert!(t["description"].as_str().is_some_and(|d| d.len() > 20), "{t}");
            assert_eq!(t["inputSchema"]["type"], "object", "{t}");
            assert!(t["inputSchema"]["properties"].is_object(), "{t}");
        }
    }

    #[test]
    fn unknown_methods_are_method_not_found() {
        let api = Fake::default();
        let s = Server::new(&api, "V");
        let r = ask(&s, json!({"jsonrpc":"2.0","id":9,"method":"nope/what"}));
        assert_eq!(r["error"]["code"], -32601);
        assert!(r["error"]["message"].as_str().unwrap().contains("nope/what"));
        assert!(r.get("result").is_none());
    }

    #[test]
    fn notifications_are_never_answered() {
        let api = Fake::default();
        let s = Server::new(&api, "V");
        assert_eq!(s.dispatch(&json!({"jsonrpc":"2.0","method":"notifications/initialized"})), None);
        // Not even unknown ones (JSON-RPC 2.0 §4.1).
        assert_eq!(s.dispatch(&json!({"jsonrpc":"2.0","method":"nope"})), None);
        assert_eq!(s.handle_line("   "), None);
    }

    #[test]
    fn malformed_input_is_a_parse_error() {
        let api = Fake::default();
        let s = Server::new(&api, "V");
        let r = s.handle_line("{not json").expect("a parse error is reported");
        assert_eq!(r["jsonrpc"], "2.0");
        assert_eq!(r["id"], Value::Null);
        assert_eq!(r["error"]["code"], -32700);
        // A well-formed value that is not a request:
        let r = ask(&s, json!({"jsonrpc":"2.0","id":3,"params":{}}));
        assert_eq!(r["error"]["code"], -32600);
    }

    #[test]
    fn ping_answers_an_empty_object() {
        let api = Fake::default();
        let s = Server::new(&api, "V");
        let r = ask(&s, json!({"jsonrpc":"2.0","id":4,"method":"ping"}));
        assert_eq!(r["result"], json!({}));
    }

    #[test]
    fn tool_calls_round_trip_through_the_api() {
        let api = Fake::with(&[("N0", "Projects/plan.md", "# Plan\n\nstep one\n")]);
        let s = Server::new(&api, "V");

        let call = |name: &str, args: Value| {
            ask(
                &s,
                json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
                           "params":{"name":name,"arguments":args}}),
            )
        };

        let r = call("read_note", json!({ "path_or_id": "Projects/plan" }));
        assert_eq!(r["result"]["isError"], false);
        assert_eq!(r["result"]["content"][0]["type"], "text");
        assert_eq!(r["result"]["content"][0]["text"], "# Plan\n\nstep one\n");

        let r = call("write_note", json!({ "path_or_id": "Ideas/new", "content": "# New\n" }));
        assert_eq!(r["result"]["isError"], false);
        assert_eq!(api.notes.borrow().len(), 2);
        assert_eq!(api.notes.borrow()[1].path, "Ideas/new.md");

        let r = call("append_to_note", json!({ "path_or_id": "Ideas/new.md", "content": "more" }));
        assert_eq!(r["result"]["isError"], false);
        assert_eq!(api.notes.borrow()[1].content, "# New\nmore\n");

        let r = call("list_notes", json!({ "folder": "Projects" }));
        let text = r["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("Projects/plan.md"), "{text}");
        assert!(!text.contains("Ideas/"), "{text}");

        let r = call("get_daily_note", json!({ "date": "2026-01-02" }));
        assert_eq!(r["result"]["content"][0]["text"], "# 2026-01-02\n\n");

        let r = call("list_tags", json!({}));
        assert!(r["result"]["content"][0]["text"].as_str().unwrap().contains("#project (2)"));

        // Failures inside a tool are results, not JSON-RPC errors.
        let r = call("read_note", json!({ "path_or_id": "nope.md" }));
        assert_eq!(r["result"]["isError"], true);
        assert!(r["result"]["content"][0]["text"].as_str().unwrap().contains("nope.md"));
        let r = call("read_note", json!({}));
        assert_eq!(r["result"]["isError"], true);
        // An unknown tool is a protocol error.
        let r = call("frobnicate", json!({}));
        assert_eq!(r["error"]["code"], -32602);
    }

    #[test]
    fn resources_expose_one_note_uri_each() {
        let api = Fake::with(&[("N0", "Projects/my plan.md", "hello\n")]);
        let s = Server::new(&api, "V");
        let r = ask(&s, json!({"jsonrpc":"2.0","id":1,"method":"resources/list"}));
        let list = r["result"]["resources"].as_array().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0]["uri"], "note://V/Projects/my%20plan.md");
        assert_eq!(list[0]["mimeType"], "text/markdown");

        let uri = list[0]["uri"].clone();
        let r = ask(&s, json!({"jsonrpc":"2.0","id":2,"method":"resources/read","params":{"uri":uri}}));
        assert_eq!(r["result"]["contents"][0]["text"], "hello\n");
        assert_eq!(r["result"]["contents"][0]["mimeType"], "text/markdown");

        let r =
            ask(&s, json!({"jsonrpc":"2.0","id":3,"method":"resources/read","params":{"uri":"http://x/y"}}));
        assert_eq!(r["error"]["code"], -32602);
    }

    #[test]
    fn batches_answer_only_the_requests() {
        let api = Fake::default();
        let s = Server::new(&api, "V");
        let r = ask(
            &s,
            json!([
                {"jsonrpc":"2.0","method":"notifications/initialized"},
                {"jsonrpc":"2.0","id":1,"method":"ping"},
            ]),
        );
        assert_eq!(r.as_array().unwrap().len(), 1);
        assert_eq!(r[0]["id"], 1);
        assert_eq!(
            s.dispatch(&json!([{"jsonrpc":"2.0","method":"notifications/initialized"}])),
            None,
            "a batch of notifications owes nothing"
        );
    }

    #[test]
    fn dates_are_validated_before_they_reach_the_server() {
        assert!(valid_date("2026-08-30"));
        assert!(!valid_date("2026-8-30"));
        assert!(!valid_date("today"));
        assert!(valid_date(&today()));
    }

    #[test]
    fn note_uris_percent_round_trip() {
        for path in ["a/b.md", "My Notes/über #1.md", "plain.md"] {
            assert_eq!(decode_path(&encode_path(path)), path);
        }
    }
}

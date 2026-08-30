//! SQLite persistence (SPEC §6.1 / §6.2). One schema serves both the server (`notes.db`) and
//! native clients (`<vault>/.notes/local.db`); clients simply leave the multi-user tables empty.

use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, params};

use crate::doc::NoteDoc;
use crate::error::Result;
use crate::ids::{DocId, NoteId, VaultId};
use crate::markdown::NoteIndex;
use crate::vault_doc::VaultDoc;

pub const SCHEMA_VERSION: u32 = 7;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS doc_updates (
    doc_id     TEXT    NOT NULL,
    seq        INTEGER NOT NULL,
    bytes      BLOB    NOT NULL,
    author_id  TEXT,
    created_ms INTEGER NOT NULL,
    PRIMARY KEY (doc_id, seq)
);
CREATE TABLE IF NOT EXISTS doc_snapshots (
    doc_id     TEXT    NOT NULL,
    seq        INTEGER NOT NULL,
    bytes      BLOB    NOT NULL,
    created_ms INTEGER NOT NULL,
    label      TEXT,               -- set for user-saved versions, which are kept forever (§9)
    author_id  TEXT,
    PRIMARY KEY (doc_id, seq)
);
CREATE TABLE IF NOT EXISTS attachments (
    hash          TEXT    NOT NULL,
    vault_id      TEXT    NOT NULL,
    size          INTEGER NOT NULL,
    mime          TEXT    NOT NULL,
    filename_hint TEXT,
    created_ms    INTEGER NOT NULL,
    orphaned_ms   INTEGER,             -- when the blob was first seen unreferenced (server)
    PRIMARY KEY (vault_id, hash)
);
-- Resolved attachment paths each note references (client sidecar; drives uploads and orphans).
CREATE TABLE IF NOT EXISTS note_attachments (
    note_id TEXT NOT NULL,
    path    TEXT NOT NULL,
    PRIMARY KEY (note_id, path)
);
CREATE TABLE IF NOT EXISTS notes (
    id         TEXT PRIMARY KEY,
    vault_id   TEXT NOT NULL,
    path       TEXT NOT NULL,
    title      TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    deleted_at TEXT
);
CREATE UNIQUE INDEX IF NOT EXISTS notes_live_path ON notes (vault_id, path) WHERE deleted_at IS NULL;
CREATE TABLE IF NOT EXISTS note_tags  (note_id TEXT NOT NULL, tag TEXT NOT NULL, PRIMARY KEY (note_id, tag));
CREATE TABLE IF NOT EXISTS note_links (note_id TEXT NOT NULL, target TEXT NOT NULL, kind TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS note_links_target ON note_links (target);
CREATE VIRTUAL TABLE IF NOT EXISTS notes_fts USING fts5 (
    note_id UNINDEXED, title, body, tokenize = 'unicode61 remove_diacritics 2'
);
-- Text last written to / read from disk per doc, so external edits can be diffed against it (§6.3).
CREATE TABLE IF NOT EXISTS projection (
    doc_id TEXT PRIMARY KEY,
    path   TEXT NOT NULL,
    text   TEXT NOT NULL
);
-- Small key/value facts about this database (e.g. `vault_id` in a client sidecar).
CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
-- Accounts and access (server only; SPEC §11).
CREATE TABLE IF NOT EXISTS users (
    id            TEXT PRIMARY KEY,
    email         TEXT NOT NULL UNIQUE,
    display_name  TEXT NOT NULL,
    password_hash TEXT,
    is_admin      INTEGER NOT NULL DEFAULT 0,
    created_ms    INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS sessions (
    token_hash  TEXT PRIMARY KEY,
    user_id     TEXT NOT NULL,
    device_name TEXT,
    created_ms  INTEGER NOT NULL,
    expires_ms  INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS sessions_user ON sessions (user_id);
CREATE TABLE IF NOT EXISTS memberships (
    vault_id TEXT NOT NULL,
    user_id  TEXT NOT NULL,
    role     TEXT NOT NULL,
    PRIMARY KEY (vault_id, user_id)
);
CREATE INDEX IF NOT EXISTS memberships_user ON memberships (user_id);
-- Per-note access (SPEC §11.2): to a user, or to anyone holding a link token (hashed here).
CREATE TABLE IF NOT EXISTS note_shares (
    note_id    TEXT NOT NULL,
    user_id    TEXT,
    token_hash TEXT,
    role       TEXT NOT NULL,
    created_by TEXT,
    created_ms INTEGER NOT NULL,
    expires_ms INTEGER,
    UNIQUE (note_id, user_id),
    UNIQUE (token_hash)
);
CREATE INDEX IF NOT EXISTS note_shares_user ON note_shares (user_id);
"#;

pub struct Store {
    conn: Connection,
}

/// Snapshot and pruning policy (SPEC §6.1, §9). Defaults: snapshot every 500 updates or 10
/// minutes, keep raw updates for 90 days.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetentionPolicy {
    pub snapshot_every_updates: u32,
    pub snapshot_interval: Duration,
    pub retain_updates: Duration,
}

impl Default for RetentionPolicy {
    fn default() -> Self {
        Self {
            snapshot_every_updates: 500,
            snapshot_interval: Duration::from_secs(10 * 60),
            retain_updates: Duration::from_secs(90 * 24 * 60 * 60),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Maintenance {
    pub snapshotted: bool,
    pub pruned_updates: usize,
    pub pruned_snapshots: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteShareRow {
    pub user_id: Option<String>,
    pub email: Option<String>,
    pub token_hash: Option<String>,
    pub role: Role,
    pub expires_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionRow {
    pub seq: i64,
    pub created_ms: i64,
    pub label: Option<String>,
    pub author: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentRow {
    pub hash: String,
    pub size: u64,
    pub mime: String,
    pub filename_hint: Option<String>,
}

/// Vault-level roles (SPEC §11.2). Ordered: a higher role includes the lower ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Role {
    Viewer,
    Editor,
    Owner,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Viewer => "viewer",
            Role::Editor => "editor",
            Role::Owner => "owner",
        }
    }
    pub fn parse(s: &str) -> Option<Role> {
        match s {
            "viewer" => Some(Role::Viewer),
            "editor" => Some(Role::Editor),
            "owner" => Some(Role::Owner),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserRow {
    pub id: String,
    pub email: String,
    pub display_name: String,
    pub password_hash: Option<String>,
    pub is_admin: bool,
}

/// A parsed search query: free text for FTS5 plus structured filters (SPEC §10).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SearchQuery {
    pub text: String,
    pub tags: Vec<String>,
    pub paths: Vec<String>,
    pub has: Vec<String>,
}

impl SearchQuery {
    pub fn parse(query: &str) -> Self {
        let mut q = SearchQuery::default();
        let mut text = Vec::new();
        for tok in query.split_whitespace() {
            if let Some(t) = tok.strip_prefix("tag:") {
                q.tags.push(t.trim_start_matches('#').trim_matches('/').to_lowercase());
            } else if let Some(p) = tok.strip_prefix("path:") {
                q.paths.push(p.trim_start_matches('/').to_owned());
            } else if let Some(h) = tok.strip_prefix("has:") {
                q.has.push(h.to_lowercase());
            } else {
                text.push(tok);
            }
        }
        q.text = text.join(" ");
        q
    }
}

/// Milliseconds since the Unix epoch.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteRow {
    pub id: NoteId,
    pub vault_id: VaultId,
    pub path: String,
    pub title: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SearchHit {
    pub note_id: NoteId,
    pub title: Option<String>,
    pub snippet: String,
    pub rank: f64,
}

impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        let version: u32 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version > 0 && version < 3 {
            // Pre-release schemas (v1/v2) stored ISO timestamps; nothing shipped, so rebuild.
            conn.execute_batch("DROP TABLE IF EXISTS doc_updates; DROP TABLE IF EXISTS doc_snapshots;")?;
        }
        if version == 3 {
            conn.execute_batch("ALTER TABLE attachments ADD COLUMN orphaned_ms INTEGER;")?;
        }
        if (4..=5).contains(&version) {
            conn.execute_batch(
                "ALTER TABLE doc_snapshots ADD COLUMN label TEXT; ALTER TABLE doc_snapshots ADD COLUMN author_id TEXT;",
            )?;
        }
        if version < SCHEMA_VERSION {
            conn.execute_batch(SCHEMA)?;
            conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        }
        Ok(Self { conn })
    }

    // ---- CRDT log -------------------------------------------------------------------------

    /// Append one update to a doc's log; returns its sequence number.
    pub fn append_update(&mut self, doc_id: DocId, bytes: &[u8], author: Option<&str>) -> Result<i64> {
        self.append_update_at(doc_id, bytes, author, now_ms())
    }

    pub fn append_update_at(
        &mut self,
        doc_id: DocId,
        bytes: &[u8],
        author: Option<&str>,
        now_ms: i64,
    ) -> Result<i64> {
        let id = doc_id.to_string();
        let tx = self.conn.transaction()?;
        let seq: i64 = tx.query_row(
            "SELECT COALESCE(MAX(seq), 0) + 1 FROM (SELECT seq FROM doc_updates WHERE doc_id = ?1 UNION ALL SELECT seq FROM doc_snapshots WHERE doc_id = ?1)",
            params![id],
            |r| r.get(0),
        )?;
        tx.execute(
            "INSERT INTO doc_updates (doc_id, seq, bytes, author_id, created_ms) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, seq, bytes, author, now_ms],
        )?;
        tx.commit()?;
        Ok(seq)
    }

    /// Latest snapshot (if any) followed by the updates after it, in order.
    pub fn load_updates(&self, doc_id: DocId) -> Result<Vec<Vec<u8>>> {
        let id = doc_id.to_string();
        let snapshot: Option<(i64, Vec<u8>)> = self
            .conn
            .query_row(
                "SELECT seq, bytes FROM doc_snapshots WHERE doc_id = ?1 ORDER BY seq DESC LIMIT 1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let (from_seq, mut updates) = match snapshot {
            Some((seq, bytes)) => (seq, vec![bytes]),
            None => (0, Vec::new()),
        };
        let mut stmt =
            self.conn.prepare("SELECT bytes FROM doc_updates WHERE doc_id = ?1 AND seq > ?2 ORDER BY seq")?;
        for row in stmt.query_map(params![id, from_seq], |r| r.get::<_, Vec<u8>>(0))? {
            updates.push(row?);
        }
        Ok(updates)
    }

    /// Load a note doc from its latest snapshot plus subsequent updates. A never-seen doc is empty.
    pub fn load_doc(&self, doc_id: DocId) -> Result<NoteDoc> {
        let updates = self.load_updates(doc_id)?;
        NoteDoc::from_updates(updates.iter().map(Vec::as_slice))
    }

    pub fn load_vault_doc(&self, vault_id: VaultId) -> Result<VaultDoc> {
        let updates = self.load_updates(DocId::Vault(vault_id))?;
        VaultDoc::from_updates(updates.iter().map(Vec::as_slice))
    }

    /// Persist a doc's full state (`encode_full()`) as a snapshot at the current head.
    pub fn snapshot_at(&mut self, doc_id: DocId, full_state: &[u8], now_ms: i64) -> Result<i64> {
        self.snapshot_labeled_at(doc_id, full_state, now_ms, None, None)
    }

    /// A snapshot with a user-visible label (a *version*, SPEC §9): never pruned.
    pub fn snapshot_labeled_at(
        &mut self,
        doc_id: DocId,
        full_state: &[u8],
        now_ms: i64,
        label: Option<&str>,
        author: Option<&str>,
    ) -> Result<i64> {
        let id = doc_id.to_string();
        let head: i64 = self.conn.query_row(
            "SELECT COALESCE(MAX(seq), 0) FROM (SELECT seq FROM doc_updates WHERE doc_id = ?1 UNION ALL SELECT seq FROM doc_snapshots WHERE doc_id = ?1)",
            params![id],
            |r| r.get(0),
        )?;
        self.conn.execute(
            "INSERT OR REPLACE INTO doc_snapshots (doc_id, seq, bytes, created_ms, label, author_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![id, head, full_state, now_ms, label, author],
        )?;
        Ok(head)
    }

    /// Addressable points in a doc's history: every snapshot, newest first.
    pub fn versions(&self, doc_id: DocId) -> Result<Vec<VersionRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT seq, created_ms, label, author_id FROM doc_snapshots WHERE doc_id = ?1 ORDER BY seq DESC",
        )?;
        let rows = stmt.query_map(params![doc_id.to_string()], |r| {
            Ok(VersionRow { seq: r.get(0)?, created_ms: r.get(1)?, label: r.get(2)?, author: r.get(3)? })
        })?;
        rows.map(|r| r.map_err(Into::into)).collect()
    }

    /// The doc as it was at sequence `seq`: newest snapshot at or before it, plus the updates
    /// up to it that are still in the journal.
    pub fn load_doc_at(&self, doc_id: DocId, seq: i64) -> Result<NoteDoc> {
        let id = doc_id.to_string();
        let snapshot: Option<(i64, Vec<u8>)> = self
            .conn
            .query_row(
                "SELECT seq, bytes FROM doc_snapshots WHERE doc_id = ?1 AND seq <= ?2 ORDER BY seq DESC LIMIT 1",
                params![id, seq],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let (from_seq, mut updates) = match snapshot {
            Some((s, bytes)) => (s, vec![bytes]),
            None => (0, Vec::new()),
        };
        let mut stmt = self.conn.prepare(
            "SELECT bytes FROM doc_updates WHERE doc_id = ?1 AND seq > ?2 AND seq <= ?3 ORDER BY seq",
        )?;
        for row in stmt.query_map(params![id, from_seq, seq], |r| r.get::<_, Vec<u8>>(0))? {
            updates.push(row?);
        }
        NoteDoc::from_updates(updates.iter().map(Vec::as_slice))
    }

    /// Apply the snapshot/pruning policy (SPEC §6.1, §9) to one doc:
    ///
    /// 1. If the updates since the last snapshot number at least `snapshot_every_updates`, or the
    ///    oldest of them is older than `snapshot_interval`, write a snapshot at the head.
    /// 2. Find the newest snapshot older than `retain_updates`; every update at or before it, and
    ///    every older snapshot, is redundant for reconstructing any state within the retention
    ///    window, so delete them.
    ///
    /// `full_state` is only invoked when a snapshot is actually written.
    pub fn maintain(
        &mut self,
        doc_id: DocId,
        policy: &RetentionPolicy,
        now_ms: i64,
        full_state: impl FnOnce() -> Vec<u8>,
    ) -> Result<Maintenance> {
        let id = doc_id.to_string();
        let mut out = Maintenance::default();

        let last_snap_seq: i64 = self.conn.query_row(
            "SELECT COALESCE(MAX(seq), 0) FROM doc_snapshots WHERE doc_id = ?1",
            params![id],
            |r| r.get(0),
        )?;
        let (pending, oldest_ms): (i64, Option<i64>) = self.conn.query_row(
            "SELECT COUNT(*), MIN(created_ms) FROM doc_updates WHERE doc_id = ?1 AND seq > ?2",
            params![id, last_snap_seq],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let stale = oldest_ms.is_some_and(|t| now_ms - t >= policy.snapshot_interval.as_millis() as i64);
        if pending > 0 && (pending >= policy.snapshot_every_updates as i64 || stale) {
            self.snapshot_at(doc_id, &full_state(), now_ms)?;
            out.snapshotted = true;
        }

        let cutoff_ms = now_ms - policy.retain_updates.as_millis() as i64;
        let cut: Option<i64> = self
            .conn
            .query_row(
                "SELECT MAX(seq) FROM doc_snapshots WHERE doc_id = ?1 AND created_ms <= ?2",
                params![id, cutoff_ms],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        if let Some(cut) = cut {
            out.pruned_updates = self
                .conn
                .execute("DELETE FROM doc_updates WHERE doc_id = ?1 AND seq <= ?2", params![id, cut])?;
            out.pruned_snapshots = self.conn.execute(
                "DELETE FROM doc_snapshots WHERE doc_id = ?1 AND seq < ?2 AND label IS NULL",
                params![id, cut],
            )?;
        }
        Ok(out)
    }

    /// Every doc id that has any journal rows.
    pub fn doc_ids(&self) -> Result<Vec<DocId>> {
        let mut stmt = self.conn.prepare(
            "SELECT doc_id FROM doc_updates UNION SELECT doc_id FROM doc_snapshots ORDER BY doc_id",
        )?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        rows.map(|r| r?.parse()).collect()
    }

    // ---- Attachments ------------------------------------------------------------------------

    pub fn upsert_attachment(&mut self, vault_id: VaultId, a: &AttachmentRow) -> Result<()> {
        self.conn.execute(
            "INSERT INTO attachments (hash, vault_id, size, mime, filename_hint, created_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(vault_id, hash) DO UPDATE SET filename_hint = COALESCE(excluded.filename_hint, filename_hint)",
            params![a.hash, vault_id.to_string(), a.size as i64, a.mime, a.filename_hint, now_ms()],
        )?;
        Ok(())
    }

    /// Replace the set of attachment paths a note references.
    pub fn set_note_attachments(&mut self, note_id: NoteId, paths: &[String]) -> Result<()> {
        let id = note_id.to_string();
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM note_attachments WHERE note_id = ?1", params![id])?;
        for p in paths {
            tx.execute(
                "INSERT OR IGNORE INTO note_attachments (note_id, path) VALUES (?1, ?2)",
                params![id, p],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn clear_note_attachments(&mut self, note_id: NoteId) -> Result<()> {
        self.conn.execute("DELETE FROM note_attachments WHERE note_id = ?1", params![note_id.to_string()])?;
        Ok(())
    }

    /// Every attachment path referenced by at least one live (non-trashed) note.
    pub fn referenced_attachment_paths(&self) -> Result<std::collections::HashSet<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT a.path FROM note_attachments a JOIN notes n ON n.id = a.note_id WHERE n.deleted_at IS NULL",
        )?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        rows.map(|r| r.map_err(Into::into)).collect()
    }

    /// (hash, orphaned_ms) for every attachment row of a vault.
    pub fn attachment_hashes(&self, vault_id: VaultId) -> Result<Vec<(String, Option<i64>)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT hash, orphaned_ms FROM attachments WHERE vault_id = ?1 ORDER BY hash")?;
        let rows = stmt.query_map(params![vault_id.to_string()], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.map(|r| r.map_err(Into::into)).collect()
    }

    pub fn set_attachment_orphaned(
        &mut self,
        vault_id: VaultId,
        hash: &str,
        orphaned_ms: Option<i64>,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE attachments SET orphaned_ms = ?3 WHERE vault_id = ?1 AND hash = ?2",
            params![vault_id.to_string(), hash, orphaned_ms],
        )?;
        Ok(())
    }

    pub fn delete_attachment(&mut self, vault_id: VaultId, hash: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM attachments WHERE vault_id = ?1 AND hash = ?2",
            params![vault_id.to_string(), hash],
        )?;
        Ok(())
    }

    pub fn attachment(&self, vault_id: VaultId, hash: &str) -> Result<Option<AttachmentRow>> {
        Ok(self
            .conn
            .query_row(
                "SELECT hash, size, mime, filename_hint FROM attachments WHERE vault_id = ?1 AND hash = ?2",
                params![vault_id.to_string(), hash],
                |r| {
                    Ok(AttachmentRow {
                        hash: r.get(0)?,
                        size: r.get::<_, i64>(1)? as u64,
                        mime: r.get(2)?,
                        filename_hint: r.get(3)?,
                    })
                },
            )
            .optional()?)
    }

    // ---- Notes and derived metadata -------------------------------------------------------

    pub fn upsert_note(
        &mut self,
        id: NoteId,
        vault_id: VaultId,
        path: &str,
        title: Option<&str>,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO notes (id, vault_id, path, title) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(id) DO UPDATE SET path = excluded.path, title = excluded.title,
                 updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), deleted_at = NULL",
            params![id.to_string(), vault_id.to_string(), path, title],
        )?;
        Ok(())
    }

    pub fn trash_note(&mut self, id: NoteId) -> Result<()> {
        self.conn.execute(
            "UPDATE notes SET deleted_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = ?1",
            params![id.to_string()],
        )?;
        Ok(())
    }

    /// Notes in the trash (soft-deleted), newest first, with when they were deleted.
    pub fn trashed_notes(&self, vault_id: VaultId) -> Result<Vec<(NoteRow, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, vault_id, path, title, deleted_at FROM notes WHERE vault_id = ?1 AND deleted_at IS NOT NULL ORDER BY deleted_at DESC",
        )?;
        let rows =
            stmt.query_map(params![vault_id.to_string()], |r| Ok((row_to_note(r)?, r.get::<_, String>(4)?)))?;
        rows.map(|r| r.map_err(Into::into)).collect()
    }

    /// Bring a trashed note back (its journal never left). Fails if the path is taken.
    pub fn restore_note(&mut self, id: NoteId) -> Result<Option<NoteRow>> {
        let Some(row) = self.trashed_row(id)? else { return Ok(None) };
        let taken = self.note_by_path(row.vault_id, &row.path)?.is_some();
        let path = if taken {
            format!("{} (restored).md", row.path.trim_end_matches(".md"))
        } else {
            row.path.clone()
        };
        self.conn.execute(
            "UPDATE notes SET deleted_at = NULL, path = ?2, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = ?1",
            params![id.to_string(), path],
        )?;
        self.note_by_id(id)
    }

    fn trashed_row(&self, id: NoteId) -> Result<Option<NoteRow>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id, vault_id, path, title FROM notes WHERE id = ?1 AND deleted_at IS NOT NULL",
                params![id.to_string()],
                row_to_note,
            )
            .optional()?)
    }

    /// Forget notes trashed more than `days` ago (SPEC §9: 30-day trash). Their journal rows
    /// are removed too.
    pub fn purge_trash(&mut self, days: u32) -> Result<usize> {
        let mut stmt = self.conn.prepare(
            "SELECT id FROM notes WHERE deleted_at IS NOT NULL AND deleted_at < strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?1)",
        )?;
        let ids: Vec<String> = stmt
            .query_map(params![format!("-{days} days")], |r| r.get(0))?
            .collect::<std::result::Result<_, _>>()?;
        drop(stmt);
        for id in &ids {
            self.conn.execute("DELETE FROM doc_updates WHERE doc_id = ?1", params![id])?;
            self.conn.execute("DELETE FROM doc_snapshots WHERE doc_id = ?1", params![id])?;
            self.conn.execute("DELETE FROM note_tags WHERE note_id = ?1", params![id])?;
            self.conn.execute("DELETE FROM note_links WHERE note_id = ?1", params![id])?;
            self.conn.execute("DELETE FROM note_attachments WHERE note_id = ?1", params![id])?;
            self.conn.execute("DELETE FROM notes_fts WHERE note_id = ?1", params![id])?;
            self.conn.execute("DELETE FROM notes WHERE id = ?1", params![id])?;
        }
        Ok(ids.len())
    }

    pub fn note_by_path(&self, vault_id: VaultId, path: &str) -> Result<Option<NoteRow>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id, vault_id, path, title FROM notes WHERE vault_id = ?1 AND path = ?2 AND deleted_at IS NULL",
                params![vault_id.to_string(), path],
                row_to_note,
            )
            .optional()?)
    }

    pub fn note_by_id(&self, id: NoteId) -> Result<Option<NoteRow>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id, vault_id, path, title FROM notes WHERE id = ?1 AND deleted_at IS NULL",
                params![id.to_string()],
                row_to_note,
            )
            .optional()?)
    }

    pub fn list_notes(&self, vault_id: VaultId) -> Result<Vec<NoteRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, vault_id, path, title FROM notes WHERE vault_id = ?1 AND deleted_at IS NULL ORDER BY path",
        )?;
        let rows = stmt.query_map(params![vault_id.to_string()], row_to_note)?;
        rows.map(|r| r.map_err(Into::into)).collect()
    }

    /// Replace a note's derived tags/links/search entry from a fresh [`NoteIndex`].
    pub fn index_note(&mut self, id: NoteId, ix: &NoteIndex) -> Result<()> {
        let sid = id.to_string();
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM note_tags WHERE note_id = ?1", params![sid])?;
        tx.execute("DELETE FROM note_links WHERE note_id = ?1", params![sid])?;
        tx.execute("DELETE FROM notes_fts WHERE note_id = ?1", params![sid])?;
        for tag in &ix.tags {
            tx.execute("INSERT OR IGNORE INTO note_tags (note_id, tag) VALUES (?1, ?2)", params![sid, tag])?;
        }
        for wl in &ix.wikilinks {
            tx.execute(
                "INSERT INTO note_links (note_id, target, kind) VALUES (?1, ?2, ?3)",
                params![sid, wl.target, if wl.embed { "embed" } else { "wikilink" }],
            )?;
        }
        for url in &ix.links {
            tx.execute(
                "INSERT INTO note_links (note_id, target, kind) VALUES (?1, ?2, 'link')",
                params![sid, url],
            )?;
        }
        tx.execute(
            "INSERT INTO notes_fts (note_id, title, body) VALUES (?1, ?2, ?3)",
            params![sid, ix.title.as_deref().unwrap_or(""), ix.plain_text],
        )?;
        tx.execute("UPDATE notes SET title = ?2 WHERE id = ?1", params![sid, ix.title])?;
        tx.commit()?;
        Ok(())
    }

    /// Live vaults known to this store: every vault doc in the journal, with its note count.
    pub fn vaults(&self) -> Result<Vec<(VaultId, u32)>> {
        let mut out = Vec::new();
        for id in self.doc_ids()? {
            if let DocId::Vault(v) = id {
                let n: u32 = self.conn.query_row(
                    "SELECT COUNT(*) FROM notes WHERE vault_id = ?1 AND deleted_at IS NULL",
                    params![v.to_string()],
                    |r| r.get(0),
                )?;
                out.push((v, n));
            }
        }
        Ok(out)
    }

    /// Notes in `vault_id` whose wikilinks resolve to `note`: by full path, path without
    /// extension, or basename without extension (SPEC §5.4 resolution order, loosely).
    pub fn backlinks_to(&self, note: &NoteRow) -> Result<Vec<NoteRow>> {
        let stem = note.path.trim_end_matches(".md").trim_end_matches(".qmd").to_owned();
        let base = stem.rsplit('/').next().unwrap_or(&stem).to_owned();
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT n.id, n.vault_id, n.path, n.title FROM note_links l JOIN notes n ON n.id = l.note_id
             WHERE n.vault_id = ?1 AND n.deleted_at IS NULL AND n.id != ?2 AND l.kind IN ('wikilink','embed')
               AND (l.target = ?3 OR l.target = ?4 OR l.target = ?5) ORDER BY n.path",
        )?;
        let rows = stmt.query_map(
            params![note.vault_id.to_string(), note.id.to_string(), note.path, stem, base],
            row_to_note,
        )?;
        rows.map(|r| r.map_err(Into::into)).collect()
    }

    /// Live notes carrying `tag` (exact, lower-case) or any nested tag under it.
    pub fn notes_with_tag(&self, vault_id: VaultId, tag: &str) -> Result<Vec<NoteRow>> {
        let tag = tag.trim_matches('/').to_lowercase();
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT n.id, n.vault_id, n.path, n.title FROM note_tags t JOIN notes n ON n.id = t.note_id
             WHERE n.vault_id = ?1 AND n.deleted_at IS NULL AND (t.tag = ?2 OR t.tag LIKE ?3) ORDER BY n.path",
        )?;
        let rows = stmt.query_map(params![vault_id.to_string(), tag, format!("{tag}/%")], row_to_note)?;
        rows.map(|r| r.map_err(Into::into)).collect()
    }

    pub fn tags_in_vault(&self, vault_id: VaultId) -> Result<Vec<(String, u32)>> {
        let mut stmt = self.conn.prepare(
            "SELECT t.tag, COUNT(*) FROM note_tags t JOIN notes n ON n.id = t.note_id
             WHERE n.vault_id = ?1 AND n.deleted_at IS NULL GROUP BY t.tag ORDER BY t.tag",
        )?;
        let rows = stmt.query_map(params![vault_id.to_string()], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.map(|r| r.map_err(Into::into)).collect()
    }

    /// Full-text search restricted to one vault. Supports the SPEC §10 filters `tag:x`
    /// (nested tags included), `path:Folder/` (prefix) and `has:math|tasks` alongside FTS5
    /// syntax (`"phrase"`, `-word`, `OR`); a query of filters only lists matching notes.
    pub fn search_in_vault(&self, vault_id: VaultId, query: &str, limit: u32) -> Result<Vec<SearchHit>> {
        let q = SearchQuery::parse(query);
        let mut args: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(vault_id.to_string())];
        // Filters only: plain rows (no snippet/rank); otherwise the FTS join.
        let mut sql = if q.text.is_empty() {
            String::from(
                "SELECT n.id, n.title, '', 0.0 FROM notes n LEFT JOIN notes_fts f ON f.note_id = n.id
                 WHERE n.deleted_at IS NULL AND n.vault_id = ?1",
            )
        } else {
            args.push(Box::new(q.text.clone()));
            String::from(
                "SELECT f.note_id, n.title, snippet(notes_fts, 2, '[', ']', '…', 12), bm25(notes_fts)
                 FROM notes_fts f JOIN notes n ON n.id = f.note_id
                 WHERE n.deleted_at IS NULL AND n.vault_id = ?1 AND notes_fts MATCH ?2",
            )
        };
        for tag in &q.tags {
            let n = args.len() + 1;
            sql.push_str(&format!(" AND EXISTS (SELECT 1 FROM note_tags t WHERE t.note_id = n.id AND (t.tag = ?{n} OR t.tag LIKE ?{n} || '/%'))"));
            args.push(Box::new(tag.clone()));
        }
        for p in &q.paths {
            let n = args.len() + 1;
            sql.push_str(&format!(" AND n.path LIKE ?{n} || '%'"));
            args.push(Box::new(p.clone()));
        }
        for h in &q.has {
            match h.as_str() {
                "math" => sql.push_str(" AND (f.body LIKE '%$%')"),
                "tasks" => sql.push_str(" AND EXISTS (SELECT 1 FROM notes_fts x WHERE x.note_id = n.id)"),
                _ => {}
            }
        }
        let n = args.len() + 1;
        sql.push_str(if q.text.is_empty() {
            " ORDER BY n.path LIMIT ?"
        } else {
            " ORDER BY bm25(notes_fts) LIMIT ?"
        });
        sql.push_str(&n.to_string());
        args.push(Box::new(limit));
        let mut stmt = self.conn.prepare(&sql)?;
        let params: Vec<&dyn rusqlite::ToSql> = args.iter().map(|b| b.as_ref()).collect();
        let rows = stmt.query_map(params.as_slice(), |r| {
            Ok((r.get::<_, String>(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?;
        rows.map(|r| {
            let (id, title, snippet, rank) = r?;
            Ok(SearchHit { note_id: id.parse()?, title, snippet, rank })
        })
        .collect()
    }

    pub fn backlinks(&self, target: &str) -> Result<Vec<NoteId>> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT note_id FROM note_links WHERE target = ?1 AND kind IN ('wikilink','embed')",
        )?;
        let rows = stmt.query_map(params![target], |r| r.get::<_, String>(0))?;
        rows.map(|r| r?.parse()).collect()
    }

    pub fn tags(&self) -> Result<Vec<(String, u32)>> {
        let mut stmt = self.conn.prepare("SELECT tag, COUNT(*) FROM note_tags GROUP BY tag ORDER BY tag")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.map(|r| r.map_err(Into::into)).collect()
    }

    /// Full-text search using FTS5 query syntax. Filters like `tag:` are layered on later (SPEC §10).
    pub fn search(&self, query: &str, limit: u32) -> Result<Vec<SearchHit>> {
        let mut stmt = self.conn.prepare(
            "SELECT f.note_id, n.title, snippet(notes_fts, 2, '[', ']', '…', 12), bm25(notes_fts)
             FROM notes_fts f JOIN notes n ON n.id = f.note_id
             WHERE notes_fts MATCH ?1 AND n.deleted_at IS NULL
             ORDER BY bm25(notes_fts) LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![query, limit], |r| {
            Ok((r.get::<_, String>(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?;
        rows.map(|r| {
            let (id, title, snippet, rank) = r?;
            Ok(SearchHit { note_id: id.parse()?, title, snippet, rank })
        })
        .collect()
    }

    // ---- Projection bookkeeping -----------------------------------------------------------

    pub fn projected_text(&self, doc_id: DocId) -> Result<Option<(String, String)>> {
        Ok(self
            .conn
            .query_row(
                "SELECT path, text FROM projection WHERE doc_id = ?1",
                params![doc_id.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?)
    }

    pub fn delete_projection(&mut self, doc_id: DocId) -> Result<()> {
        self.conn.execute("DELETE FROM projection WHERE doc_id = ?1", params![doc_id.to_string()])?;
        Ok(())
    }

    // ---- Users, sessions, memberships (server) ---------------------------------------------

    pub fn create_user(
        &mut self,
        id: &str,
        email: &str,
        display_name: &str,
        password_hash: Option<&str>,
        is_admin: bool,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO users (id, email, display_name, password_hash, is_admin, created_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![id, email.to_lowercase(), display_name, password_hash, is_admin as i32, now_ms()],
        )?;
        Ok(())
    }

    pub fn user_count(&self) -> Result<u32> {
        Ok(self.conn.query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0))?)
    }

    pub fn user_by_email(&self, email: &str) -> Result<Option<UserRow>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id, email, display_name, password_hash, is_admin FROM users WHERE email = ?1",
                params![email.to_lowercase()],
                row_to_user,
            )
            .optional()?)
    }

    pub fn user_by_id(&self, id: &str) -> Result<Option<UserRow>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id, email, display_name, password_hash, is_admin FROM users WHERE id = ?1",
                params![id],
                row_to_user,
            )
            .optional()?)
    }

    pub fn create_session(
        &mut self,
        token_hash: &str,
        user_id: &str,
        device: Option<&str>,
        ttl_ms: i64,
    ) -> Result<()> {
        let now = now_ms();
        self.conn.execute(
            "INSERT INTO sessions (token_hash, user_id, device_name, created_ms, expires_ms) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![token_hash, user_id, device, now, now + ttl_ms],
        )?;
        Ok(())
    }

    /// The user behind a live (unexpired) session token hash.
    pub fn session_user(&self, token_hash: &str) -> Result<Option<UserRow>> {
        Ok(self
            .conn
            .query_row(
                "SELECT u.id, u.email, u.display_name, u.password_hash, u.is_admin FROM sessions s JOIN users u ON u.id = s.user_id
                 WHERE s.token_hash = ?1 AND s.expires_ms > ?2",
                params![token_hash, now_ms()],
                row_to_user,
            )
            .optional()?)
    }

    pub fn delete_session(&mut self, token_hash: &str) -> Result<()> {
        self.conn.execute("DELETE FROM sessions WHERE token_hash = ?1", params![token_hash])?;
        Ok(())
    }

    pub fn set_membership(&mut self, vault_id: VaultId, user_id: &str, role: Role) -> Result<()> {
        self.conn.execute(
            "INSERT INTO memberships (vault_id, user_id, role) VALUES (?1, ?2, ?3) ON CONFLICT(vault_id, user_id) DO UPDATE SET role = excluded.role",
            params![vault_id.to_string(), user_id, role.as_str()],
        )?;
        Ok(())
    }

    pub fn remove_membership(&mut self, vault_id: VaultId, user_id: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM memberships WHERE vault_id = ?1 AND user_id = ?2",
            params![vault_id.to_string(), user_id],
        )?;
        Ok(())
    }

    pub fn membership(&self, vault_id: VaultId, user_id: &str) -> Result<Option<Role>> {
        let role: Option<String> = self
            .conn
            .query_row(
                "SELECT role FROM memberships WHERE vault_id = ?1 AND user_id = ?2",
                params![vault_id.to_string(), user_id],
                |r| r.get(0),
            )
            .optional()?;
        Ok(role.and_then(|r| Role::parse(&r)))
    }

    pub fn member_count(&self, vault_id: VaultId) -> Result<u32> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM memberships WHERE vault_id = ?1",
            params![vault_id.to_string()],
            |r| r.get(0),
        )?)
    }

    pub fn members(&self, vault_id: VaultId) -> Result<Vec<(UserRow, Role)>> {
        let mut stmt = self.conn.prepare(
            "SELECT u.id, u.email, u.display_name, u.password_hash, u.is_admin, m.role FROM memberships m JOIN users u ON u.id = m.user_id
             WHERE m.vault_id = ?1 ORDER BY m.role DESC, u.email",
        )?;
        let rows =
            stmt.query_map(params![vault_id.to_string()], |r| Ok((row_to_user(r)?, r.get::<_, String>(5)?)))?;
        rows.map(|r| {
            let (u, role) = r?;
            Ok((u, Role::parse(&role).unwrap_or(Role::Viewer)))
        })
        .collect()
    }

    /// Vaults the user belongs to, with role and live note count.
    pub fn vaults_of(&self, user_id: &str) -> Result<Vec<(VaultId, Role, u32)>> {
        let mut stmt = self.conn.prepare(
            "SELECT m.vault_id, m.role, (SELECT COUNT(*) FROM notes n WHERE n.vault_id = m.vault_id AND n.deleted_at IS NULL)
             FROM memberships m WHERE m.user_id = ?1 ORDER BY m.vault_id",
        )?;
        let rows = stmt.query_map(params![user_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, u32>(2)?))
        })?;
        rows.map(|r| {
            let (v, role, n) = r?;
            Ok((v.parse()?, Role::parse(&role).unwrap_or(Role::Viewer), n))
        })
        .collect()
    }

    // ---- Note shares ------------------------------------------------------------------------

    pub fn share_note_with_user(
        &mut self,
        note_id: NoteId,
        user_id: &str,
        role: Role,
        by: &str,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO note_shares (note_id, user_id, role, created_by, created_ms) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(note_id, user_id) DO UPDATE SET role = excluded.role",
            params![note_id.to_string(), user_id, role.as_str(), by, now_ms()],
        )?;
        Ok(())
    }

    pub fn share_note_by_link(
        &mut self,
        note_id: NoteId,
        token_hash: &str,
        role: Role,
        by: &str,
        expires_ms: Option<i64>,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO note_shares (note_id, token_hash, role, created_by, created_ms, expires_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![note_id.to_string(), token_hash, role.as_str(), by, now_ms(), expires_ms],
        )?;
        Ok(())
    }

    pub fn unshare_note_user(&mut self, note_id: NoteId, user_id: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM note_shares WHERE note_id = ?1 AND user_id = ?2",
            params![note_id.to_string(), user_id],
        )?;
        Ok(())
    }

    pub fn unshare_note_link(&mut self, note_id: NoteId, token_hash: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM note_shares WHERE note_id = ?1 AND token_hash = ?2",
            params![note_id.to_string(), token_hash],
        )?;
        Ok(())
    }

    /// The role a user holds on a note through a direct share.
    pub fn note_share_role(&self, note_id: NoteId, user_id: &str) -> Result<Option<Role>> {
        let role: Option<String> = self
            .conn
            .query_row(
                "SELECT role FROM note_shares WHERE note_id = ?1 AND user_id = ?2 AND (expires_ms IS NULL OR expires_ms > ?3)",
                params![note_id.to_string(), user_id, now_ms()],
                |r| r.get(0),
            )
            .optional()?;
        Ok(role.and_then(|r| Role::parse(&r)))
    }

    /// The note (and role) a public link token grants, if it is live.
    pub fn note_for_link(&self, token_hash: &str) -> Result<Option<(NoteId, Role)>> {
        let row: Option<(String, String)> = self
            .conn
            .query_row(
                "SELECT note_id, role FROM note_shares WHERE token_hash = ?1 AND (expires_ms IS NULL OR expires_ms > ?2)",
                params![token_hash, now_ms()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        Ok(match row {
            Some((id, role)) => Some((id.parse()?, Role::parse(&role).unwrap_or(Role::Viewer))),
            None => None,
        })
    }

    pub fn note_shares(&self, note_id: NoteId) -> Result<Vec<NoteShareRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT s.user_id, u.email, s.token_hash, s.role, s.expires_ms FROM note_shares s LEFT JOIN users u ON u.id = s.user_id
             WHERE s.note_id = ?1 ORDER BY s.created_ms",
        )?;
        let rows = stmt.query_map(params![note_id.to_string()], |r| {
            Ok(NoteShareRow {
                user_id: r.get(0)?,
                email: r.get(1)?,
                token_hash: r.get(2)?,
                role: Role::parse(&r.get::<_, String>(3)?).unwrap_or(Role::Viewer),
                expires_ms: r.get(4)?,
            })
        })?;
        rows.map(|r| r.map_err(Into::into)).collect()
    }

    /// Live notes shared directly with a user, with the vault they live in.
    pub fn notes_shared_with(&self, user_id: &str) -> Result<Vec<(NoteRow, Role)>> {
        let mut stmt = self.conn.prepare(
            "SELECT n.id, n.vault_id, n.path, n.title, s.role FROM note_shares s JOIN notes n ON n.id = s.note_id
             WHERE s.user_id = ?1 AND n.deleted_at IS NULL AND (s.expires_ms IS NULL OR s.expires_ms > ?2) ORDER BY n.path",
        )?;
        let rows =
            stmt.query_map(params![user_id, now_ms()], |r| Ok((row_to_note(r)?, r.get::<_, String>(4)?)))?;
        rows.map(|r| {
            let (n, role) = r?;
            Ok((n, Role::parse(&role).unwrap_or(Role::Viewer)))
        })
        .collect()
    }

    // ---- Meta -------------------------------------------------------------------------------

    pub fn meta_get(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT value FROM meta WHERE key = ?1", params![key], |r| r.get(0))
            .optional()?)
    }

    pub fn meta_set(&mut self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO meta (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn set_projected_text(&mut self, doc_id: DocId, path: &str, text: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO projection (doc_id, path, text) VALUES (?1, ?2, ?3)
             ON CONFLICT(doc_id) DO UPDATE SET path = excluded.path, text = excluded.text",
            params![doc_id.to_string(), path, text],
        )?;
        Ok(())
    }
}

fn row_to_user(r: &rusqlite::Row<'_>) -> rusqlite::Result<UserRow> {
    Ok(UserRow {
        id: r.get(0)?,
        email: r.get(1)?,
        display_name: r.get(2)?,
        password_hash: r.get(3)?,
        is_admin: r.get::<_, i32>(4)? != 0,
    })
}

fn row_to_note(r: &rusqlite::Row<'_>) -> rusqlite::Result<NoteRow> {
    let id: String = r.get(0)?;
    let vault: String = r.get(1)?;
    Ok(NoteRow {
        id: id.parse().map_err(|_| rusqlite::Error::InvalidQuery)?,
        vault_id: vault.parse().map_err(|_| rusqlite::Error::InvalidQuery)?,
        path: r.get(2)?,
        title: r.get(3)?,
    })
}

/// Runtime SQLite library version string.
pub fn sqlite_version() -> String {
    rusqlite::version().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::markdown;

    #[test]
    fn update_log_snapshot_and_reload() {
        let mut store = Store::open_in_memory().unwrap();
        let id = DocId::Note(NoteId::new());
        let doc = NoteDoc::new();
        let u1 = doc.set_text("first");
        let u2 = doc.set_text("first second");
        assert_eq!(store.append_update(id, &u1, None).unwrap(), 1);
        assert_eq!(store.append_update(id, &u2, Some("alice")).unwrap(), 2);
        assert_eq!(store.load_doc(id).unwrap().text(), "first second");

        let head = store.snapshot_at(id, &doc.encode_full(), 1_000).unwrap();
        assert_eq!(head, 2);
        let m = store
            .maintain(
                id,
                &RetentionPolicy { retain_updates: Duration::ZERO, ..Default::default() },
                1_000,
                || unreachable!(),
            )
            .unwrap();
        assert_eq!(m, Maintenance { snapshotted: false, pruned_updates: 2, pruned_snapshots: 0 });
        let u3 = doc.set_text("first second third");
        assert_eq!(store.append_update(id, &u3, None).unwrap(), 3);
        assert_eq!(store.load_doc(id).unwrap().text(), "first second third");
        assert_eq!(store.load_doc(DocId::Note(NoteId::new())).unwrap().text(), "");

        let vault = VaultId::new();
        let vd = VaultDoc::new();
        let n = NoteId::new();
        let uv = vd.set_path(n, "x.md");
        store.append_update(DocId::Vault(vault), &uv, None).unwrap();
        assert_eq!(store.load_vault_doc(vault).unwrap().entries(), vec![(n, "x.md".to_owned())]);

        assert_eq!(store.meta_get("vault_id").unwrap(), None);
        store.meta_set("vault_id", &vault.to_string()).unwrap();
        store.meta_set("vault_id", &vault.to_string()).unwrap();
        assert_eq!(store.meta_get("vault_id").unwrap().as_deref(), Some(vault.to_string().as_str()));
    }

    #[test]
    fn maintenance_policy() {
        const MIN: i64 = 60_000;
        const DAY: i64 = 24 * 60 * MIN;
        let policy = RetentionPolicy {
            snapshot_every_updates: 3,
            snapshot_interval: Duration::from_millis(10 * MIN as u64),
            retain_updates: Duration::from_millis(90 * DAY as u64),
        };
        let mut store = Store::open_in_memory().unwrap();
        let id = DocId::Note(NoteId::new());
        let doc = NoteDoc::new();
        let mut t = 0;
        let push = |store: &mut Store, text: &str, t: i64| {
            let u = doc.set_text(text);
            store.append_update_at(id, &u, None, t).unwrap();
        };

        // Two updates: below the count threshold, not stale → nothing.
        push(&mut store, "a", t);
        push(&mut store, "a b", t);
        assert_eq!(store.maintain(id, &policy, t, || doc.encode_full()).unwrap(), Maintenance::default());
        // Third update reaches the count threshold → snapshot at seq 3, nothing pruned (too young).
        push(&mut store, "a b c", t);
        let m = store.maintain(id, &policy, t, || doc.encode_full()).unwrap();
        assert!(m.snapshotted && m.pruned_updates == 0);
        // One more update now; 11 minutes later it is stale → second snapshot.
        push(&mut store, "a b c d", t);
        assert!(!store.maintain(id, &policy, t, || unreachable!()).unwrap().snapshotted);
        t += 11 * MIN;
        assert!(store.maintain(id, &policy, t, || doc.encode_full()).unwrap().snapshotted);
        // Same call again: nothing pending → no snapshot; `full_state` must not be invoked.
        assert!(!store.maintain(id, &policy, t, || unreachable!()).unwrap().snapshotted);

        // 89 days later: both snapshots are inside the window → no pruning.
        t += 89 * DAY;
        assert_eq!(store.maintain(id, &policy, t, || unreachable!()).unwrap().pruned_updates, 0);
        // 91 days after the second snapshot: everything up to it is redundant.
        t += 2 * DAY;
        let m = store.maintain(id, &policy, t, || unreachable!()).unwrap();
        assert_eq!((m.pruned_updates, m.pruned_snapshots), (4, 1));
        // The doc is still fully reconstructible, and new updates keep working.
        assert_eq!(store.load_doc(id).unwrap().text(), "a b c d");
        push(&mut store, "a b c d e", t);
        assert_eq!(store.load_doc(id).unwrap().text(), "a b c d e");
        assert_eq!(store.doc_ids().unwrap(), vec![id]);
    }

    #[test]
    fn note_shares() {
        let mut store = Store::open_in_memory().unwrap();
        let v = VaultId::new();
        let (n1, n2) = (NoteId::new(), NoteId::new());
        store.create_user("u1", "a@x", "A", None, false).unwrap();
        store.create_user("u2", "b@x", "B", None, false).unwrap();
        store.upsert_note(n1, v, "one.md", Some("One")).unwrap();
        store.upsert_note(n2, v, "two.md", None).unwrap();
        store.share_note_with_user(n1, "u2", Role::Viewer, "u1").unwrap();
        store.share_note_with_user(n1, "u2", Role::Editor, "u1").unwrap();
        assert_eq!(store.note_share_role(n1, "u2").unwrap(), Some(Role::Editor));
        assert_eq!(store.note_share_role(n2, "u2").unwrap(), None);
        store.share_note_by_link(n2, "h1", Role::Viewer, "u1", None).unwrap();
        store.share_note_by_link(n2, "h2", Role::Viewer, "u1", Some(1)).unwrap();
        assert_eq!(store.note_for_link("h1").unwrap(), Some((n2, Role::Viewer)));
        assert_eq!(store.note_for_link("h2").unwrap(), None, "expired");
        assert_eq!(store.note_shares(n2).unwrap().len(), 2);
        let shared = store.notes_shared_with("u2").unwrap();
        assert_eq!(shared.len(), 1);
        assert_eq!(shared[0].0.path, "one.md");
        store.unshare_note_user(n1, "u2").unwrap();
        store.unshare_note_link(n2, "h1").unwrap();
        assert!(store.notes_shared_with("u2").unwrap().is_empty());
        assert_eq!(store.note_for_link("h1").unwrap(), None);
    }

    #[test]
    fn versions_and_history() {
        let mut store = Store::open_in_memory().unwrap();
        let id = DocId::Note(NoteId::new());
        let doc = NoteDoc::new();
        for (i, text) in ["one", "one two", "one two three", "one two three four"].iter().enumerate() {
            let u = doc.set_text(text);
            store.append_update_at(id, &u, None, i as i64 * 1000).unwrap();
        }
        let v = store
            .snapshot_labeled_at(id, &doc.encode_full(), 5000, Some("before rewrite"), Some("ann"))
            .unwrap();
        assert_eq!(v, 4);
        let u = doc.set_text("rewritten");
        store.append_update_at(id, &u, None, 6000).unwrap();
        assert_eq!(store.load_doc_at(id, 2).unwrap().text(), "one two");
        assert_eq!(store.load_doc_at(id, 4).unwrap().text(), "one two three four");
        assert_eq!(store.load_doc(id).unwrap().text(), "rewritten");
        let versions = store.versions(id).unwrap();
        assert_eq!(versions.len(), 1);
        assert_eq!(versions[0].label.as_deref(), Some("before rewrite"));
        // Pruning far in the future keeps the labelled version but drops the raw updates.
        let policy = RetentionPolicy { snapshot_every_updates: 1, ..Default::default() };
        let later = 6000 + 200 * 24 * 3600 * 1000;
        let m = store.maintain(id, &policy, later, || doc.encode_full()).unwrap();
        assert!(m.snapshotted && m.pruned_updates == 4 && m.pruned_snapshots == 0, "{m:?}");
        assert_eq!(store.versions(id).unwrap().len(), 2);
        assert_eq!(store.load_doc_at(id, 4).unwrap().text(), "one two three four");
    }

    #[test]
    fn attachments_rows() {
        let mut store = Store::open_in_memory().unwrap();
        let v = VaultId::new();
        let row =
            AttachmentRow { hash: "abc".into(), size: 3, mime: "image/png".into(), filename_hint: None };
        store.upsert_attachment(v, &row).unwrap();
        store
            .upsert_attachment(v, &AttachmentRow { filename_hint: Some("x.png".into()), ..row.clone() })
            .unwrap();
        let got = store.attachment(v, "abc").unwrap().unwrap();
        assert_eq!(got.filename_hint.as_deref(), Some("x.png"));
        assert_eq!(store.attachment(VaultId::new(), "abc").unwrap(), None);

        let (a, b) = (NoteId::new(), NoteId::new());
        store.upsert_note(a, v, "a.md", None).unwrap();
        store.upsert_note(b, v, "b.md", None).unwrap();
        store.set_note_attachments(a, &["x.png".into(), "y.png".into()]).unwrap();
        store.set_note_attachments(b, &["y.png".into()]).unwrap();
        assert_eq!(store.referenced_attachment_paths().unwrap().len(), 2);
        store.trash_note(a).unwrap();
        let refs = store.referenced_attachment_paths().unwrap();
        assert!(refs.contains("y.png") && !refs.contains("x.png"));
        store.clear_note_attachments(b).unwrap();
        assert!(store.referenced_attachment_paths().unwrap().is_empty());
    }

    #[test]
    fn users_sessions_memberships() {
        let mut store = Store::open_in_memory().unwrap();
        assert_eq!(store.user_count().unwrap(), 0);
        store.create_user("u1", "Ann@Example.org", "Ann", Some("hash"), true).unwrap();
        store.create_user("u2", "bob@example.org", "Bob", Some("hash"), false).unwrap();
        assert!(
            store.create_user("u3", "ann@example.org", "Dup", None, false).is_err(),
            "emails are unique, case-insensitively"
        );
        assert_eq!(store.user_by_email("ANN@example.org").unwrap().unwrap().id, "u1");
        store.create_session("t1", "u1", Some("laptop"), 60_000).unwrap();
        store.create_session("t2", "u2", None, -1).unwrap();
        assert_eq!(store.session_user("t1").unwrap().unwrap().email, "ann@example.org");
        assert_eq!(store.session_user("t2").unwrap(), None, "expired");
        store.delete_session("t1").unwrap();
        assert_eq!(store.session_user("t1").unwrap(), None);

        let v = VaultId::new();
        assert_eq!(store.member_count(v).unwrap(), 0);
        store.set_membership(v, "u1", Role::Owner).unwrap();
        store.set_membership(v, "u2", Role::Viewer).unwrap();
        store.set_membership(v, "u2", Role::Editor).unwrap();
        assert_eq!(store.membership(v, "u2").unwrap(), Some(Role::Editor));
        assert_eq!(store.membership(VaultId::new(), "u2").unwrap(), None);
        assert!(Role::Owner > Role::Editor && Role::Editor > Role::Viewer);
        assert_eq!(store.members(v).unwrap().len(), 2);
        assert_eq!(store.vaults_of("u2").unwrap(), vec![(v, Role::Editor, 0)]);
        store.remove_membership(v, "u2").unwrap();
        assert_eq!(store.member_count(v).unwrap(), 1);
    }

    #[test]
    fn notes_index_and_search() {
        let mut store = Store::open_in_memory().unwrap();
        let vault = VaultId::new();
        let a = NoteId::new();
        let b = NoteId::new();
        store.upsert_note(a, vault, "Projects/Alpha.md", None).unwrap();
        store.upsert_note(b, vault, "Daily/2026-08-29.md", None).unwrap();
        store.index_note(a, &markdown::index("# Alpha\n\nThe quick brown fox. #project\n").unwrap()).unwrap();
        store
            .index_note(b, &markdown::index("Worked on [[Alpha]] today. #daily #project\n").unwrap())
            .unwrap();

        assert_eq!(store.list_notes(vault).unwrap().len(), 2);
        assert_eq!(
            store.note_by_path(vault, "Projects/Alpha.md").unwrap().unwrap().title.as_deref(),
            Some("Alpha")
        );
        assert_eq!(store.backlinks("Alpha").unwrap(), vec![b]);
        let alpha = store.note_by_id(a).unwrap().unwrap();
        assert_eq!(store.backlinks_to(&alpha).unwrap().iter().map(|r| r.id).collect::<Vec<_>>(), vec![b]);
        assert_eq!(store.vaults().unwrap(), vec![]); // no vault doc journaled in this test
        assert_eq!(store.tags_in_vault(vault).unwrap().len(), 2);
        assert_eq!(store.notes_with_tag(vault, "project").unwrap().len(), 2);
        assert_eq!(
            store.notes_with_tag(vault, "Daily").unwrap().iter().map(|r| r.id).collect::<Vec<_>>(),
            vec![b]
        );
        assert!(store.notes_with_tag(vault, "nope").unwrap().is_empty());
        assert_eq!(store.search_in_vault(vault, "quick fox", 10).unwrap().len(), 1);
        assert_eq!(store.search_in_vault(VaultId::new(), "quick fox", 10).unwrap().len(), 0);
        // Filters (SPEC §10)
        assert_eq!(store.search_in_vault(vault, "tag:project", 10).unwrap().len(), 2);
        assert_eq!(store.search_in_vault(vault, "tag:daily", 10).unwrap().len(), 1);
        assert_eq!(store.search_in_vault(vault, "today tag:project", 10).unwrap().len(), 1);
        assert_eq!(store.search_in_vault(vault, "path:Projects/", 10).unwrap().len(), 1);
        assert_eq!(store.search_in_vault(vault, "path:Nope/", 10).unwrap().len(), 0);
        assert_eq!(
            SearchQuery::parse("a tag:#X/ path:/P has:Math b"),
            SearchQuery {
                text: "a b".into(),
                tags: vec!["x".into()],
                paths: vec!["P".into()],
                has: vec!["math".into()]
            }
        );
        assert_eq!(store.tags().unwrap(), vec![("daily".to_owned(), 1), ("project".to_owned(), 2)]);
        let hits = store.search("quick fox", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].note_id, a);
        store.trash_note(a).unwrap();
        assert!(store.search("quick fox", 10).unwrap().is_empty());
        assert_eq!(store.list_notes(vault).unwrap().len(), 1);
        assert_eq!(store.trashed_notes(vault).unwrap().len(), 1);
        let restored = store.restore_note(a).unwrap().unwrap();
        assert_eq!(restored.path, "Projects/Alpha.md");
        assert_eq!(store.list_notes(vault).unwrap().len(), 2);
        assert_eq!(store.restore_note(a).unwrap(), None, "not trashed any more");
        store.trash_note(a).unwrap();
        assert_eq!(store.purge_trash(30).unwrap(), 0, "too fresh");
        store
            .conn
            .execute(
                "UPDATE notes SET deleted_at = '2000-01-01T00:00:00.000Z' WHERE id = ?1",
                params![a.to_string()],
            )
            .unwrap();
        assert_eq!(store.purge_trash(30).unwrap(), 1);
        assert_eq!(store.note_by_id(a).unwrap(), None);
    }
}

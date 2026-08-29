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

pub const SCHEMA_VERSION: u32 = 4;

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
pub struct AttachmentRow {
    pub hash: String,
    pub size: u64,
    pub mime: String,
    pub filename_hint: Option<String>,
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
        let id = doc_id.to_string();
        let head: i64 = self.conn.query_row(
            "SELECT COALESCE(MAX(seq), 0) FROM (SELECT seq FROM doc_updates WHERE doc_id = ?1 UNION ALL SELECT seq FROM doc_snapshots WHERE doc_id = ?1)",
            params![id],
            |r| r.get(0),
        )?;
        self.conn.execute(
            "INSERT OR REPLACE INTO doc_snapshots (doc_id, seq, bytes, created_ms) VALUES (?1, ?2, ?3, ?4)",
            params![id, head, full_state, now_ms],
        )?;
        Ok(head)
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
            out.pruned_snapshots = self
                .conn
                .execute("DELETE FROM doc_snapshots WHERE doc_id = ?1 AND seq < ?2", params![id, cut])?;
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
        assert_eq!(store.tags().unwrap(), vec![("daily".to_owned(), 1), ("project".to_owned(), 2)]);
        let hits = store.search("quick fox", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].note_id, a);
        store.trash_note(a).unwrap();
        assert!(store.search("quick fox", 10).unwrap().is_empty());
        assert_eq!(store.list_notes(vault).unwrap().len(), 1);
    }
}

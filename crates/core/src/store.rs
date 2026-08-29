//! SQLite persistence (SPEC §6.1 / §6.2). One schema serves both the server (`notes.db`) and
//! native clients (`<vault>/.notes/local.db`); clients simply leave the multi-user tables empty.

use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};

use crate::doc::NoteDoc;
use crate::error::Result;
use crate::ids::{DocId, NoteId, VaultId};
use crate::markdown::NoteIndex;

pub const SCHEMA_VERSION: u32 = 1;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS doc_updates (
    doc_id     TEXT    NOT NULL,
    seq        INTEGER NOT NULL,
    bytes      BLOB    NOT NULL,
    author_id  TEXT,
    created_at TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    PRIMARY KEY (doc_id, seq)
);
CREATE TABLE IF NOT EXISTS doc_snapshots (
    doc_id     TEXT    NOT NULL,
    seq        INTEGER NOT NULL,
    bytes      BLOB    NOT NULL,
    created_at TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    PRIMARY KEY (doc_id, seq)
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
"#;

pub struct Store {
    conn: Connection,
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
        if version < SCHEMA_VERSION {
            conn.execute_batch(SCHEMA)?;
            conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        }
        Ok(Self { conn })
    }

    // ---- CRDT log -------------------------------------------------------------------------

    /// Append one update to a doc's log; returns its sequence number.
    pub fn append_update(&mut self, doc_id: DocId, bytes: &[u8], author: Option<&str>) -> Result<i64> {
        let id = doc_id.to_string();
        let tx = self.conn.transaction()?;
        let seq: i64 = tx.query_row(
            "SELECT COALESCE(MAX(seq), 0) + 1 FROM (SELECT seq FROM doc_updates WHERE doc_id = ?1 UNION ALL SELECT seq FROM doc_snapshots WHERE doc_id = ?1)",
            params![id],
            |r| r.get(0),
        )?;
        tx.execute(
            "INSERT INTO doc_updates (doc_id, seq, bytes, author_id) VALUES (?1, ?2, ?3, ?4)",
            params![id, seq, bytes, author],
        )?;
        tx.commit()?;
        Ok(seq)
    }

    /// Load a doc from its latest snapshot plus subsequent updates. A never-seen doc is empty.
    pub fn load_doc(&self, doc_id: DocId) -> Result<NoteDoc> {
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
        NoteDoc::from_updates(updates.iter().map(Vec::as_slice))
    }

    /// Persist the doc's full state as a snapshot at the current head. Retention/pruning of
    /// older updates is a policy decision left to the caller (SPEC §9).
    pub fn snapshot(&mut self, doc_id: DocId, doc: &NoteDoc) -> Result<i64> {
        let id = doc_id.to_string();
        let head: i64 = self.conn.query_row(
            "SELECT COALESCE(MAX(seq), 0) FROM doc_updates WHERE doc_id = ?1",
            params![id],
            |r| r.get(0),
        )?;
        self.conn.execute(
            "INSERT OR REPLACE INTO doc_snapshots (doc_id, seq, bytes) VALUES (?1, ?2, ?3)",
            params![id, head, doc.encode_full()],
        )?;
        Ok(head)
    }

    pub fn prune_updates_before(&mut self, doc_id: DocId, seq: i64) -> Result<usize> {
        Ok(self.conn.execute(
            "DELETE FROM doc_updates WHERE doc_id = ?1 AND seq <= ?2",
            params![doc_id.to_string(), seq],
        )?)
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

        let head = store.snapshot(id, &doc).unwrap();
        assert_eq!(head, 2);
        store.prune_updates_before(id, head).unwrap();
        let u3 = doc.set_text("first second third");
        assert_eq!(store.append_update(id, &u3, None).unwrap(), 3);
        assert_eq!(store.load_doc(id).unwrap().text(), "first second third");
        assert_eq!(store.load_doc(DocId::Note(NoteId::new())).unwrap().text(), "");
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

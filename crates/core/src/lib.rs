//! notes-core — the shared engine behind the server, desktop, and mobile apps.
//!
//! Responsibilities (see SPEC.md §3.1):
//! - CRDT note documents ([`doc`]) backed by yrs, with text-diff application ([`diff`]).
//! - Persistence of update logs, snapshots, and derived metadata in SQLite ([`store`]).
//! - The wire framing used over the sync WebSocket ([`sync`]).
//! - The on-disk projection of a vault and ingestion of external edits ([`projection`], [`watcher`]).
//! - Markdown indexing for titles, tags, links, and search ([`markdown`]).
//!
//! No UI, no network I/O beyond types; the server and native shells wire those up.

pub mod diff;
pub mod doc;
pub mod error;
pub mod ids;
pub mod markdown;
pub mod projection;
pub mod store;
pub mod sync;
pub mod watcher;

pub use doc::NoteDoc;
pub use error::{Error, Result};
pub use ids::{DocId, NoteId, VaultId};
pub use markdown::NoteIndex;
pub use projection::Projection;
pub use store::Store;

/// Name of the Y.Text holding a note's markdown source (SPEC §4.2).
pub const CONTENT_FIELD: &str = "content";
/// Sidecar directory inside a projected vault (SPEC §6.2).
pub const SIDECAR_DIR: &str = ".notes";

//! Two vault directories synced through a real server, exercising create, edit, rename, and
//! delete in both directions via `once` runs (offline-then-reconcile is the hardest path).

use std::net::SocketAddr;
use std::path::Path;

use std::time::Duration;

use notes_core::client::{SyncOptions, SyncReport};
use notes_core::{Store, VaultId};
use notes_server::{AppState, build_state, router};
use std::sync::Arc;

async fn start_server() -> (String, Arc<AppState>) {
    let state = build_state(Store::open_in_memory().unwrap());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    let app = router(state.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), state)
}

fn opts(dir: &Path, server: &str, vault_id: Option<VaultId>) -> SyncOptions {
    SyncOptions { vault_dir: dir.to_path_buf(), server_url: server.to_owned(), vault_id, once: true }
}

/// `client::run` in once mode, bounded so a regression can never hang the suite.
async fn run(o: SyncOptions) -> notes_core::Result<SyncReport> {
    tokio::time::timeout(Duration::from_secs(30), notes_core::client::run(o))
        .await
        .expect("sync run timed out")
}

fn read(dir: &Path, rel: &str) -> String {
    std::fs::read_to_string(dir.join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

#[tokio::test]
async fn two_directories_round_trip_through_server() {
    let (server, state) = start_server().await;
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();

    // A creates a vault with one note and pushes it.
    std::fs::create_dir_all(a.path().join("Projects")).unwrap();
    std::fs::write(a.path().join("Projects/plan.md"), "# Plan\n\nstep one\n").unwrap();
    let report = run(opts(a.path(), &server, None)).await.unwrap();
    assert_eq!(report.notes, 1);
    let vault_id = report.vault_id;

    // B joins the vault into an empty directory and receives the file.
    let report = run(opts(b.path(), &server, Some(vault_id))).await.unwrap();
    assert_eq!(report.notes, 1);
    assert_eq!(read(b.path(), "Projects/plan.md"), "# Plan\n\nstep one\n");

    // B edits offline, syncs; A syncs and sees the edit.
    std::fs::write(b.path().join("Projects/plan.md"), "# Plan\n\nstep one\nstep two\n").unwrap();
    run(opts(b.path(), &server, None)).await.unwrap();
    run(opts(a.path(), &server, None)).await.unwrap();
    assert_eq!(read(a.path(), "Projects/plan.md"), "# Plan\n\nstep one\nstep two\n");

    // Concurrent offline edits on both sides merge rather than clobber.
    std::fs::write(a.path().join("Projects/plan.md"), "# Plan!\n\nstep one\nstep two\n").unwrap();
    std::fs::write(b.path().join("Projects/plan.md"), "# Plan\n\nstep one\nstep two\nstep three\n").unwrap();
    run(opts(a.path(), &server, None)).await.unwrap();
    run(opts(b.path(), &server, None)).await.unwrap();
    run(opts(a.path(), &server, None)).await.unwrap();
    let merged = "# Plan!\n\nstep one\nstep two\nstep three\n";
    assert_eq!(read(a.path(), "Projects/plan.md"), merged);
    assert_eq!(read(b.path(), "Projects/plan.md"), merged);

    // A renames the file offline; B ends up with the file moved, same content, no duplicate.
    std::fs::rename(a.path().join("Projects/plan.md"), a.path().join("plan-v2.md")).unwrap();
    run(opts(a.path(), &server, None)).await.unwrap();
    run(opts(b.path(), &server, None)).await.unwrap();
    assert!(!b.path().join("Projects/plan.md").exists());
    assert_eq!(read(b.path(), "plan-v2.md"), merged);
    let b_store = Store::open(b.path().join(".notes/local.db")).unwrap();
    assert_eq!(b_store.list_notes(vault_id).unwrap().len(), 1);
    drop(b_store);

    // B adds a note and deletes the first; A converges.
    std::fs::write(b.path().join("new.md"), "fresh\n").unwrap();
    std::fs::remove_file(b.path().join("plan-v2.md")).unwrap();
    run(opts(b.path(), &server, None)).await.unwrap();
    let report = run(opts(a.path(), &server, None)).await.unwrap();
    assert_eq!(report.notes, 1);
    assert_eq!(read(a.path(), "new.md"), "fresh\n");
    assert!(!a.path().join("plan-v2.md").exists());

    // The server derived its relational view from the CRDT stream: one live note, searchable.
    let store = state.store.lock().await;
    let rows = store.list_notes(vault_id).unwrap();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].path, "new.md");
    let hits = store.search("fresh", 10).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].note_id, rows[0].id);
}

#[tokio::test]
async fn once_fails_fast_when_server_is_down() {
    let dir = tempfile::tempdir().unwrap();
    let err = run(opts(dir.path(), "http://127.0.0.1:9", None)).await.unwrap_err();
    assert!(err.to_string().contains("cannot connect"), "{err}");
}

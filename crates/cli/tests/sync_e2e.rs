//! Two vault directories synced through a real server, exercising create, edit, rename, and
//! delete in both directions via `once` runs (offline-then-reconcile is the hardest path).

use std::net::SocketAddr;
use std::path::Path;

use std::time::Duration;

use notes_core::client::{SyncOptions, SyncReport};
use notes_core::{Store, VaultId};
use notes_server::{AppState, ServerOptions, build_state, router};
use std::sync::Arc;

async fn start_server() -> (String, Arc<AppState>, tempfile::TempDir) {
    let blobs = tempfile::tempdir().unwrap();
    let options = ServerOptions { attachments_dir: blobs.path().to_path_buf(), ..ServerOptions::default() };
    let state = build_state(Store::open_in_memory().unwrap(), options);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    let app = router(state.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), state, blobs)
}

fn opts(dir: &Path, server: &str, vault_id: Option<VaultId>) -> SyncOptions {
    SyncOptions {
        vault_dir: dir.to_path_buf(),
        server_url: server.to_owned(),
        vault_id,
        once: true,
        ca_cert: None,
        token: None,
    }
}

/// `client::run` in once mode, bounded so a regression can never hang the suite.
async fn run(o: SyncOptions) -> notes_core::Result<SyncReport> {
    tokio::time::timeout(Duration::from_secs(30), notes_core::client::run(o))
        .await
        .expect("sync run timed out")
}

/// Note body without the `id:` front matter the engine adds (SPEC §6.3).
fn read(dir: &Path, rel: &str) -> String {
    let text = std::fs::read_to_string(dir.join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"));
    match notes_core::frontmatter::block(&text) {
        Some((_, end)) => text[end..].to_owned(),
        None => text,
    }
}

/// Replace a note's body the way an editor would, keeping its front matter.
fn write_body(dir: &Path, rel: &str, body: &str) {
    let path = dir.join(rel);
    let head = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| notes_core::frontmatter::block(&t).map(|(_, end)| t[..end].to_owned()))
        .unwrap_or_default();
    std::fs::write(path, format!("{head}{body}")).unwrap();
}

#[tokio::test]
async fn two_directories_round_trip_through_server() {
    let (server, state, _blobs) = start_server().await;
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();

    // A creates a vault with one note and pushes it.
    std::fs::create_dir_all(a.path().join("Projects")).unwrap();
    write_body(a.path(), "Projects/plan.md", "# Plan\n\nstep one\n");
    let report = run(opts(a.path(), &server, None)).await.unwrap();
    assert_eq!(report.notes, 1);
    let vault_id = report.vault_id;

    // B joins the vault into an empty directory and receives the file.
    let report = run(opts(b.path(), &server, Some(vault_id))).await.unwrap();
    assert_eq!(report.notes, 1);
    assert_eq!(read(b.path(), "Projects/plan.md"), "# Plan\n\nstep one\n");

    // B edits offline, syncs; A syncs and sees the edit.
    write_body(b.path(), "Projects/plan.md", "# Plan\n\nstep one\nstep two\n");
    run(opts(b.path(), &server, None)).await.unwrap();
    run(opts(a.path(), &server, None)).await.unwrap();
    assert_eq!(read(a.path(), "Projects/plan.md"), "# Plan\n\nstep one\nstep two\n");

    // Concurrent offline edits on both sides merge rather than clobber.
    write_body(a.path(), "Projects/plan.md", "# Plan!\n\nstep one\nstep two\n");
    write_body(b.path(), "Projects/plan.md", "# Plan\n\nstep one\nstep two\nstep three\n");
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
async fn attachments_follow_references() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_test_writer()
        .try_init();
    let (server, state, _blobs) = start_server().await;
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let logo: Vec<u8> = (0..3000u32).map(|i| (i * 7 % 251) as u8).collect();

    // A references a file two ways; only referenced files travel.
    std::fs::create_dir_all(a.path().join("attachments")).unwrap();
    std::fs::write(a.path().join("attachments/logo.png"), &logo).unwrap();
    std::fs::write(a.path().join("attachments/unreferenced.bin"), b"nope").unwrap();
    write_body(a.path(), "pic.md", "# Pic\n\n![[logo.png]] and ![again](attachments/logo.png)\n");
    let report = run(opts(a.path(), &server, None)).await.unwrap();
    let vault_id = report.vault_id;

    run(opts(b.path(), &server, Some(vault_id))).await.unwrap();
    assert_eq!(std::fs::read(b.path().join("attachments/logo.png")).unwrap(), logo);
    assert!(!b.path().join("attachments/unreferenced.bin").exists());
    {
        let store = state.store.lock().await;
        let hash = notes_core::attachments::hash_bytes(&logo);
        let row = store.attachment(vault_id, &hash).unwrap().expect("server row");
        assert_eq!(
            (row.size, row.mime.as_str(), row.filename_hint.as_deref()),
            (3000, "image/png", Some("logo.png"))
        );
        assert!(state.attachments.exists(vault_id, &hash));
    }

    // A replaces the image bytes; B receives the new content at the same path.
    let logo2: Vec<u8> = logo.iter().map(|b| b ^ 0xff).collect();
    std::fs::write(a.path().join("attachments/logo.png"), &logo2).unwrap();
    run(opts(a.path(), &server, None)).await.unwrap();
    run(opts(b.path(), &server, None)).await.unwrap();
    assert_eq!(std::fs::read(b.path().join("attachments/logo.png")).unwrap(), logo2);

    // B adds a note with a note-relative link; A gets the file at the same relative place.
    std::fs::create_dir_all(b.path().join("sub/img")).unwrap();
    std::fs::write(b.path().join("sub/img/two.bin"), b"two").unwrap();
    std::fs::write(b.path().join("sub/note.md"), "![x](img/two.bin)\n").unwrap();
    run(opts(b.path(), &server, None)).await.unwrap();
    run(opts(a.path(), &server, None)).await.unwrap();
    assert_eq!(std::fs::read(a.path().join("sub/img/two.bin")).unwrap(), b"two");

    // Orphans: B drops every reference to the logo; the entry leaves the vault doc, the server
    // purges both logo versions after the grace period, and two.bin survives.
    let (h1, h2, h_two) = (
        notes_core::attachments::hash_bytes(&logo),
        notes_core::attachments::hash_bytes(&logo2),
        notes_core::attachments::hash_bytes(b"two"),
    );
    let now = notes_core::store::now_ms();
    let first = notes_server::purge_orphans(&state, now, Duration::from_secs(3600)).await.unwrap();
    assert_eq!(
        (first.newly_orphaned, first.purged),
        (1, 0),
        "old logo version is orphaned; nothing purged yet"
    );
    write_body(
        b.path(),
        "pic.md",
        "# Pic

no images any more
",
    );
    run(opts(b.path(), &server, None)).await.unwrap();
    run(opts(a.path(), &server, None)).await.unwrap();
    let a_store = Store::open(a.path().join(".notes/local.db")).unwrap();
    let entries = a_store.load_vault_doc(vault_id).unwrap().attachment_entries();
    assert_eq!(entries.iter().map(|(p, _)| p.as_str()).collect::<Vec<_>>(), vec!["sub/img/two.bin"]);
    assert!(a.path().join("attachments/logo.png").exists(), "local files are never deleted by cleanup");
    drop(a_store);

    let within = notes_server::purge_orphans(&state, now + 1000, Duration::from_secs(3600)).await.unwrap();
    assert_eq!((within.newly_orphaned, within.purged), (1, 0));
    let later =
        notes_server::purge_orphans(&state, now + 3_600_000 + 2000, Duration::from_secs(3600)).await.unwrap();
    assert_eq!(later.purged, 2);
    assert!(!state.attachments.exists(vault_id, &h1) && !state.attachments.exists(vault_id, &h2));
    assert!(state.attachments.exists(vault_id, &h_two));
    let store = state.store.lock().await;
    assert!(store.attachment(vault_id, &h2).unwrap().is_none());
    assert!(store.attachment(vault_id, &h_two).unwrap().is_some());
}

/// `wss://` + `https://` against a TLS server signed by a private CA, trusted via `ca_cert`.
#[tokio::test]
async fn wss_with_private_ca() {
    notes_core::tls::install_crypto_provider();
    let cert =
        rcgen::generate_simple_self_signed(vec!["localhost".to_owned(), "127.0.0.1".to_owned()]).unwrap();
    let ca_file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(ca_file.path(), cert.cert.pem()).unwrap();
    let tls = axum_server::tls_rustls::RustlsConfig::from_pem(
        cert.cert.pem().into_bytes(),
        cert.signing_key.serialize_pem().into_bytes(),
    )
    .await
    .unwrap();

    let blobs = tempfile::tempdir().unwrap();
    let options = ServerOptions { attachments_dir: blobs.path().to_path_buf(), ..ServerOptions::default() };
    let state = build_state(Store::open_in_memory().unwrap(), options);
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router(state.clone());
    let server_task = axum_server::from_tcp_rustls(listener, tls).unwrap().serve(app.into_make_service());
    tokio::spawn(async move { server_task.await.unwrap() });
    let server = format!("https://{addr}");

    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    std::fs::write(a.path().join("secure.md"), "![[blob.bin]]\n").unwrap();
    std::fs::write(a.path().join("blob.bin"), b"over tls").unwrap();

    // Public roots do not know this CA → the handshake must fail.
    let err = run(opts(a.path(), &server, None)).await.unwrap_err();
    assert!(err.to_string().contains("cannot connect"), "{err}");

    let with_ca = |dir: &Path, id| SyncOptions {
        ca_cert: Some(ca_file.path().to_path_buf()),
        ..opts(dir, &server, id)
    };
    let report = run(with_ca(a.path(), None)).await.unwrap();
    run(with_ca(b.path(), Some(report.vault_id))).await.unwrap();
    assert_eq!(read(b.path(), "secure.md"), "![[blob.bin]]\n");
    assert_eq!(std::fs::read(b.path().join("blob.bin")).unwrap(), b"over tls");
}

#[tokio::test]
async fn once_fails_fast_when_server_is_down() {
    let dir = tempfile::tempdir().unwrap();
    let err = run(opts(dir.path(), "http://127.0.0.1:9", None)).await.unwrap_err();
    assert!(err.to_string().contains("cannot connect"), "{err}");
}

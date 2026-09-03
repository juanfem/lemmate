//! One relay, every vault (SPEC §9, §14): the desktop opens each vault the account can read in
//! its own folder, behind one relay, and the UI sees the workspace it sees against the server —
//! one socket, one vault list, search across the lot.

use std::net::SocketAddr;
use std::path::Path;
use std::time::Duration;

use futures_util::SinkExt;
use lemmate_core::client::{LocalHandle, LocalOptions, SyncOptions, start_many};
use lemmate_core::sync::{Frame, Message, SyncMessage};
use lemmate_core::vault_doc::VaultDoc;
use lemmate_core::{DocId, NoteDoc, NoteId, Store, VaultId};
use lemmate_server::{ServerOptions, build_state, router};
use serde_json::Value;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message as TMsg;

async fn server() -> (SocketAddr, std::sync::Arc<lemmate_server::AppState>) {
    let state = build_state(Store::open_in_memory().unwrap(), ServerOptions::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router(state.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (addr, state)
}

/// A relay over `dirs`, one engine per folder. `root` is where a vault the UI creates lands;
/// `None` refuses creation, the way a single-vault shell does.
async fn relay(server: SocketAddr, dirs: &[&Path], root: Option<&Path>) -> LocalHandle {
    let opts = dirs
        .iter()
        .map(|dir| SyncOptions {
            vault_dir: dir.to_path_buf(),
            server_url: Some(format!("http://{server}")),
            vault_id: None,
            once: false,
            ca_cert: None,
            token: None,
        })
        .collect();
    let local = LocalOptions {
        bind: "127.0.0.1:0".parse().unwrap(),
        web_dir: None,
        vault_root: root.map(Path::to_path_buf),
        config_path: None,
    };
    start_many(opts, local).await.unwrap()
}

/// Frames the way a local UI sends them.
async fn send(
    ws: &mut tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    doc: DocId,
    update: Vec<u8>,
) {
    let m = Message::Sync(SyncMessage::Update(update));
    ws.send(TMsg::Binary(Frame::new(doc.to_string(), &m).encode().into())).await.unwrap();
}

async fn get(url: String) -> (u16, Value) {
    tokio::task::spawn_blocking(move || match ureq::get(&url).call() {
        Ok(mut r) => {
            let status = r.status().as_u16();
            let text = r.body_mut().read_to_string().unwrap_or_default();
            (status, serde_json::from_str(&text).unwrap_or(Value::Null))
        }
        Err(ureq::Error::StatusCode(c)) => (c, Value::Null),
        Err(e) => panic!("{e}"),
    })
    .await
    .unwrap()
}

async fn post(url: String, body: Value) -> (u16, Value) {
    tokio::task::spawn_blocking(move || {
        match ureq::post(&url).header("content-type", "application/json").send(body.to_string().as_bytes()) {
            Ok(mut r) => {
                let status = r.status().as_u16();
                let text = r.body_mut().read_to_string().unwrap_or_default();
                (status, serde_json::from_str(&text).unwrap_or(Value::Null))
            }
            Err(ureq::Error::StatusCode(c)) => (c, Value::Null),
            Err(e) => panic!("{e}"),
        }
    })
    .await
    .unwrap()
}

/// Poll until `f` holds, so a slow machine does not decide the outcome.
async fn until(what: &str, mut f: impl FnMut() -> bool) {
    for i in 0..100 {
        if f() {
            return;
        }
        assert!(i < 99, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test]
async fn every_vault_is_listed_addressed_and_searched_through_one_relay() {
    let (srv, state) = server().await;
    let root = tempfile::tempdir().unwrap();
    let (work, home) = (root.path().join("Work"), root.path().join("Home"));
    std::fs::create_dir_all(&work).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    let handle = relay(srv, &[&work, &home], None).await;
    assert_eq!(handle.vaults.len(), 2);
    let (a, b) = (handle.vaults[0], handle.vaults[1]);
    assert_ne!(a, b);
    let base = format!("http://{}/api/v1", handle.addr);

    // The vault list is the whole workspace, not one vault.
    let (s, vaults) = get(format!("{base}/vaults")).await;
    assert_eq!(s, 200);
    let listed: Vec<&str> = vaults.as_array().unwrap().iter().map(|v| v["id"].as_str().unwrap()).collect();
    assert_eq!(listed, vec![a.to_string(), b.to_string()]);

    // A write is addressed by vault, and lands in that vault's folder — not the other's.
    let (s, note) = post(
        format!("{base}/vaults/{a}/notes"),
        serde_json::json!({"path": "Quarterly", "content": "# Q3\n\nrevenue\n"}),
    )
    .await;
    assert_eq!(s, 201, "{note}");
    assert!(work.join("Quarterly.md").is_file());
    assert!(!home.join("Quarterly.md").exists());

    let (s, note) = post(
        format!("{base}/vaults/{b}/notes"),
        serde_json::json!({"path": "Recipes", "content": "# Bread\n\nrevenue of flour\n"}),
    )
    .await;
    assert_eq!(s, 201, "{note}");
    assert!(home.join("Recipes.md").is_file());

    // A vault this relay does not hold is a 404, not somebody else's note.
    let stranger = VaultId::new();
    assert_eq!(get(format!("{base}/vaults/{stranger}/notes")).await.0, 404);

    // Cross-vault search (SPEC §10) — the endpoint the web client calls, which a relay serving
    // one vault never had.
    let (s, hits) = get(format!("{base}/search?q=revenue")).await;
    assert_eq!(s, 200);
    let found = hits.as_array().unwrap().len();
    assert_eq!(found, 2, "both vaults answer one search: {hits}");

    // Both vaults reach the server, each as itself.
    for i in 0..100 {
        let (in_a, in_b) = {
            let store = state.store.lock().await;
            (store.list_notes(a).unwrap().len(), store.list_notes(b).unwrap().len())
        };
        if in_a == 1 && in_b == 1 {
            break;
        }
        assert!(i < 99, "timed out waiting for both vaults to reach the server");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    handle.abort();
}

/// The UI writes a new note's text *before* its vault entry, so the relay sees frames for a note
/// no vault has claimed yet. With more than one vault it cannot know where they belong, so it
/// holds them until the vault entry says — dropping them would lose what was typed.
#[tokio::test]
async fn a_note_created_over_the_socket_reaches_the_vault_that_claims_it() {
    let (srv, _state) = server().await;
    let root = tempfile::tempdir().unwrap();
    let (one, two) = (root.path().join("One"), root.path().join("Two"));
    std::fs::create_dir_all(&one).unwrap();
    std::fs::create_dir_all(&two).unwrap();
    let handle = relay(srv, &[&one, &two], None).await;
    let (a, b) = (handle.vaults[0], handle.vaults[1]);

    let (mut ws, _) = connect_async(format!("ws://{}/ws", handle.addr)).await.unwrap();
    // Text first, addressed only by note id: neither vault has heard of it.
    let id = NoteId::new();
    let note = NoteDoc::new();
    note.set_text("# Held\n\nwritten before the vault entry\n");
    send(&mut ws, DocId::Note(id), note.encode_full()).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!one.join("held.md").exists() && !two.join("held.md").exists(), "nobody owns it yet");

    // Then the vault entry, which says it belongs to the second vault.
    let vault = VaultDoc::new();
    let update = vault.set_path(id, "held.md");
    send(&mut ws, DocId::Vault(b), update).await;

    until("the held text to reach the vault that claimed the note", || {
        std::fs::read_to_string(two.join("held.md"))
            .map(|t| t.contains("written before the vault entry"))
            .unwrap_or(false)
    })
    .await;
    assert!(!one.join("held.md").exists(), "the other vault never saw it: {a}");
    handle.abort();
}

/// "New vault" in the tree mints an id in the browser and speaks it over the socket — there is
/// no REST call to intercept. The relay holds those frames and opens a folder and an engine for
/// the vault, or everything typed into it would be dropped on the floor.
#[tokio::test]
async fn a_vault_the_ui_creates_gets_a_folder_and_an_engine() {
    let (srv, _state) = server().await;
    let root = tempfile::tempdir().unwrap();
    let first = root.path().join("Notes");
    std::fs::create_dir_all(&first).unwrap();
    let handle = relay(srv, &[&first], Some(root.path())).await;

    let (mut ws, _) = connect_async(format!("ws://{}/ws", handle.addr)).await.unwrap();
    let fresh = VaultId::new();
    let id = NoteId::new();
    let vault = VaultDoc::new();
    let note = NoteDoc::new();
    note.set_text("# Idea\n\nkept\n");

    // Exactly the order the UI writes in: the note's text, then the vault that claims it.
    send(&mut ws, DocId::Note(id), note.encode_full()).await;
    send(&mut ws, DocId::Vault(fresh), vault.set_path(id, "idea.md")).await;

    let dir = root.path().join(lemmate_core::vaults::default_folder_name(fresh));
    until("the new vault's folder and note", || {
        std::fs::read_to_string(dir.join("idea.md")).map(|t| t.contains("kept")).unwrap_or(false)
    })
    .await;
    assert!(dir.join(".lemmate/local.db").is_file(), "the folder is a vault, not just a folder");

    // And it is part of the workspace from now on, without a restart.
    let (s, vaults) = get(format!("http://{}/api/v1/vaults", handle.addr)).await;
    assert_eq!(s, 200);
    let ids: Vec<&str> = vaults.as_array().unwrap().iter().map(|v| v["id"].as_str().unwrap()).collect();
    assert!(ids.contains(&fresh.to_string().as_str()), "listed: {ids:?}");
    handle.abort();
}

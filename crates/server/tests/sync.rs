//! End-to-end: two WebSocket clients sync one note through the relay, and the update log persists.

use std::net::SocketAddr;

use futures_util::{SinkExt, StreamExt};
use lemmate_core::sync::{Frame, Message, SyncMessage};
use lemmate_core::{DocId, NoteDoc, NoteId, Store, VaultId};
use lemmate_server::{ServerOptions, build_state, router};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message as TMsg;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

type Client = WebSocketStream<MaybeTlsStream<TcpStream>>;

async fn start() -> (SocketAddr, std::sync::Arc<lemmate_server::AppState>) {
    let state = build_state(Store::open_in_memory().unwrap(), ServerOptions::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router(state.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (addr, state)
}

async fn connect(addr: SocketAddr) -> Client {
    connect_async(format!("ws://{addr}/ws")).await.unwrap().0
}

async fn send(c: &mut Client, doc: &str, m: Message) {
    c.send(TMsg::Binary(Frame::new(doc, &m).encode().into())).await.unwrap();
}

async fn recv(c: &mut Client) -> (String, Message) {
    let msg = tokio::time::timeout(std::time::Duration::from_secs(5), c.next())
        .await
        .expect("timeout")
        .unwrap()
        .unwrap();
    let TMsg::Binary(b) = msg else { panic!("expected binary, got {msg:?}") };
    let f = Frame::decode(&b).unwrap();
    let m = f.message().unwrap();
    (f.doc_id, m)
}

/// Standard handshake: send our SyncStep1, apply the server's SyncStep2, answer its SyncStep1.
async fn handshake(c: &mut Client, doc_id: &str, doc: &NoteDoc) {
    send(c, doc_id, Message::Sync(SyncMessage::SyncStep1(doc.state_vector()))).await;
    let (_, step2) = recv(c).await;
    let Message::Sync(SyncMessage::SyncStep2(u)) = step2 else { panic!("expected SyncStep2, got {step2:?}") };
    doc.apply_update(&u).unwrap();
    let (_, step1) = recv(c).await;
    let Message::Sync(SyncMessage::SyncStep1(sv)) = step1 else {
        panic!("expected SyncStep1, got {step1:?}")
    };
    let diff = doc.diff_since(&sv);
    send(c, doc_id, Message::Sync(SyncMessage::SyncStep2(diff))).await;
    fence(c, doc_id, doc).await;
}

/// Ordering fence: a second SyncStep1 on the same connection is answered only after the server
/// has processed everything we sent before it (frames on one connection are handled in order).
async fn fence(c: &mut Client, doc_id: &str, doc: &NoteDoc) {
    send(c, doc_id, Message::Sync(SyncMessage::SyncStep1(doc.state_vector()))).await;
    let (_, step2) = recv(c).await;
    let Message::Sync(SyncMessage::SyncStep2(u)) = step2 else { panic!("expected SyncStep2, got {step2:?}") };
    doc.apply_update(&u).unwrap();
    let (_, step1) = recv(c).await;
    assert!(matches!(step1, Message::Sync(SyncMessage::SyncStep1(_))), "expected SyncStep1, got {step1:?}");
}

#[tokio::test]
async fn two_clients_converge_and_updates_persist() {
    let (addr, state) = start().await;
    let note = NoteId::new();
    let doc_id = DocId::Note(note).to_string();

    // Client A creates content and syncs it up.
    let mut a = connect(addr).await;
    let doc_a = NoteDoc::new();
    doc_a.set_text("# Hello\n\nfrom A\n");
    handshake(&mut a, &doc_id, &doc_a).await;

    // Client B starts empty and receives A's state through the handshake.
    let mut b = connect(addr).await;
    let doc_b = NoteDoc::new();
    handshake(&mut b, &doc_id, &doc_b).await;
    assert_eq!(doc_b.text(), "# Hello\n\nfrom A\n");

    // A live edit from A is fanned out to B.
    let update = doc_a.set_text("# Hello\n\nfrom A, edited\n");
    send(&mut a, &doc_id, Message::Sync(SyncMessage::Update(update))).await;
    let (id, m) = recv(&mut b).await;
    assert_eq!(id, doc_id);
    let Message::Sync(SyncMessage::Update(u)) = m else { panic!("expected Update, got {m:?}") };
    doc_b.apply_update(&u).unwrap();
    assert_eq!(doc_b.text(), doc_a.text());

    // Concurrent edits from both sides converge.
    let ua = doc_a.set_text("# Hello!\n\nfrom A, edited\n");
    let ub = doc_b.set_text("# Hello\n\nfrom A, edited\nand B\n");
    send(&mut a, &doc_id, Message::Sync(SyncMessage::Update(ua))).await;
    send(&mut b, &doc_id, Message::Sync(SyncMessage::Update(ub))).await;
    let (_, ma) = recv(&mut a).await;
    let (_, mb) = recv(&mut b).await;
    if let Message::Sync(SyncMessage::Update(u)) = ma {
        doc_a.apply_update(&u).unwrap();
    } else {
        panic!()
    }
    if let Message::Sync(SyncMessage::Update(u)) = mb {
        doc_b.apply_update(&u).unwrap();
    } else {
        panic!()
    }
    assert_eq!(doc_a.text(), doc_b.text());
    assert_eq!(doc_a.text(), "# Hello!\n\nfrom A, edited\nand B\n");

    // The server persisted everything: a cold load from the store matches.
    let reloaded = state.store.lock().await.load_doc(DocId::Note(note)).unwrap();
    assert_eq!(reloaded.text(), doc_a.text());
}

#[tokio::test]
async fn malformed_frames_are_ignored_and_connection_survives() {
    let (addr, _) = start().await;
    let mut c = connect(addr).await;
    c.send(TMsg::Binary(vec![0, 9, b'x'].into())).await.unwrap();
    c.send(TMsg::Binary(Frame { doc_id: "not-a-ulid".into(), payload: vec![] }.encode().into()))
        .await
        .unwrap();
    let doc = NoteDoc::new();
    handshake(&mut c, &DocId::Note(NoteId::new()).to_string(), &doc).await;
    assert_eq!(doc.text(), "");
}

/// The REST write side: create/replace/rename/delete land in the CRDT stream and reach a
/// connected client like any other update.
#[tokio::test]
async fn rest_writes_are_crdt_edits() {
    let (addr, state) = start().await;
    let vault = VaultId::new();
    let vdoc_id = DocId::Vault(vault).to_string();
    let mut c = connect(addr).await;
    let vdoc = lemmate_core::VaultDoc::new();
    // Subscribe to the vault doc first.
    send(&mut c, &vdoc_id, Message::Sync(SyncMessage::SyncStep1(vdoc.state_vector()))).await;
    let _ = recv(&mut c).await;
    let _ = recv(&mut c).await;

    let base = format!("http://{addr}/api/v1/vaults/{vault}");
    let created: serde_json::Value = tokio::task::spawn_blocking({
        let base = base.clone();
        move || {
            let mut r = ureq::post(format!("{base}/notes"))
                .header("content-type", "application/json")
                .send(r##"{"path":"Inbox/From API","content":"# Hello\n\nvia REST\n"}"##.as_bytes())
                .unwrap();
            assert_eq!(r.status().as_u16(), 201);
            serde_json::from_str(&r.body_mut().read_to_string().unwrap()).unwrap()
        }
    })
    .await
    .unwrap();
    assert_eq!(created["path"], "Inbox/From API.md");
    assert!(created["content"].as_str().unwrap().starts_with("---\nid: "));
    let id = created["id"].as_str().unwrap().to_owned();

    // The connected client receives the vault entry.
    let (doc, m) = recv(&mut c).await;
    assert_eq!(doc, vdoc_id);
    let Message::Sync(SyncMessage::Update(u)) = m else { panic!("expected vault update, got {m:?}") };
    vdoc.apply_update(&u).unwrap();
    assert_eq!(vdoc.path_of(id.parse().unwrap()).as_deref(), Some("Inbox/From API.md"));

    // Replace merges as a diff: a concurrent client edit survives.
    let ndoc = NoteDoc::new();
    send(&mut c, &id, Message::Sync(SyncMessage::SyncStep1(ndoc.state_vector()))).await;
    let Some((_, Message::Sync(SyncMessage::SyncStep2(u)))) = Some(recv(&mut c).await) else { panic!() };
    ndoc.apply_update(&u).unwrap();
    let _ = recv(&mut c).await;
    let mine = ndoc.set_text(&format!("{}client line\n", ndoc.text()));
    send(&mut c, &id, Message::Sync(SyncMessage::Update(mine))).await;
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    let replaced: serde_json::Value = tokio::task::spawn_blocking({
        let base = base.clone();
        let id = id.clone();
        move || {
            let mut r = ureq::put(format!("{base}/notes/{id}"))
                .header("content-type", "application/json")
                .send(r##"{"content":"# Hello!\n\nvia REST\nclient line\n"}"##.as_bytes())
                .unwrap();
            serde_json::from_str(&r.body_mut().read_to_string().unwrap()).unwrap()
        }
    })
    .await
    .unwrap();
    let (_, m) = recv(&mut c).await;
    let Message::Sync(SyncMessage::Update(u)) = m else { panic!("expected note update, got {m:?}") };
    ndoc.apply_update(&u).unwrap();
    assert!(ndoc.text().contains("# Hello!") && ndoc.text().contains("client line"), "{}", ndoc.text());
    assert_eq!(replaced["title"], "Hello!");

    // Rename, daily get-or-create, delete.
    let status = tokio::task::spawn_blocking({
        let base = base.clone();
        let id = id.clone();
        move || {
            ureq::patch(format!("{base}/notes/{id}"))
                .header("content-type", "application/json")
                .send(r##"{"path":"Archive/Moved"}"##.as_bytes())
                .map(|r| r.status().as_u16())
                .unwrap()
        }
    })
    .await
    .unwrap();
    assert_eq!(status, 204);
    let (_, m) = recv(&mut c).await;
    if let Message::Sync(SyncMessage::Update(u)) = m {
        vdoc.apply_update(&u).unwrap();
    }
    assert_eq!(vdoc.path_of(id.parse().unwrap()).as_deref(), Some("Archive/Moved.md"));
    let daily: serde_json::Value = tokio::task::spawn_blocking({
        let base = base.clone();
        move || {
            serde_json::from_str(
                &ureq::get(format!("{base}/daily/2026-08-30"))
                    .call()
                    .unwrap()
                    .body_mut()
                    .read_to_string()
                    .unwrap(),
            )
            .unwrap()
        }
    })
    .await
    .unwrap();
    assert_eq!(daily["path"], "Daily/2026-08-30.md");
    let again: serde_json::Value = tokio::task::spawn_blocking({
        let base = base.clone();
        move || {
            serde_json::from_str(
                &ureq::get(format!("{base}/daily/2026-08-30"))
                    .call()
                    .unwrap()
                    .body_mut()
                    .read_to_string()
                    .unwrap(),
            )
            .unwrap()
        }
    })
    .await
    .unwrap();
    assert_eq!(again["id"], daily["id"], "get-or-create is idempotent");
    let status = tokio::task::spawn_blocking({
        let base = base.clone();
        let id = id.clone();
        move || ureq::delete(format!("{base}/notes/{id}")).call().map(|r| r.status().as_u16()).unwrap()
    })
    .await
    .unwrap();
    assert_eq!(status, 204);
    assert!(state.store.lock().await.note_by_id(id.parse().unwrap()).unwrap().is_none(), "trashed");
}

/// Export goes through pandoc when the server has one, else answers 501.
#[tokio::test]
async fn export_uses_pandoc_or_says_so() {
    let pandoc = std::env::var_os("LEMMATE_TEST_PANDOC").map(std::path::PathBuf::from);
    // What decides the answer is whether the server can reach *a* pandoc, not whether the
    // variable happens to be set: with `pandoc: None` it falls back to `pandoc` on PATH, so a
    // machine that has one installed gets an export either way. Asking the same question the
    // server asks keeps this test honest on both kinds of machine.
    let available = lemmate_core::pandoc::pandoc_available(pandoc.as_deref());
    let options = ServerOptions { pandoc: pandoc.clone(), ..ServerOptions::default() };
    let state = build_state(Store::open_in_memory().unwrap(), options);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router(state.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let vault = VaultId::new();
    let base = format!("http://{addr}/api/v1/vaults/{vault}");
    let created: serde_json::Value = tokio::task::spawn_blocking({
        let base = base.clone();
        move || {
            let mut r = ureq::post(format!("{base}/notes"))
                .header("content-type", "application/json")
                .send(
                    r##"{"path":"Export me","content":"# Exported\n\nwith $x^2$ and [[Other|link]]\n"}"##
                        .as_bytes(),
                )
                .unwrap();
            serde_json::from_str(&r.body_mut().read_to_string().unwrap()).unwrap()
        }
    })
    .await
    .unwrap();
    let id = created["id"].as_str().unwrap().to_owned();
    let (status, body, ctype) = tokio::task::spawn_blocking({
        let base = base.clone();
        move || match ureq::post(format!("{base}/notes/{id}/export"))
            .header("content-type", "application/json")
            .send(r#"{"format":"html"}"#.as_bytes())
        {
            Ok(mut r) => {
                let ct =
                    r.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("").to_owned();
                (r.status().as_u16(), r.body_mut().read_to_string().unwrap_or_default(), ct)
            }
            Err(ureq::Error::StatusCode(c)) => (c, String::new(), String::new()),
            Err(e) => panic!("{e}"),
        }
    })
    .await
    .unwrap();
    if available {
        assert_eq!(status, 200);
        assert!(ctype.starts_with("text/html"), "{ctype}");
        assert!(body.contains("<h1") && body.contains("link"), "{body}");
        assert!(!body.contains("id: 01"), "front matter stripped");
    } else {
        assert_eq!(status, 501, "no pandoc reachable, so the server must say so rather than fail");
    }
}

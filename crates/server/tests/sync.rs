//! End-to-end: two WebSocket clients sync one note through the relay, and the update log persists.

use std::net::SocketAddr;

use futures_util::{SinkExt, StreamExt};
use notes_core::sync::{Frame, Message, SyncMessage};
use notes_core::{DocId, NoteDoc, NoteId, Store};
use notes_server::{build_state, router};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message as TMsg;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

type Client = WebSocketStream<MaybeTlsStream<TcpStream>>;

async fn start() -> (SocketAddr, std::sync::Arc<notes_server::AppState>) {
    let state = build_state(Store::open_in_memory().unwrap());
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

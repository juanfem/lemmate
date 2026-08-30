//! Accounts and permissions end to end: REST and the WebSocket relay (SPEC §11).

use std::net::SocketAddr;
use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use notes_core::sync::{Frame, Message, SyncMessage};
use notes_core::{DocId, NoteDoc, NoteId, Store, VaultId};
use notes_server::{AppState, AuthMode, ServerOptions, build_state, router};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message as TMsg;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

async fn start() -> (SocketAddr, Arc<AppState>) {
    let options = ServerOptions {
        auth: AuthMode::Enabled { allow_registration: false, secure_cookies: false },
        ..ServerOptions::default()
    };
    let state = build_state(Store::open_in_memory().unwrap(), options);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router(state.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (addr, state)
}

async fn post(addr: SocketAddr, path: &str, body: Value, token: Option<&str>) -> (u16, Value) {
    let mut req = ureq::post(format!("http://{addr}{path}")).header("content-type", "application/json");
    if let Some(t) = token {
        req = req.header("authorization", &format!("Bearer {t}"));
    }
    let body = body.to_string();
    tokio::task::spawn_blocking(move || match req.send(body.as_bytes()) {
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

async fn get(addr: SocketAddr, path: &str, token: Option<&str>) -> (u16, Value) {
    let mut req = ureq::get(format!("http://{addr}{path}"));
    if let Some(t) = token {
        req = req.header("authorization", &format!("Bearer {t}"));
    }
    tokio::task::spawn_blocking(move || match req.call() {
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

async fn put(addr: SocketAddr, path: &str, body: Value, token: &str) -> u16 {
    let req = ureq::put(format!("http://{addr}{path}"))
        .header("content-type", "application/json")
        .header("authorization", &format!("Bearer {token}"));
    let body = body.to_string();
    tokio::task::spawn_blocking(move || match req.send(body.as_bytes()) {
        Ok(r) => r.status().as_u16(),
        Err(ureq::Error::StatusCode(c)) => c,
        Err(e) => panic!("{e}"),
    })
    .await
    .unwrap()
}

type Client = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn connect(addr: SocketAddr, token: &str) -> Client {
    let mut req = format!("ws://{addr}/ws").into_client_request().unwrap();
    req.headers_mut().insert("authorization", format!("Bearer {token}").parse().unwrap());
    tokio_tungstenite::connect_async(req).await.unwrap().0
}

async fn send(c: &mut Client, doc: &str, m: Message) {
    c.send(TMsg::Binary(Frame::new(doc, &m).encode().into())).await.unwrap();
}

async fn recv(c: &mut Client) -> Option<(String, Message)> {
    let msg = tokio::time::timeout(std::time::Duration::from_millis(1500), c.next()).await.ok()??.ok()?;
    let TMsg::Binary(b) = msg else { return None };
    let f = Frame::decode(&b).ok()?;
    let m = f.message().ok()?;
    Some((f.doc_id, m))
}

#[tokio::test]
async fn accounts_and_roles() {
    let (addr, _state) = start().await;

    // No session → 401 on the API and on the socket upgrade.
    assert_eq!(get(addr, "/api/v1/vaults", None).await.0, 401);
    let bad = format!("ws://{addr}/ws").into_client_request().unwrap();
    assert!(tokio_tungstenite::connect_async(bad).await.is_err());

    // First account registers freely and is the admin; the second needs the admin.
    let (s, body) = post(
        addr,
        "/api/v1/auth/register",
        json!({"email": "Ann@Example.org", "password": "correct horse"}),
        None,
    )
    .await;
    assert_eq!(s, 200, "{body}");
    let ann = body["token"].as_str().unwrap().to_owned();
    assert_eq!(body["user"]["is_admin"], true);
    assert_eq!(
        post(
            addr,
            "/api/v1/auth/register",
            json!({"email": "bob@example.org", "password": "another pass"}),
            None
        )
        .await
        .0,
        403
    );
    let (s, body) = post(
        addr,
        "/api/v1/auth/register",
        json!({"email": "bob@example.org", "password": "another pass"}),
        Some(&ann),
    )
    .await;
    assert_eq!(s, 200);
    assert_eq!(body["is_admin"], false);
    assert!(body.get("token").is_none(), "admin-created accounts are not logged in as the admin");
    assert_eq!(
        post(addr, "/api/v1/auth/login", json!({"email": "bob@example.org", "password": "wrong"}), None)
            .await
            .0,
        401
    );
    let (s, body) = post(
        addr,
        "/api/v1/auth/login",
        json!({"email": "BOB@example.org", "password": "another pass"}),
        None,
    )
    .await;
    assert_eq!(s, 200);
    let bob = body["token"].as_str().unwrap().to_owned();
    assert_eq!(get(addr, "/api/v1/auth/me", Some(&bob)).await.1["email"], "bob@example.org");

    // Ann creates a vault simply by syncing a new id: she becomes its owner.
    let vault = VaultId::new();
    let vdoc = format!("vault:{vault}");
    let mut a = connect(addr, &ann).await;
    let doc_a = NoteDoc::new();
    send(&mut a, &vdoc, Message::Sync(SyncMessage::SyncStep1(doc_a.state_vector()))).await;
    assert!(matches!(recv(&mut a).await, Some((_, Message::Sync(SyncMessage::SyncStep2(_))))));
    assert!(matches!(recv(&mut a).await, Some((_, Message::Sync(SyncMessage::SyncStep1(_))))));
    let (_, members) = get(addr, &format!("/api/v1/vaults/{vault}/members"), Some(&ann)).await;
    assert_eq!(members[0]["role"], "owner");
    assert_eq!(get(addr, "/api/v1/vaults", Some(&ann)).await.1.as_array().unwrap().len(), 1);
    assert_eq!(get(addr, "/api/v1/vaults", Some(&bob)).await.1.as_array().unwrap().len(), 0);

    // Bob is not a member: the API hides the vault and the relay denies the doc.
    assert_eq!(get(addr, &format!("/api/v1/vaults/{vault}/notes"), Some(&bob)).await.0, 404);
    let mut b = connect(addr, &bob).await;
    send(&mut b, &vdoc, Message::Sync(SyncMessage::SyncStep1(NoteDoc::new().state_vector()))).await;
    assert!(matches!(recv(&mut b).await, Some((_, Message::Auth(Some(_))))), "expected a denial");

    // Only the owner can add members; a viewer can read but not write.
    assert_eq!(
        put(
            addr,
            &format!("/api/v1/vaults/{vault}/members"),
            json!({"email": "bob@example.org", "role": "viewer"}),
            &bob
        )
        .await,
        404
    );
    assert_eq!(
        put(
            addr,
            &format!("/api/v1/vaults/{vault}/members"),
            json!({"email": "bob@example.org", "role": "viewer"}),
            &ann
        )
        .await,
        204
    );
    assert_eq!(get(addr, &format!("/api/v1/vaults/{vault}/notes"), Some(&bob)).await.0, 200);
    let note = DocId::Note(NoteId::new()).to_string();
    // Ann writes a note in the vault.
    let text_a = NoteDoc::new();
    let u = text_a.set_text("owner text");
    send(&mut a, &note, Message::Sync(SyncMessage::Update(u))).await;
    // Bob (viewer) can read it...
    let doc_b = NoteDoc::new();
    send(&mut b, &vdoc, Message::Sync(SyncMessage::SyncStep1(doc_b.state_vector()))).await;
    assert!(matches!(recv(&mut b).await, Some((_, Message::Sync(SyncMessage::SyncStep2(_))))));
    assert!(matches!(recv(&mut b).await, Some((_, Message::Sync(SyncMessage::SyncStep1(_))))));
    let nb = NoteDoc::new();
    send(&mut b, &note, Message::Sync(SyncMessage::SyncStep1(nb.state_vector()))).await;
    let Some((_, Message::Sync(SyncMessage::SyncStep2(u)))) = recv(&mut b).await else {
        panic!("viewer should read")
    };
    nb.apply_update(&u).unwrap();
    assert_eq!(nb.text(), "owner text");
    let _ = recv(&mut b).await; // server's SyncStep1
    // ...but his write is refused and never reaches Ann.
    let bad = nb.set_text("owner text (defaced)");
    send(&mut b, &note, Message::Sync(SyncMessage::Update(bad))).await;
    assert!(matches!(recv(&mut b).await, Some((_, Message::Auth(Some(_))))));
    assert_eq!(
        put(addr, &format!("/api/v1/vaults/{vault}/attachments/{}", "0".repeat(64)), json!({}), &bob).await,
        403
    );

    // Promoted to editor, a write goes through and reaches the owner. A client whose write was
    // refused has diverged (its later edits depend on the rejected one), so it resyncs first —
    // exactly what the UI does on a denial.
    assert_eq!(
        put(
            addr,
            &format!("/api/v1/vaults/{vault}/members"),
            json!({"email": "bob@example.org", "role": "editor"}),
            &ann
        )
        .await,
        204
    );
    send(&mut a, &note, Message::Sync(SyncMessage::SyncStep1(text_a.state_vector()))).await;
    let _ = recv(&mut a).await;
    let _ = recv(&mut a).await;
    let nb = NoteDoc::new();
    send(&mut b, &note, Message::Sync(SyncMessage::SyncStep1(nb.state_vector()))).await;
    let Some((_, Message::Sync(SyncMessage::SyncStep2(u)))) = recv(&mut b).await else {
        panic!("editor should read")
    };
    nb.apply_update(&u).unwrap();
    let _ = recv(&mut b).await;
    assert_eq!(nb.text(), "owner text");
    let good = nb.set_text("owner text (edited by bob)");
    send(&mut b, &note, Message::Sync(SyncMessage::Update(good))).await;
    let Some((_, Message::Sync(SyncMessage::Update(u)))) = recv(&mut a).await else {
        panic!("owner should receive the editor's update")
    };
    text_a.apply_update(&u).unwrap();
    assert_eq!(text_a.text(), "owner text (edited by bob)");

    // Logout invalidates the session; the last owner cannot leave.
    assert_eq!(post(addr, "/api/v1/auth/logout", json!({}), Some(&bob)).await.0, 204);
    assert_eq!(get(addr, "/api/v1/auth/me", Some(&bob)).await.0, 401);
    let ann_id = get(addr, "/api/v1/auth/me", Some(&ann)).await.1["id"].as_str().unwrap().to_owned();
    let del = ureq::delete(format!("http://{addr}/api/v1/vaults/{vault}/members/{ann_id}"))
        .header("authorization", &format!("Bearer {ann}"));
    let status = tokio::task::spawn_blocking(move || match del.call() {
        Ok(r) => r.status().as_u16(),
        Err(ureq::Error::StatusCode(c)) => c,
        Err(e) => panic!("{e}"),
    })
    .await
    .unwrap();
    assert_eq!(status, 409);
}

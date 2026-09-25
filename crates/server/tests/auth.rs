//! Accounts and permissions end to end: REST and the WebSocket relay (SPEC §11).

use std::net::SocketAddr;
use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use lemmate_core::sync::{Frame, Message, SyncMessage};
use lemmate_core::{DocId, NoteDoc, NoteId, Store, VaultDoc, VaultId};
use lemmate_server::{AppState, AuthMode, ServerOptions, build_state, router};
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

    // Per-note shares (SPEC §11.2): Carol is no member, but a direct share lets her read the
    // note (and nothing else); a public link needs no account at all.
    let (s, body) = post(
        addr,
        "/api/v1/auth/register",
        json!({"email": "carol@example.org", "password": "carol pass"}),
        Some(&ann),
    )
    .await;
    assert_eq!(s, 200, "{body}");
    let (_, body) = post(
        addr,
        "/api/v1/auth/login",
        json!({"email": "carol@example.org", "password": "carol pass"}),
        None,
    )
    .await;
    let carol = body["token"].as_str().unwrap().to_owned();
    let note_id = note.clone();
    // The note needs a row (a vault entry) before it can be shared.
    let entry = VaultDoc::new();
    let vu = entry.set_path(note_id.parse().unwrap(), "shared.md");
    send(&mut a, &vdoc, Message::Sync(SyncMessage::Update(vu))).await;
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert_eq!(get(addr, &format!("/api/v1/vaults/{vault}/notes/{note_id}"), Some(&carol)).await.0, 404);
    let s = put(
        addr,
        &format!("/api/v1/vaults/{vault}/notes/{note_id}/shares"),
        json!({"kind": "user", "email": "carol@example.org", "role": "viewer"}),
        &ann,
    )
    .await;
    assert_eq!(s, 200);
    let (s, body) = get(addr, &format!("/api/v1/vaults/{vault}/notes/{note_id}"), Some(&carol)).await;
    assert_eq!(s, 200, "shared note readable");
    assert!(body["content"].as_str().unwrap().contains("edited by bob"));
    assert_eq!(
        get(addr, &format!("/api/v1/vaults/{vault}/notes"), Some(&carol)).await.0,
        404,
        "vault itself stays hidden"
    );
    let (_, mine) = get(addr, "/api/v1/shared-with-me", Some(&carol)).await;
    assert_eq!(mine[0]["path"], "shared.md");
    // ... including over the relay: the note doc syncs, the vault doc does not.
    let mut c = connect(addr, &carol).await;
    send(&mut c, &vdoc, Message::Sync(SyncMessage::SyncStep1(NoteDoc::new().state_vector()))).await;
    assert!(matches!(recv(&mut c).await, Some((_, Message::Auth(Some(_))))));
    let nc = NoteDoc::new();
    send(&mut c, &note_id, Message::Sync(SyncMessage::SyncStep1(nc.state_vector()))).await;
    assert!(
        matches!(recv(&mut c).await, Some((_, Message::Sync(SyncMessage::SyncStep2(_))))),
        "direct share grants the note"
    );
    // Public link: anonymous read, revocable.
    let (s, link) = {
        let req = ureq::put(format!("http://{addr}/api/v1/vaults/{vault}/notes/{note_id}/shares"))
            .header("content-type", "application/json")
            .header("authorization", &format!("Bearer {ann}"));
        let body = json!({"kind": "link"}).to_string();
        tokio::task::spawn_blocking(move || {
            let mut r = req.send(body.as_bytes()).unwrap();
            let v: Value = serde_json::from_str(&r.body_mut().read_to_string().unwrap()).unwrap();
            (r.status().as_u16(), v)
        })
        .await
        .unwrap()
    };
    assert_eq!(s, 200);
    let token = link["link"].as_str().unwrap().rsplit('/').next().unwrap().to_owned();
    let (s, public) = get(addr, &format!("/api/v1/shared/{token}"), None).await;
    assert_eq!(s, 200);
    assert_eq!(public["path"], "shared.md");
    let del = ureq::delete(format!("http://{addr}/api/v1/vaults/{vault}/notes/{note_id}/shares"))
        .header("content-type", "application/json")
        .header("authorization", &format!("Bearer {ann}"));
    let body = json!({"links": true}).to_string();
    let s = tokio::task::spawn_blocking(move || match del.force_send_body().send(body.as_bytes()) {
        Ok(r) => r.status().as_u16(),
        Err(ureq::Error::StatusCode(c)) => c,
        Err(e) => panic!("{e}"),
    })
    .await
    .unwrap();
    assert_eq!(s, 204);
    assert_eq!(get(addr, &format!("/api/v1/shared/{token}"), None).await.0, 404, "revoked");

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

async fn del(addr: SocketAddr, path: &str, token: &str) -> u16 {
    let req =
        ureq::delete(format!("http://{addr}{path}")).header("authorization", &format!("Bearer {token}"));
    tokio::task::spawn_blocking(move || match req.call() {
        Ok(r) => r.status().as_u16(),
        Err(ureq::Error::StatusCode(c)) => c,
        Err(e) => panic!("{e}"),
    })
    .await
    .unwrap()
}

/// Register the first (admin) account and return its token.
async fn admin(addr: SocketAddr) -> String {
    let (s, body) = post(
        addr,
        "/api/v1/auth/register",
        json!({"email": "ann@example.org", "password": "first pass"}),
        None,
    )
    .await;
    assert_eq!(s, 200, "{body}");
    assert_eq!(body["user"]["is_admin"], true);
    body["token"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn changing_your_own_password_needs_the_old_one_and_drops_other_sessions() {
    let (addr, _state) = start().await;
    let ann = admin(addr).await;

    // A second session for the same account, to prove the change signs it out.
    let (s, body) =
        post(addr, "/api/v1/auth/login", json!({"email": "ann@example.org", "password": "first pass"}), None)
            .await;
    assert_eq!(s, 200);
    let other = body["token"].as_str().unwrap().to_owned();
    assert_eq!(get(addr, "/api/v1/auth/me", Some(&other)).await.0, 200);

    // Wrong current password, and a too-short new one, both refused.
    assert_eq!(
        post(
            addr,
            "/api/v1/auth/password",
            json!({"current_password": "nope", "new_password": "long enough"}),
            Some(&ann)
        )
        .await
        .0,
        401
    );
    assert_eq!(
        post(
            addr,
            "/api/v1/auth/password",
            json!({"current_password": "first pass", "new_password": "short"}),
            Some(&ann)
        )
        .await
        .0,
        400
    );
    // Unauthenticated callers do not get to guess at all.
    assert_eq!(
        post(addr, "/api/v1/auth/password", json!({"new_password": "long enough"}), None).await.0,
        401
    );

    let (s, body) = post(
        addr,
        "/api/v1/auth/password",
        json!({"current_password": "first pass", "new_password": "second pass"}),
        Some(&ann),
    )
    .await;
    assert_eq!(s, 200, "{body}");
    assert_eq!(body["sessions_revoked"], 1);

    // The session that made the change survives; the other one is gone.
    assert_eq!(get(addr, "/api/v1/auth/me", Some(&ann)).await.0, 200);
    assert_eq!(get(addr, "/api/v1/auth/me", Some(&other)).await.0, 401);

    // And the new password is the one that works.
    assert_eq!(
        post(addr, "/api/v1/auth/login", json!({"email": "ann@example.org", "password": "first pass"}), None)
            .await
            .0,
        401
    );
    assert_eq!(
        post(
            addr,
            "/api/v1/auth/login",
            json!({"email": "ann@example.org", "password": "second pass"}),
            None
        )
        .await
        .0,
        200
    );
}

#[tokio::test]
async fn an_admin_resets_a_forgotten_password_and_a_user_cannot_reset_anyone_else() {
    let (addr, _state) = start().await;
    let ann = admin(addr).await;
    assert_eq!(
        post(
            addr,
            "/api/v1/auth/register",
            json!({"email": "bob@example.org", "password": "bob's pass"}),
            Some(&ann)
        )
        .await
        .0,
        200
    );
    let (_, body) =
        post(addr, "/api/v1/auth/login", json!({"email": "bob@example.org", "password": "bob's pass"}), None)
            .await;
    let bob = body["token"].as_str().unwrap().to_owned();

    // Bob may not reset Ann, and may not reach an account that does not exist either.
    assert_eq!(
        post(
            addr,
            "/api/v1/auth/password",
            json!({"email": "ann@example.org", "new_password": "hijacked!"}),
            Some(&bob)
        )
        .await
        .0,
        403
    );
    assert_eq!(
        post(
            addr,
            "/api/v1/auth/password",
            json!({"email": "nobody@example.org", "new_password": "long enough"}),
            Some(&ann)
        )
        .await
        .0,
        404
    );

    // The admin resets Bob without knowing his password. Every session of Bob's dies, including
    // the one he is holding — the admin's own is untouched.
    let (s, body) = post(
        addr,
        "/api/v1/auth/password",
        json!({"email": "Bob@Example.org", "new_password": "reset by ann"}),
        Some(&ann),
    )
    .await;
    assert_eq!(s, 200, "{body}");
    assert_eq!(body["sessions_revoked"], 1);
    assert_eq!(get(addr, "/api/v1/auth/me", Some(&bob)).await.0, 401);
    assert_eq!(get(addr, "/api/v1/auth/me", Some(&ann)).await.0, 200);
    assert_eq!(
        post(
            addr,
            "/api/v1/auth/login",
            json!({"email": "bob@example.org", "password": "reset by ann"}),
            None
        )
        .await
        .0,
        200
    );
}

#[tokio::test]
async fn an_invite_opens_registration_exactly_once() {
    let (addr, _state) = start().await;
    let ann = admin(addr).await;

    // Registration is closed without one.
    assert_eq!(
        post(
            addr,
            "/api/v1/auth/register",
            json!({"email": "bob@example.org", "password": "bob's pass"}),
            None
        )
        .await
        .0,
        403
    );

    // Only an admin mints them.
    let (s, body) = post(addr, "/api/v1/invites", json!({}), Some(&ann)).await;
    assert_eq!(s, 200, "{body}");
    let link = body["link"].as_str().unwrap().to_owned();
    let token = link.strip_prefix("/#/invite/").expect("registration URL").to_owned();
    let id = body["id"].as_str().unwrap().to_owned();
    assert_ne!(id, token, "the id is the hash, not the token itself");
    assert_eq!(body["usable"], true);

    // A garbage token is refused, and does not consume the real one.
    assert_eq!(
        post(
            addr,
            "/api/v1/auth/register",
            json!({"email": "bob@example.org", "password": "bob's pass", "invite": "0".repeat(64)}),
            None
        )
        .await
        .0,
        403
    );

    // The invite registers one account, with no session of the admin's involved.
    let (s, body) = post(
        addr,
        "/api/v1/auth/register",
        json!({"email": "bob@example.org", "password": "bob's pass", "invite": token}),
        None,
    )
    .await;
    assert_eq!(s, 200, "{body}");
    assert_eq!(body["user"]["is_admin"], false, "an invited account is never an admin");
    assert!(body["token"].is_string(), "the invited user is signed in");

    // Single use: the same link does not work twice.
    assert_eq!(
        post(
            addr,
            "/api/v1/auth/register",
            json!({"email": "cara@example.org", "password": "cara's pass", "invite": token}),
            None
        )
        .await
        .0,
        403
    );

    // It now lists as spent, names who used it, and cannot be revoked away.
    let (s, body) = get(addr, "/api/v1/invites", Some(&ann)).await;
    assert_eq!(s, 200);
    assert_eq!(body.as_array().unwrap().len(), 1);
    assert_eq!(body[0]["id"], id);
    assert_eq!(body[0]["usable"], false);
    assert_eq!(body[0]["used_by"], "bob@example.org");
    assert!(body[0]["link"].is_null(), "the token is never handed out again");
    assert_eq!(del(addr, &format!("/api/v1/invites/{id}"), &ann).await, 409);
}

#[tokio::test]
async fn invites_expire_are_revocable_and_are_admin_only() {
    let (addr, _state) = start().await;
    let ann = admin(addr).await;
    assert_eq!(
        post(
            addr,
            "/api/v1/auth/register",
            json!({"email": "bob@example.org", "password": "bob's pass"}),
            Some(&ann)
        )
        .await
        .0,
        200
    );
    let (_, body) =
        post(addr, "/api/v1/auth/login", json!({"email": "bob@example.org", "password": "bob's pass"}), None)
            .await;
    let bob = body["token"].as_str().unwrap().to_owned();

    // A non-admin can neither mint nor list nor revoke.
    assert_eq!(post(addr, "/api/v1/invites", json!({}), Some(&bob)).await.0, 403);
    assert_eq!(get(addr, "/api/v1/invites", Some(&bob)).await.0, 403);
    assert_eq!(post(addr, "/api/v1/invites", json!({}), None).await.0, 401);

    // Revoking an unused invite makes its link stop working.
    let (_, body) = post(addr, "/api/v1/invites", json!({"expires_days": 7}), Some(&ann)).await;
    let token = body["link"].as_str().unwrap().strip_prefix("/#/invite/").unwrap().to_owned();
    let id = body["id"].as_str().unwrap().to_owned();
    assert!(body["expires_ms"].as_i64().unwrap() > 0);
    assert_eq!(del(addr, &format!("/api/v1/invites/{id}"), &bob).await, 403);
    assert_eq!(del(addr, &format!("/api/v1/invites/{id}"), &ann).await, 204);
    assert_eq!(del(addr, &format!("/api/v1/invites/{id}"), &ann).await, 404);
    assert_eq!(
        post(
            addr,
            "/api/v1/auth/register",
            json!({"email": "cara@example.org", "password": "cara's pass", "invite": token}),
            None
        )
        .await
        .0,
        403
    );

    // An expired one is refused too, and a duplicate email does not spend a good invite.
    let (_, body) = post(addr, "/api/v1/invites", json!({}), Some(&ann)).await;
    let good = body["link"].as_str().unwrap().strip_prefix("/#/invite/").unwrap().to_owned();
    assert_eq!(
        post(
            addr,
            "/api/v1/auth/register",
            json!({"email": "bob@example.org", "password": "another pass", "invite": good}),
            None
        )
        .await
        .0,
        409,
        "email already taken"
    );
    let (s, body) = post(
        addr,
        "/api/v1/auth/register",
        json!({"email": "cara@example.org", "password": "cara's pass", "invite": good}),
        None,
    )
    .await;
    assert_eq!(s, 200, "the invite survived the 409: {body}");
}

/// A render page opened where there is no session — another browser — goes to the sign-in, with
/// the way back; with a session it is served. The POST the pane uses still answers 401.
#[tokio::test]
async fn a_render_page_without_a_session_goes_to_the_sign_in() {
    let (addr, _) = start().await;
    let path = format!(
        "/api/v1/vaults/{}/notes/{}/render/01M398NHVVT8VC8KH4F8CSJTWS?format=revealjs",
        VaultId::new(),
        NoteId::new()
    );
    let (status, location) = tokio::task::spawn_blocking(move || {
        let agent: ureq::Agent =
            ureq::Agent::config_builder().http_status_as_error(false).max_redirects(0).build().into();
        let r = agent.get(format!("http://{addr}{path}")).call().unwrap();
        let location = r.headers().get("location").and_then(|v| v.to_str().ok()).unwrap_or("").to_owned();
        (r.status().as_u16(), location)
    })
    .await
    .unwrap();
    assert!((300..400).contains(&status), "{status}");
    assert!(location.starts_with("/?next=/api/v1/vaults/"), "{location}");
    assert!(location.contains("render/01M398NHVVT8VC8KH4F8CSJTWS%3Fformat%3Drevealjs"), "{location}");
    let (code, _) = post(
        addr,
        &format!("/api/v1/vaults/{}/notes/{}/render", VaultId::new(), NoteId::new()),
        json!({"format":"html"}),
        None,
    )
    .await;
    assert_eq!(code, 401);
}

/// Personal access tokens (SPEC §11.1): narrowed to vaults and to reading, over REST and the
/// socket alike, and unable to widen themselves.
#[tokio::test]
async fn access_tokens_are_scoped_and_cannot_mint_more() {
    let (addr, _) = start().await;
    let ann = admin(addr).await;
    // Two vaults, made the way a client makes them: by syncing a new id.
    let (a, b) = (VaultId::new(), VaultId::new());
    let mut ws = connect(addr, &ann).await;
    for v in [a, b] {
        send(
            &mut ws,
            &format!("vault:{v}"),
            Message::Sync(SyncMessage::SyncStep1(NoteDoc::new().state_vector())),
        )
        .await;
        assert!(matches!(recv(&mut ws).await, Some((_, Message::Sync(SyncMessage::SyncStep2(_))))));
        assert!(matches!(recv(&mut ws).await, Some((_, Message::Sync(SyncMessage::SyncStep1(_))))));
    }
    let (s, _) = post(
        addr,
        &format!("/api/v1/vaults/{a}/notes"),
        json!({"path": "a.md", "content": "# A\n"}),
        Some(&ann),
    )
    .await;
    assert_eq!(s, 201);

    // Minting: a session can; a vault you are not in is refused; the token is shown once.
    let (s, full) = post(addr, "/api/v1/tokens", json!({"name": "laptop"}), Some(&ann)).await;
    assert_eq!(s, 200, "{full}");
    let full = full["token"].as_str().unwrap().to_owned();
    assert!(full.starts_with("lmt_"));
    let (s, ro) = post(
        addr,
        "/api/v1/tokens",
        json!({"name": "mcp", "vaults": [a.to_string()], "read_only": true}),
        Some(&ann),
    )
    .await;
    assert_eq!(s, 200);
    let ro_id = ro["id"].as_str().unwrap().to_owned();
    let ro = ro["token"].as_str().unwrap().to_owned();
    let (_, only_b) =
        post(addr, "/api/v1/tokens", json!({"name": "b", "vaults": [b.to_string()]}), Some(&ann)).await;
    let only_b = only_b["token"].as_str().unwrap().to_owned();
    assert_eq!(
        post(
            addr,
            "/api/v1/tokens",
            json!({"name": "x", "vaults": [VaultId::new().to_string()]}),
            Some(&ann)
        )
        .await
        .0,
        404
    );
    let (_, listed) = get(addr, "/api/v1/tokens", Some(&ann)).await;
    assert_eq!(listed.as_array().unwrap().len(), 3);
    assert!(listed.as_array().unwrap().iter().all(|t| t["token"].is_null()), "never listed again: {listed}");

    // An unscoped token is its user, minus admin rights.
    assert_eq!(get(addr, "/api/v1/vaults", Some(&full)).await.1.as_array().unwrap().len(), 2);
    let (_, me) = get(addr, "/api/v1/auth/me", Some(&full)).await;
    assert_eq!(me["is_admin"], false);
    assert_eq!(me["token"]["name"], "laptop");
    assert_eq!(post(addr, "/api/v1/invites", json!({}), Some(&full)).await.0, 403);

    // The read-only one sees vault A, reads it, and writes nothing — not over REST…
    let (_, vaults) = get(addr, "/api/v1/vaults", Some(&ro)).await;
    assert_eq!(vaults.as_array().unwrap().len(), 1);
    assert_eq!(vaults[0]["id"], a.to_string());
    assert_eq!(get(addr, &format!("/api/v1/vaults/{a}/notes"), Some(&ro)).await.0, 200);
    assert_eq!(
        post(addr, &format!("/api/v1/vaults/{a}/notes"), json!({"path": "no.md", "content": "x"}), Some(&ro))
            .await
            .0,
        403
    );
    assert_eq!(get(addr, &format!("/api/v1/vaults/{b}/notes"), Some(&ro)).await.0, 404);
    // …nor over the socket, where it cannot claim a new vault either.
    let mut r = connect(addr, &ro).await;
    let vdoc = format!("vault:{a}");
    send(&mut r, &vdoc, Message::Sync(SyncMessage::SyncStep1(NoteDoc::new().state_vector()))).await;
    assert!(matches!(recv(&mut r).await, Some((_, Message::Sync(SyncMessage::SyncStep2(_))))));
    let _ = recv(&mut r).await; // the server's own SyncStep1
    let edit = VaultDoc::new().set_path(NoteId::new(), "sneaky.md");
    send(&mut r, &vdoc, Message::Sync(SyncMessage::Update(edit))).await;
    assert!(matches!(recv(&mut r).await, Some((_, Message::Auth(Some(_))))), "a viewer's write is denied");
    let fresh = format!("vault:{}", VaultId::new());
    send(&mut r, &fresh, Message::Sync(SyncMessage::SyncStep1(NoteDoc::new().state_vector()))).await;
    assert!(matches!(recv(&mut r).await, Some((_, Message::Auth(Some(_))))), "no claiming");

    // A token scoped to B does not reach A, even with Ann's full rights there.
    assert_eq!(get(addr, &format!("/api/v1/vaults/{a}/notes"), Some(&only_b)).await.0, 404);
    assert_eq!(get(addr, &format!("/api/v1/vaults/{b}/notes"), Some(&only_b)).await.0, 200);

    // No token can manage tokens or the password, however wide it is.
    assert_eq!(get(addr, "/api/v1/tokens", Some(&full)).await.0, 403);
    assert_eq!(post(addr, "/api/v1/tokens", json!({"name": "wider"}), Some(&ro)).await.0, 403);
    assert_eq!(del(addr, &format!("/api/v1/tokens/{ro_id}"), &full).await, 403);
    assert_eq!(
        post(
            addr,
            "/api/v1/auth/password",
            json!({"current_password": "first pass", "new_password": "hijacked!"}),
            Some(&full)
        )
        .await
        .0,
        403
    );

    // Revoked is gone.
    assert_eq!(del(addr, &format!("/api/v1/tokens/{ro_id}"), &ann).await, 204);
    assert_eq!(get(addr, "/api/v1/vaults", Some(&ro)).await.0, 401);
    assert_eq!(del(addr, &format!("/api/v1/tokens/{ro_id}"), &ann).await, 404);
}

/// With password login off, the password routes are closed and the sign-in page is told so.
#[tokio::test]
async fn password_login_can_be_turned_off() {
    let options = ServerOptions {
        auth: AuthMode::Enabled { allow_registration: true, secure_cookies: false },
        password_login: false,
        ..ServerOptions::default()
    };
    let state = build_state(Store::open_in_memory().unwrap(), options);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router(state.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let (s, config) = get(addr, "/api/v1/auth/config", None).await;
    assert_eq!(s, 200);
    assert_eq!(config["password_login"], false);
    assert_eq!(config["registration"], true);
    assert_eq!(config["oidc"], Value::Null);
    let creds = json!({"email": "ann@example.org", "password": "first pass"});
    assert_eq!(post(addr, "/api/v1/auth/register", creds.clone(), None).await.0, 403);
    assert_eq!(post(addr, "/api/v1/auth/login", creds, None).await.0, 403);
}

/// A native app signing in through the browser (`apps.rs`): the page approves with the
/// session, the app trades the code for a token named after the device.
#[tokio::test]
async fn an_app_signs_in_through_the_browser() {
    let (addr, _) = start().await;
    let ann = admin(addr).await;
    let dir = tempfile::tempdir().unwrap();
    // SAFETY: the only test in this binary that touches these variables.
    unsafe {
        std::env::set_var("LEMMATE_CONFIG_DIR", dir.path());
        std::env::set_var("LEMMATE_KEYCHAIN", "0");
    }
    let server = format!("http://{addr}");
    let flow = lemmate_core::credentials::BrowserSignIn::start(
        &server,
        "http://127.0.0.1/lemmate-callback",
        "Lemmate desktop on test",
    )
    .unwrap();
    // What the page reads off its own address.
    let q: std::collections::HashMap<String, String> =
        url::Url::parse(&flow.url).unwrap().query_pairs().into_owned().collect();
    assert_eq!(q["authorize"], "app");
    let approve = |redirect: &str, token: &str| {
        let body = json!({
            "redirect_uri": redirect, "state": q["state"], "code_challenge": q["code_challenge"], "device": q["device"],
        });
        let token = token.to_owned();
        async move { post(addr, "/api/v1/auth/app/approve", body, Some(&token)).await }
    };
    assert_eq!(approve("https://evil.example/cb", &ann).await.0, 400, "only a loopback redirect");
    let (s, out) = approve(&q["redirect_uri"], &ann).await;
    assert_eq!(s, 200, "{out}");
    let redirect = out["redirect"].as_str().unwrap().to_owned();
    assert!(flow.is_callback(&redirect), "{redirect}");
    let query = url::Url::parse(&redirect).unwrap().query().unwrap().to_owned();

    // A code is spent once, and only with the verifier behind its challenge.
    let (s, _) = post(
        addr,
        "/api/v1/auth/app/token",
        json!({"code": query.split('&').next().unwrap().trim_start_matches("code="), "code_verifier": "wrong".repeat(10), "redirect_uri": q["redirect_uri"]}),
        None,
    )
    .await;
    assert_eq!(s, 400);
    let (s, out) = approve(&q["redirect_uri"], &ann).await; // the wrong guess burnt that code
    assert_eq!(s, 200);
    let query = url::Url::parse(out["redirect"].as_str().unwrap()).unwrap().query().unwrap().to_owned();
    let email = tokio::task::spawn_blocking({
        let (flow, query) = (flow.clone(), query.clone());
        move || flow.finish(&query, None)
    })
    .await
    .unwrap()
    .unwrap();
    assert_eq!(email, "ann@example.org");
    let again = tokio::task::spawn_blocking(move || flow.finish(&query, None)).await.unwrap();
    assert!(again.is_err(), "replayed");

    let token = lemmate_core::credentials::load(&server).unwrap();
    assert!(token.starts_with("lmt_"));
    let (_, me) = get(addr, "/api/v1/auth/me", Some(&token)).await;
    assert_eq!(me["token"]["name"], "Lemmate desktop on test");
    // …and that token cannot approve another app in turn.
    assert_eq!(approve(&q["redirect_uri"], &token).await.0, 403);

    // Signing the app out (`lemmate logout`, the desktop's Sign out) revokes its token on the
    // server and forgets it here — and touches nothing else.
    let revoked = tokio::task::spawn_blocking({
        let server = server.clone();
        move || lemmate_core::credentials::sign_out(&server, None)
    })
    .await
    .unwrap()
    .unwrap();
    assert!(revoked);
    assert_eq!(lemmate_core::credentials::load(&server), None);
    assert_eq!(get(addr, "/api/v1/auth/me", Some(&token)).await.0, 401);
    assert_eq!(get(addr, "/api/v1/auth/me", Some(&ann)).await.0, 200, "the browser session is untouched");
}

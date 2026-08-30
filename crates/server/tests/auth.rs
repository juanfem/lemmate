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

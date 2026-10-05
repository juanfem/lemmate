//! What one account must not be able to do to another's notes, and what the server must not let
//! anyone do to it: reaching a note through the wrong vault, pulling one into your own vault,
//! adopting trashed or orphaned ones, keeping a socket after access is gone, unsafe paths,
//! unsafe attachments, oversized bodies, guessing passwords (SPEC §11).

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use lemmate_core::attachments::hash_bytes;
use lemmate_core::sync::{Frame, Message, SyncMessage};
use lemmate_core::{DocId, NoteDoc, NoteId, Store, VaultDoc, VaultId};
use lemmate_server::{AppState, AuthMode, ServerOptions, build_state, router};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message as TMsg;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

async fn start() -> (SocketAddr, Arc<AppState>) {
    let dir = tempfile::tempdir().unwrap().keep();
    let options = ServerOptions {
        auth: AuthMode::Enabled { allow_registration: false, secure_cookies: false },
        attachments_dir: dir,
        ..ServerOptions::default()
    };
    let state = build_state(Store::open_in_memory().unwrap(), options);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router(state.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (addr, state)
}

struct Reply {
    status: u16,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

impl Reply {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
}

/// Any request; `body` is (bytes, content type).
async fn call(
    method: &str,
    addr: SocketAddr,
    path: &str,
    body: Option<(Vec<u8>, &str)>,
    token: Option<&str>,
) -> Reply {
    let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).build().into();
    let url = format!("http://{addr}{path}");
    let method = method.to_owned();
    let token = token.map(str::to_owned);
    let body = body.map(|(b, t)| (b, t.to_owned()));
    tokio::task::spawn_blocking(move || {
        let req = ureq::http::Request::builder().method(method.as_str()).uri(&url);
        let req = match &token {
            Some(t) => req.header("authorization", format!("Bearer {t}")),
            None => req,
        };
        let (req, bytes) = match body {
            Some((b, t)) => (req.header("content-type", t), b),
            None => (req, Vec::new()),
        };
        let mut r = agent.run(req.body(bytes).unwrap()).unwrap();
        let headers = r
            .headers()
            .iter()
            .map(|(k, v)| (k.as_str().to_owned(), v.to_str().unwrap_or_default().to_owned()))
            .collect();
        let body = r.body_mut().with_config().limit(u64::MAX).read_to_vec().unwrap_or_default();
        Reply { status: r.status().as_u16(), headers, body }
    })
    .await
    .unwrap()
}

async fn get(addr: SocketAddr, path: &str, token: &str) -> Reply {
    call("GET", addr, path, None, Some(token)).await
}

async fn send_json(method: &str, addr: SocketAddr, path: &str, body: Value, token: &str) -> Reply {
    call(method, addr, path, Some((body.to_string().into_bytes(), "application/json")), Some(token)).await
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
    let msg = tokio::time::timeout(Duration::from_millis(1500), c.next()).await.ok()??.ok()?;
    let TMsg::Binary(b) = msg else { return None };
    let f = Frame::decode(&b).ok()?;
    let m = f.message().ok()?;
    Some((f.doc_id, m))
}

/// Whether the server closes the socket within a few seconds (frames on the way are skipped).
async fn closes(c: &mut Client) -> bool {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(4);
    loop {
        match tokio::time::timeout_at(deadline, c.next()).await {
            Err(_) => return false,
            Ok(None) | Ok(Some(Err(_))) | Ok(Some(Ok(TMsg::Close(_)))) => return true,
            Ok(Some(Ok(_))) => {}
        }
    }
}

/// Sync a vault doc from scratch and return the local copy; the first sync of a new id makes
/// the user its owner.
async fn open_vault(c: &mut Client, vault: VaultId) -> VaultDoc {
    let doc = VaultDoc::new();
    send(c, &format!("vault:{vault}"), Message::Sync(SyncMessage::SyncStep1(doc.state_vector()))).await;
    let me = format!("vault:{vault}");
    // Updates to docs this socket already follows may be queued ahead of the answer.
    let mut got_state = false;
    loop {
        match recv(c).await {
            Some((d, Message::Sync(SyncMessage::SyncStep2(u)))) if d == me => {
                doc.apply_update(&u).unwrap();
                got_state = true;
            }
            Some((d, Message::Sync(SyncMessage::SyncStep1(_)))) if d == me && got_state => return doc,
            Some((d, _)) if d != me => {}
            other => panic!("opening {me}: {other:?}"),
        }
    }
}

/// Two accounts, Ann (the admin) and Bob, each with a session token.
async fn ann_and_bob(addr: SocketAddr) -> (String, String) {
    let r = call(
        "POST",
        addr,
        "/api/v1/auth/register",
        Some((
            json!({"email": "ann@example.org", "password": "first pass"}).to_string().into_bytes(),
            "application/json",
        )),
        None,
    )
    .await;
    assert_eq!(r.status, 200);
    let ann = r.json()["token"].as_str().unwrap().to_owned();
    let r = send_json(
        "POST",
        addr,
        "/api/v1/auth/register",
        json!({"email": "bob@example.org", "password": "bobs pass"}),
        &ann,
    )
    .await;
    assert_eq!(r.status, 200);
    (ann, login(addr, "bob@example.org", "bobs pass").await.1.unwrap())
}

async fn login(addr: SocketAddr, email: &str, password: &str) -> (u16, Option<String>) {
    let r = call(
        "POST",
        addr,
        "/api/v1/auth/login",
        Some((json!({"email": email, "password": password}).to_string().into_bytes(), "application/json")),
        None,
    )
    .await;
    (r.status, r.json()["token"].as_str().map(str::to_owned))
}

async fn create_note(addr: SocketAddr, vault: VaultId, path: &str, content: &str, token: &str) -> NoteId {
    let r = send_json(
        "POST",
        addr,
        &format!("/api/v1/vaults/{vault}/notes"),
        json!({"path": path, "content": content}),
        token,
    )
    .await;
    assert_eq!(r.status, 201, "{}", String::from_utf8_lossy(&r.body));
    r.json()["id"].as_str().unwrap().parse().unwrap()
}

async fn note_ids(addr: SocketAddr, vault: VaultId, token: &str) -> Vec<String> {
    let r = get(addr, &format!("/api/v1/vaults/{vault}/notes"), token).await;
    assert_eq!(r.status, 200);
    r.json().as_array().unwrap().iter().map(|n| n["id"].as_str().unwrap().to_owned()).collect()
}

async fn user_id(addr: SocketAddr, token: &str) -> String {
    get(addr, "/api/v1/auth/me", token).await.json()["id"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn a_note_is_reached_only_through_its_own_vault() {
    let (addr, _) = start().await;
    let (ann, bob) = ann_and_bob(addr).await;
    let (a, b) = (VaultId::new(), VaultId::new());
    open_vault(&mut connect(addr, &ann).await, a).await;
    open_vault(&mut connect(addr, &bob).await, b).await;
    let secret = create_note(addr, a, "secret.md", "# Ann's secret\n", &ann).await;

    // Bob names Ann's note under his own vault: every route answers as if it did not exist.
    let under_b = format!("/api/v1/vaults/{b}/notes/{secret}");
    let export = json!({"format": "html"});
    assert_eq!(send_json("POST", addr, &format!("{under_b}/export"), export.clone(), &bob).await.status, 404);
    assert_eq!(send_json("POST", addr, &format!("{under_b}/render"), export, &bob).await.status, 404);
    assert_eq!(get(addr, &format!("{under_b}/render?format=html"), &bob).await.status, 404);
    assert_eq!(get(addr, &format!("{under_b}/render/someid?format=html"), &bob).await.status, 404);
    assert_eq!(get(addr, &under_b, &bob).await.status, 404);
    assert_eq!(get(addr, &format!("{under_b}/versions"), &bob).await.status, 404);

    // Unsharing needs the note to be in the vault, as sharing does.
    let ann_id = user_id(addr, &ann).await;
    let r = send_json(
        "PUT",
        addr,
        &format!("/api/v1/vaults/{a}/notes/{secret}/shares"),
        json!({"kind": "link"}),
        &ann,
    )
    .await;
    assert_eq!(r.status, 200);
    let r = send_json(
        "DELETE",
        addr,
        &format!("{under_b}/shares"),
        json!({"links": true, "user_id": ann_id}),
        &bob,
    )
    .await;
    assert_eq!(r.status, 404);
    let shares = get(addr, &format!("/api/v1/vaults/{a}/notes/{secret}/shares"), &ann).await.json();
    assert_eq!(shares.as_array().unwrap().len(), 1, "the link survives");

    // Restoring through the wrong vault changes nothing — not even the trash.
    assert_eq!(
        call("DELETE", addr, &format!("/api/v1/vaults/{a}/notes/{secret}"), None, Some(&ann)).await.status,
        204
    );
    assert_eq!(call("POST", addr, &format!("{under_b}/restore"), None, Some(&bob)).await.status, 404);
    let trash = get(addr, &format!("/api/v1/vaults/{a}/trash"), &ann).await.json();
    assert_eq!(trash.as_array().unwrap().len(), 1, "still in Ann's trash");
}

#[tokio::test]
async fn a_restored_note_keeps_its_extension() {
    let (addr, _) = start().await;
    let (ann, _) = ann_and_bob(addr).await;
    let a = VaultId::new();
    open_vault(&mut connect(addr, &ann).await, a).await;
    let first = create_note(addr, a, "deck.qmd", "# One\n", &ann).await;
    assert_eq!(
        call("DELETE", addr, &format!("/api/v1/vaults/{a}/notes/{first}"), None, Some(&ann)).await.status,
        204
    );
    create_note(addr, a, "deck.qmd", "# Two\n", &ann).await;
    let r = call("POST", addr, &format!("/api/v1/vaults/{a}/notes/{first}/restore"), None, Some(&ann)).await;
    assert_eq!(r.status, 200);
    assert_eq!(r.json()["path"], "deck (restored).qmd");
}

#[tokio::test]
async fn a_vault_doc_cannot_take_another_vaults_note() {
    let (addr, _) = start().await;
    let (ann, bob) = ann_and_bob(addr).await;
    let (a, b, c) = (VaultId::new(), VaultId::new(), VaultId::new());
    let mut ann_ws = connect(addr, &ann).await;
    open_vault(&mut ann_ws, a).await;
    let mut bob_ws = connect(addr, &bob).await;
    let bob_doc = open_vault(&mut bob_ws, b).await;
    let secret = create_note(addr, a, "secret.md", "# Ann's secret\n", &ann).await;

    // Bob writes Ann's note id into his own vault doc.
    let u = bob_doc.set_path(secret, "stolen.md");
    send(&mut bob_ws, &format!("vault:{b}"), Message::Sync(SyncMessage::Update(u))).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(note_ids(addr, a, &ann).await.contains(&secret.to_string()), "still Ann's");
    assert!(note_ids(addr, b, &bob).await.is_empty());
    assert_eq!(get(addr, &format!("/api/v1/vaults/{b}/notes/{secret}"), &bob).await.status, 404);
    send(
        &mut bob_ws,
        &DocId::Note(secret).to_string(),
        Message::Sync(SyncMessage::SyncStep1(NoteDoc::new().state_vector())),
    )
    .await;
    let got = recv(&mut bob_ws).await;
    assert!(matches!(got, Some((_, Message::Auth(Some(_))))), "the text stays Ann's: {got:?}");

    // Ann may move her own note between her vaults, which is what a merge does.
    let ann_c = open_vault(&mut ann_ws, c).await;
    let u = ann_c.set_path(secret, "moved.md");
    send(&mut ann_ws, &format!("vault:{c}"), Message::Sync(SyncMessage::Update(u))).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(note_ids(addr, c, &ann).await.contains(&secret.to_string()), "moved into C");
    assert!(!note_ids(addr, a, &ann).await.contains(&secret.to_string()));
}

#[tokio::test]
async fn trashed_and_orphaned_notes_cannot_be_claimed() {
    let (addr, state) = start().await;
    let (ann, bob) = ann_and_bob(addr).await;
    let (a, d, b) = (VaultId::new(), VaultId::new(), VaultId::new());
    let mut ann_ws = connect(addr, &ann).await;
    open_vault(&mut ann_ws, a).await;
    open_vault(&mut ann_ws, d).await;
    let mut bob_ws = connect(addr, &bob).await;
    open_vault(&mut bob_ws, b).await;

    // A note in the trash is still its vault's.
    let trashed = create_note(addr, a, "old.md", "# Old\n", &ann).await;
    let r = send_json(
        "PUT",
        addr,
        &format!("/api/v1/vaults/{a}/notes/{trashed}/shares"),
        json!({"kind": "user", "email": "bob@example.org", "role": "viewer"}),
        &ann,
    )
    .await;
    assert_eq!(r.status, 200);
    assert_eq!(
        call("DELETE", addr, &format!("/api/v1/vaults/{a}/notes/{trashed}"), None, Some(&ann)).await.status,
        204
    );
    let probe = Message::Sync(SyncMessage::Update(NoteDoc::new().set_text("mine now")));
    send(&mut bob_ws, &DocId::Note(trashed).to_string(), probe.clone()).await;
    let got = recv(&mut bob_ws).await;
    assert!(matches!(got, Some((_, Message::Auth(Some(_))))), "{got:?}");

    // A note left behind by a deleted vault is nobody's to take.
    let orphan = create_note(addr, d, "left.md", "# Left behind\n", &ann).await;
    assert_eq!(call("DELETE", addr, &format!("/api/v1/vaults/{d}"), None, Some(&ann)).await.status, 204);
    send(
        &mut bob_ws,
        &DocId::Note(orphan).to_string(),
        Message::Sync(SyncMessage::SyncStep1(NoteDoc::new().state_vector())),
    )
    .await;
    assert!(matches!(recv(&mut bob_ws).await, Some((_, Message::Auth(Some(_))))));

    // Purging the trash takes the note's shares with it.
    tokio::time::sleep(Duration::from_millis(20)).await;
    let report =
        lemmate_server::purge_orphans(&state, lemmate_core::store::now_ms(), Duration::ZERO).await.unwrap();
    assert_eq!(report.purged_notes, 1);
    assert!(state.store.lock().await.note_shares(trashed).unwrap().is_empty());

    // A note nobody has written is still a new note, and its writer's.
    let fresh = NoteId::new();
    send(&mut bob_ws, &DocId::Note(fresh).to_string(), probe).await;
    assert!(recv(&mut bob_ws).await.is_none(), "no denial");
}

#[tokio::test]
async fn sockets_lose_what_access_they_lose() {
    let (addr, _) = start().await;
    let (ann, bob) = ann_and_bob(addr).await;
    let bob_id = user_id(addr, &bob).await;
    let a = VaultId::new();
    open_vault(&mut connect(addr, &ann).await, a).await;
    let note = create_note(addr, a, "n.md", "# N\n", &ann).await;
    let r = send_json(
        "PUT",
        addr,
        &format!("/api/v1/vaults/{a}/members"),
        json!({"email": "bob@example.org", "role": "viewer"}),
        &ann,
    )
    .await;
    assert_eq!(r.status, 204);

    // Bob, a member, follows the vault and the note.
    let mut ws = connect(addr, &bob).await;
    open_vault(&mut ws, a).await;
    send(
        &mut ws,
        &DocId::Note(note).to_string(),
        Message::Sync(SyncMessage::SyncStep1(NoteDoc::new().state_vector())),
    )
    .await;
    let got = recv(&mut ws).await;
    assert!(matches!(got, Some((_, Message::Sync(SyncMessage::SyncStep2(_))))), "{got:?}");
    assert!(matches!(recv(&mut ws).await, Some((_, Message::Sync(SyncMessage::SyncStep1(_))))));

    // Removed, he is told he lost both, and hears nothing more of either.
    assert_eq!(
        call("DELETE", addr, &format!("/api/v1/vaults/{a}/members/{bob_id}"), None, Some(&ann)).await.status,
        204
    );
    let mut lost = Vec::new();
    while let Some((doc, m)) = recv(&mut ws).await {
        assert!(matches!(m, Message::Auth(Some(_))), "{doc}: only denials");
        lost.push(doc);
    }
    lost.sort();
    let mut expected = vec![format!("vault:{a}"), DocId::Note(note).to_string()];
    expected.sort();
    assert_eq!(lost, expected);
    let r = send_json(
        "PUT",
        addr,
        &format!("/api/v1/vaults/{a}/notes/{note}"),
        json!({"content": "# N, edited\n"}),
        &ann,
    )
    .await;
    assert_eq!(r.status, 200);
    create_note(addr, a, "later.md", "# Later\n", &ann).await;
    assert!(recv(&mut ws).await.is_none(), "no update reaches a removed member");

    // A share taken back is the same.
    let r = send_json(
        "PUT",
        addr,
        &format!("/api/v1/vaults/{a}/notes/{note}/shares"),
        json!({"kind": "user", "email": "bob@example.org"}),
        &ann,
    )
    .await;
    assert_eq!(r.status, 200);
    send(
        &mut ws,
        &DocId::Note(note).to_string(),
        Message::Sync(SyncMessage::SyncStep1(NoteDoc::new().state_vector())),
    )
    .await;
    assert!(matches!(recv(&mut ws).await, Some((_, Message::Sync(SyncMessage::SyncStep2(_))))));
    assert!(matches!(recv(&mut ws).await, Some((_, Message::Sync(SyncMessage::SyncStep1(_))))));
    let r = send_json(
        "DELETE",
        addr,
        &format!("/api/v1/vaults/{a}/notes/{note}/shares"),
        json!({"user_id": bob_id}),
        &ann,
    )
    .await;
    assert_eq!(r.status, 204);
    assert!(matches!(recv(&mut ws).await, Some((_, Message::Auth(Some(_))))));

    // Signing out closes the socket the session opened; so does revoking a token.
    assert!(!closes(&mut ws).await, "nothing closes it yet");
    assert_eq!(call("POST", addr, "/api/v1/auth/logout", None, Some(&bob)).await.status, 204);
    assert!(closes(&mut ws).await, "logout closes the socket");

    let r = send_json("POST", addr, "/api/v1/tokens", json!({"name": "cli"}), &ann).await;
    let (token, token_id) =
        (r.json()["token"].as_str().unwrap().to_owned(), r.json()["id"].as_str().unwrap().to_owned());
    let mut tws = connect(addr, &token).await;
    open_vault(&mut tws, a).await;
    assert_eq!(
        call("DELETE", addr, &format!("/api/v1/tokens/{token_id}"), None, Some(&ann)).await.status,
        204
    );
    assert!(closes(&mut tws).await, "a revoked token's socket is closed");
}

#[tokio::test]
async fn two_requests_for_the_same_day_make_one_note() {
    let (addr, _) = start().await;
    let (ann, _) = ann_and_bob(addr).await;
    let a = VaultId::new();
    open_vault(&mut connect(addr, &ann).await, a).await;
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let ann = ann.clone();
        tasks.push(tokio::spawn(async move {
            get(addr, &format!("/api/v1/vaults/{a}/daily/2026-10-05"), &ann).await
        }));
    }
    let mut ids = std::collections::HashSet::new();
    for t in tasks {
        let r = t.await.unwrap();
        assert_eq!(r.status, 200, "{}", String::from_utf8_lossy(&r.body));
        ids.insert(r.json()["id"].as_str().unwrap().to_owned());
    }
    assert_eq!(ids.len(), 1, "every request got the same note");
    assert_eq!(note_ids(addr, a, &ann).await.len(), 1);
}

#[tokio::test]
async fn guessing_a_password_is_cut_short() {
    let (addr, _) = start().await;
    ann_and_bob(addr).await;
    for _ in 0..10 {
        assert_eq!(login(addr, "bob@example.org", "wrong").await.0, 401);
    }
    assert_eq!(login(addr, "bob@example.org", "bobs pass").await.0, 429, "even the right one, for now");
    // Another address is not held up by Bob's; an unknown one fails like a wrong password.
    assert_eq!(login(addr, "ann@example.org", "first pass").await.0, 200);
    assert_eq!(login(addr, "nobody@example.org", "whatever").await.0, 401);
}

#[tokio::test]
async fn the_last_owner_cannot_step_down() {
    let (addr, _) = start().await;
    let (ann, _) = ann_and_bob(addr).await;
    let a = VaultId::new();
    open_vault(&mut connect(addr, &ann).await, a).await;
    let members = format!("/api/v1/vaults/{a}/members");
    let r =
        send_json("PUT", addr, &members, json!({"email": "ann@example.org", "role": "editor"}), &ann).await;
    assert_eq!(r.status, 409);
    let r =
        send_json("PUT", addr, &members, json!({"email": "bob@example.org", "role": "owner"}), &ann).await;
    assert_eq!(r.status, 204);
    let r =
        send_json("PUT", addr, &members, json!({"email": "ann@example.org", "role": "editor"}), &ann).await;
    assert_eq!(r.status, 204, "with another owner she may");

    // The vault list says what each caller may do there, as their credential allows it.
    assert_eq!(get(addr, "/api/v1/vaults", &ann).await.json()[0]["role"], "editor");
    let r = send_json("POST", addr, "/api/v1/tokens", json!({"name": "ro", "read_only": true}), &ann).await;
    let ro = r.json()["token"].as_str().unwrap().to_owned();
    assert_eq!(get(addr, "/api/v1/vaults", &ro).await.json()[0]["role"], "viewer");
}

#[tokio::test]
async fn attachments_are_served_so_they_cannot_run_here() {
    let (addr, _) = start().await;
    let (ann, _) = ann_and_bob(addr).await;
    let a = VaultId::new();
    open_vault(&mut connect(addr, &ann).await, a).await;
    let upload = |bytes: &'static [u8], mime: &'static str| {
        let ann = ann.clone();
        async move {
            let hash = hash_bytes(bytes);
            let path = format!("/api/v1/vaults/{a}/attachments/{hash}");
            assert!(call("PUT", addr, &path, Some((bytes.to_vec(), mime)), Some(&ann)).await.status < 300);
            get(addr, &path, &ann).await
        }
    };
    let html = upload(b"<script>alert(document.cookie)</script>", "text/html").await;
    assert_eq!(html.status, 200);
    assert_eq!(html.headers["content-disposition"], "attachment");
    assert_eq!(html.headers["content-security-policy"], "sandbox");
    assert_eq!(html.headers["x-content-type-options"], "nosniff");
    assert!(html.headers["cache-control"].starts_with("private"));
    let svg = upload(b"<svg xmlns='http://www.w3.org/2000/svg' onload='alert(1)'/>", "image/svg+xml").await;
    assert_eq!(svg.headers["content-disposition"], "attachment");
    let png = upload(b"\x89PNG\r\n\x1a\n not really", "image/png").await;
    assert_eq!(png.headers["content-disposition"], "inline");
    assert_eq!(png.headers["content-security-policy"], "sandbox");
}

#[tokio::test]
async fn bodies_are_bounded() {
    let (addr, _) = start().await;
    let (ann, _) = ann_and_bob(addr).await;
    let a = VaultId::new();
    let mut ws = connect(addr, &ann).await;
    open_vault(&mut ws, a).await;
    // A JSON route takes a few MiB at most; an upload, far more.
    let big = "x".repeat(lemmate_server::app::JSON_BODY_LIMIT + 1);
    let r = send_json(
        "POST",
        addr,
        &format!("/api/v1/vaults/{a}/notes"),
        json!({"path": "big.md", "content": big}),
        &ann,
    )
    .await;
    assert_eq!(r.status, 413);
    let bytes = vec![7u8; lemmate_server::app::JSON_BODY_LIMIT * 2];
    let path = format!("/api/v1/vaults/{a}/attachments/{}", hash_bytes(&bytes));
    assert_eq!(
        call("PUT", addr, &path, Some((bytes, "application/octet-stream")), Some(&ann)).await.status,
        201
    );
    // A WebSocket message past the limit ends the connection.
    let huge = vec![0u8; lemmate_server::app::WS_MAX_MESSAGE + 1];
    let _ = ws.send(TMsg::Binary(huge.into())).await;
    assert!(closes(&mut ws).await);
}

#[tokio::test]
async fn a_vault_doc_with_an_unsafe_path_is_refused() {
    let (addr, _) = start().await;
    let (ann, _) = ann_and_bob(addr).await;
    let a = VaultId::new();
    let mut ws = connect(addr, &ann).await;
    let doc = open_vault(&mut ws, a).await;
    for bad in [
        ".lemmate/state.md",
        "../outside.md",
        "/etc/passwd.md",
        "a//b.md",
        "a\\b.md",
        ".git/hooks/x.md",
        "C:/x.md",
    ] {
        let local = VaultDoc::from_updates([doc.encode_full().as_slice()]).unwrap();
        let u = local.set_path(NoteId::new(), bad);
        send(&mut ws, &format!("vault:{a}"), Message::Sync(SyncMessage::Update(u))).await;
        assert!(matches!(recv(&mut ws).await, Some((_, Message::Auth(Some(_))))), "{bad}");
        let local = VaultDoc::from_updates([doc.encode_full().as_slice()]).unwrap();
        let u = local.set_attachment(bad, &hash_bytes(b"x"));
        send(&mut ws, &format!("vault:{a}"), Message::Sync(SyncMessage::Update(u))).await;
        assert!(matches!(recv(&mut ws).await, Some((_, Message::Auth(Some(_))))), "attachment {bad}");
    }
    assert!(note_ids(addr, a, &ann).await.is_empty());
    // A path a folder can hold goes through, and so does REST's with the same rule.
    let u = doc.set_path(NoteId::new(), "Projects/fine.md");
    send(&mut ws, &format!("vault:{a}"), Message::Sync(SyncMessage::Update(u))).await;
    assert!(recv(&mut ws).await.is_none(), "no denial");
    assert_eq!(note_ids(addr, a, &ann).await.len(), 1);
    let r =
        send_json("POST", addr, &format!("/api/v1/vaults/{a}/notes"), json!({"path": ".lemmate/x.md"}), &ann)
            .await;
    assert_eq!(r.status, 400);
}

#[tokio::test]
async fn search_finds_notes_shared_with_you() {
    let (addr, _) = start().await;
    let (ann, bob) = ann_and_bob(addr).await;
    let a = VaultId::new();
    open_vault(&mut connect(addr, &ann).await, a).await;
    let shared = create_note(addr, a, "zoo.md", "# Zoo\n\nA zebra lives here.\n", &ann).await;
    create_note(addr, a, "other.md", "# Other\n\nAnother zebra.\n", &ann).await;
    assert!(get(addr, "/api/v1/search?q=zebra", &bob).await.json().as_array().unwrap().is_empty());
    let r = send_json(
        "PUT",
        addr,
        &format!("/api/v1/vaults/{a}/notes/{shared}/shares"),
        json!({"kind": "user", "email": "bob@example.org"}),
        &ann,
    )
    .await;
    assert_eq!(r.status, 200);
    let hits = get(addr, "/api/v1/search?q=zebra", &bob).await.json();
    let ids: Vec<&str> = hits.as_array().unwrap().iter().map(|h| h["note_id"].as_str().unwrap()).collect();
    assert_eq!(ids, [shared.to_string()], "the shared note, and not its neighbour");
}

#[tokio::test]
async fn idle_rooms_are_dropped_and_come_back_unchanged() {
    let (addr, state) = start().await;
    let (ann, _) = ann_and_bob(addr).await;
    let a = VaultId::new();
    let mut ws = connect(addr, &ann).await;
    open_vault(&mut ws, a).await;
    let note = create_note(addr, a, "n.md", "# Kept\n", &ann).await;
    assert!(state.room_count().await >= 2);
    assert!(state.evict_idle_rooms(Duration::ZERO).await >= 2);
    assert_eq!(state.room_count().await, 0);
    let r = get(addr, &format!("/api/v1/vaults/{a}/notes/{note}"), &ann).await;
    assert!(r.json()["content"].as_str().unwrap().contains("# Kept"));
    // A subscriber of an evicted room still hears about the next change.
    let mut other = connect(addr, &ann).await;
    open_vault(&mut other, a).await;
    state.evict_idle_rooms(Duration::ZERO).await;
    create_note(addr, a, "m.md", "# M\n", &ann).await;
    assert!(matches!(recv(&mut other).await, Some((_, Message::Sync(SyncMessage::Update(_))))));
}

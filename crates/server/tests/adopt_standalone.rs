//! Giving a standalone vault a server later (SPEC §3.2).
//!
//! A vault that has only ever existed on one machine is adopted by the first account that syncs
//! it: the notes and their history go up as an ordinary first sync, and the attachment blobs —
//! which no server has ever been offered, because standalone the engine records them in the
//! vault doc itself — are backfilled on that first connected run.

use std::net::SocketAddr;
use std::path::Path;
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use lemmate_core::attachments::hash_bytes;
use lemmate_core::client::{LocalHandle, LocalOptions, SyncOptions, start};
use lemmate_core::sync::{Frame, Message, SyncMessage};
use lemmate_core::{DocId, Role, Store, VaultDoc, VaultId};
use lemmate_server::{AuthMode, ServerOptions, build_state, router};
use serde_json::Value;
use tokio_tungstenite::tungstenite::Message as TMsg;

async fn server(attachments: &Path) -> SocketAddr {
    let opts = ServerOptions { attachments_dir: attachments.to_path_buf(), ..Default::default() };
    let state = build_state(Store::open_in_memory().unwrap(), opts);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router(state);
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    addr
}

async fn relay(dir: &Path, server: Option<SocketAddr>) -> LocalHandle {
    relay_as(dir, server, None, None).await
}

async fn relay_as(
    dir: &Path,
    server: Option<SocketAddr>,
    vault_id: Option<VaultId>,
    token: Option<String>,
) -> LocalHandle {
    let opts = SyncOptions {
        vault_dir: dir.to_path_buf(),
        server_url: server.map(|s| format!("http://{s}")),
        vault_id,
        once: false,
        ca_cert: None,
        token,
    };
    let local = LocalOptions {
        bind: "127.0.0.1:0".parse().unwrap(),
        web_dir: None,
        vault_root: None,
        config_path: None,
    };
    start(opts, local).await.unwrap()
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

async fn until(what: &str, mut f: impl AsyncFnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if f().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("timed out waiting for {what}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_standalone_vault_is_adopted_by_a_server_with_its_attachments() {
    let tmp = tempfile::tempdir().unwrap();
    let vault_dir = tmp.path().join("notes");
    let bytes = b"pretend this is a diagram".to_vec();
    let hash = hash_bytes(&bytes);

    // ---- Phase one: no server at all.
    let local = relay(&vault_dir, None).await;
    let base = format!("http://{}", local.addr);
    let vault = local.vault_id.to_string();

    let attachment_url = format!("{base}/api/v1/vaults/{vault}/attachments/{hash}");
    let stored = tokio::task::spawn_blocking({
        let (url, bytes) = (attachment_url.clone(), bytes.clone());
        move || {
            let mut r = ureq::put(&url).header("x-filename", "diagram.png").send(&bytes[..]).unwrap();
            serde_json::from_str::<Value>(&r.body_mut().read_to_string().unwrap()).unwrap()
        }
    })
    .await
    .unwrap();
    let path = stored["path"].as_str().unwrap().to_owned();

    let (code, note) = post(
        format!("{base}/api/v1/vaults/{vault}/notes"),
        serde_json::json!({ "path": "Plan.md", "content": format!("# Plan\n\n![diagram]({path})\n") }),
    )
    .await;
    assert_eq!(code, 201, "{note}");
    let note_id = note["id"].as_str().unwrap().to_owned();
    // Recorded locally, since there is nowhere to upload it to.
    until("the attachment to be recorded", async || get(attachment_url.clone()).await.0 == 200).await;
    local.abort();

    // ---- Phase two: the same folder, now with a server. Nothing else changes.
    let server_addr = server(&tmp.path().join("blobs")).await;
    let synced = relay(&vault_dir, Some(server_addr)).await;
    let remote = format!("http://{server_addr}");

    // A vault nobody owns is claimed by the first client that syncs it, history and all.
    until("the server to hold the note", async || {
        let (_, notes) = get(format!("{remote}/api/v1/vaults/{vault}/notes")).await;
        notes.as_array().is_some_and(|n| !n.is_empty())
    })
    .await;
    let (_, uploaded) = get(format!("{remote}/api/v1/vaults/{vault}/notes/{note_id}")).await;
    assert_eq!(uploaded["path"], "Plan.md");
    assert!(
        uploaded["content"].as_str().unwrap().contains("![diagram]"),
        "the note's content came with it: {uploaded}"
    );

    // The blob itself: recorded in the vault doc while standalone, so nothing would have
    // uploaded it without the backfill, and every other replica would find a dangling hash.
    until("the attachment to be backfilled", async || {
        get(format!("{remote}/api/v1/vaults/{vault}/attachments/{hash}")).await.0 == 200
    })
    .await;
    let served = tokio::task::spawn_blocking({
        let url = format!("{remote}/api/v1/vaults/{vault}/attachments/{hash}");
        move || ureq::get(&url).call().unwrap().body_mut().read_to_vec().unwrap()
    })
    .await
    .unwrap();
    assert_eq!(served, bytes, "the server serves the bytes the standalone vault had");

    synced.abort();
}

/// The other outcome: the vault id already belongs to somebody else, so the server refuses it.
///
/// This is the failure a local relay could hide completely — the window's socket is the loopback
/// one and stays perfectly healthy — so the engine passes the refusal on to whatever UI is
/// connected, and keeps it for one that connects later.
#[tokio::test(flavor = "multi_thread")]
async fn a_vault_the_server_refuses_says_so_in_the_window() {
    let tmp = tempfile::tempdir().unwrap();
    let options = ServerOptions {
        auth: AuthMode::Enabled { allow_registration: true, secure_cookies: false },
        attachments_dir: tmp.path().join("blobs"),
        ..Default::default()
    };
    let state = build_state(Store::open_in_memory().unwrap(), options);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let server_addr = listener.local_addr().unwrap();
    let app = router(state.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    // Somebody else already owns this vault id, and we are a different account.
    let vault = VaultId::new();
    let (_, owner) = post(
        format!("http://{server_addr}/api/v1/auth/register"),
        serde_json::json!({ "email": "owner@example.org", "password": "hunter2222", "display_name": "Owner" }),
    )
    .await;
    let owner_id = owner["user"]["id"].as_str().unwrap().to_owned();
    state.store.lock().await.set_membership(vault, &owner_id, Role::Owner).unwrap();
    let (_, me) = post(
        format!("http://{server_addr}/api/v1/auth/register"),
        serde_json::json!({ "email": "me@example.org", "password": "hunter2222", "display_name": "Me" }),
    )
    .await;
    let token = me["token"].as_str().unwrap().to_owned();

    let vault_dir = tmp.path().join("notes");
    let handle = relay_as(&vault_dir, Some(server_addr), Some(vault), Some(token)).await;

    // A window opening after the refusal — the ordinary case, since the engine connects while
    // the page is still loading — still hears about it.
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{}/ws", handle.addr)).await.unwrap();
    let doc = DocId::Vault(vault).to_string();
    let sv = VaultDoc::new().state_vector();
    let hello = Frame::new(&doc, &Message::Sync(SyncMessage::SyncStep1(sv))).encode();
    let mut refusal = None;
    for _ in 0..30 {
        ws.send(TMsg::Binary(hello.clone().into())).await.unwrap();
        let next = tokio::time::timeout(Duration::from_millis(500), ws.next()).await;
        if let Ok(Some(Ok(TMsg::Binary(b)))) = next
            && let Ok(frame) = Frame::decode(&b)
            && let Ok(Message::Auth(Some(reason))) = frame.message()
        {
            refusal = Some(reason);
            break;
        }
    }
    let refusal = refusal.expect("the relay passes the server's refusal on to the window");
    assert!(refusal.contains("denied"), "the window is told why: {refusal:?}");

    handle.abort();
}

//! Merging a vault that is already on a server into another one (SPEC §3.2).
//!
//! The local half is covered in `lemmate-core`'s `merge` test; what this adds is the half that
//! only exists with a server behind it. A note that moves belongs to the destination vault
//! afterwards — the server's metadata is derived from whichever vault doc names it — and the
//! vault it left stops existing there too, or every client would pull the empty shell back down
//! on its next launch.

use std::net::SocketAddr;
use std::path::Path;
use std::time::{Duration, Instant};

use lemmate_core::Store;
use lemmate_core::attachments::hash_bytes;
use lemmate_core::client::{LocalHandle, LocalOptions, SyncOptions, start_many};
use lemmate_server::{ServerOptions, build_state, router};
use serde_json::Value;

async fn server(attachments: &Path) -> SocketAddr {
    let opts = ServerOptions { attachments_dir: attachments.to_path_buf(), ..Default::default() };
    let state = build_state(Store::open_in_memory().unwrap(), opts);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router(state);
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    addr
}

async fn relay(root: &Path, dirs: &[&str], server: SocketAddr) -> LocalHandle {
    let opts = dirs
        .iter()
        .map(|d| SyncOptions {
            vault_dir: root.join(d),
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
        vault_root: Some(root.to_path_buf()),
        config_path: None,
    };
    start_many(opts, local).await.unwrap()
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

async fn json(url: String, body: Value) -> Value {
    let (code, text) = tokio::task::spawn_blocking(move || {
        let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).build().into();
        let mut r = agent
            .post(&url)
            .header("content-type", "application/json")
            .send(body.to_string().as_bytes())
            .unwrap();
        (r.status().as_u16(), r.body_mut().read_to_string().unwrap_or_default())
    })
    .await
    .unwrap();
    assert!((200..300).contains(&code), "{code}: {text}");
    serde_json::from_str(&text).unwrap_or(Value::Null)
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
async fn a_synced_vault_merged_away_moves_its_notes_and_stops_existing() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let server_addr = server(&tmp.path().join("blobs")).await;
    let remote = format!("http://{server_addr}");
    let handle = relay(&root, &["Loose", "Work"], server_addr).await;
    let base = format!("http://{}", handle.addr);
    let (from, into) = (handle.vaults[0].to_string(), handle.vaults[1].to_string());

    // Both vaults exist on the server, and the source has a note with an image.
    let bytes = b"a diagram".to_vec();
    let hash = hash_bytes(&bytes);
    tokio::task::spawn_blocking({
        let (url, bytes) = (format!("{base}/api/v1/vaults/{from}/attachments/{hash}"), bytes.clone());
        move || ureq::put(&url).header("x-filename", "diagram.png").send(&bytes[..]).unwrap()
    })
    .await
    .unwrap();
    let note = json(
        format!("{base}/api/v1/vaults/{from}/notes"),
        serde_json::json!({ "path": "Plan.md", "content": "# Plan\n\n![d](attachments/diagram.png)\n" }),
    )
    .await;
    let note_id = note["id"].as_str().unwrap().to_owned();
    json(
        format!("{base}/api/v1/vaults/{into}/notes"),
        serde_json::json!({ "path": "Kept.md", "content": "# Kept\n" }),
    )
    .await;
    until("the server to hold both vaults", async || {
        let (_, vaults) = get(format!("{remote}/api/v1/vaults")).await;
        vaults.as_array().is_some_and(|v| v.len() == 2)
    })
    .await;
    until("the server to hold the source's note", async || {
        get(format!("{remote}/api/v1/vaults/{from}/notes/{note_id}")).await.0 == 200
    })
    .await;

    // ---- Merge, with the server watching.
    let done = json(
        format!("{base}/api/v1/local/merge"),
        serde_json::json!({ "from": from, "into": into, "folder": "Loose" }),
    )
    .await;
    assert_eq!(done["applied"], true);

    // The note is the destination's now: same id, new vault, new path.
    until("the server to list the note under the destination", async || {
        let (_, notes) = get(format!("{remote}/api/v1/vaults/{into}/notes")).await;
        notes.as_array().is_some_and(|n| n.len() == 2)
    })
    .await;
    let (_, notes) = get(format!("{remote}/api/v1/vaults/{into}/notes")).await;
    let paths: Vec<&str> = notes.as_array().unwrap().iter().map(|n| n["path"].as_str().unwrap()).collect();
    assert!(paths.contains(&"Loose/Plan.md"), "moved with its folder: {paths:?}");
    assert!(paths.contains(&"Kept.md"), "the destination's own note is untouched");
    let (_, moved) = get(format!("{remote}/api/v1/vaults/{into}/notes/{note_id}")).await;
    assert_eq!(moved["path"], "Loose/Plan.md", "the same note id, in the destination");

    // The image travelled with it, and the server serves it for the destination vault.
    until("the destination's copy of the attachment", async || {
        get(format!("{remote}/api/v1/vaults/{into}/attachments/{hash}")).await.0 == 200
    })
    .await;

    // And the vault that was merged away is gone from the server, not merely empty: an empty
    // one would be pulled back down as a folder on the next launch.
    until("the server to forget the source vault", async || {
        let (_, vaults) = get(format!("{remote}/api/v1/vaults")).await;
        vaults.as_array().is_some_and(|v| v.len() == 1 && v[0]["id"] == into.as_str())
    })
    .await;
    assert!(!root.join("Loose").exists(), "and its folder is gone here");

    handle.abort();
}

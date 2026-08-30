//! The local relay's write API (SPEC §13.1 offline): notes created through the relay land on
//! disk and reach the server; replace, rename and delete follow the same path.

use std::net::SocketAddr;

use lemmate_core::client::{LocalHandle, LocalOptions, SyncOptions, start};
use lemmate_core::{Store, VaultId};
use lemmate_server::{ServerOptions, build_state, router};
use serde_json::Value;

async fn server() -> (SocketAddr, std::sync::Arc<lemmate_server::AppState>) {
    let state = build_state(Store::open_in_memory().unwrap(), ServerOptions::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router(state.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (addr, state)
}

async fn relay(server: SocketAddr, dir: &std::path::Path) -> LocalHandle {
    let opts = SyncOptions {
        vault_dir: dir.to_path_buf(),
        server_url: format!("http://{server}"),
        vault_id: None,
        once: false,
        ca_cert: None,
        token: None,
    };
    start(opts, LocalOptions { bind: "127.0.0.1:0".parse().unwrap(), web_dir: None }).await.unwrap()
}

async fn call(method: &'static str, url: String, body: Option<Value>) -> (u16, Value) {
    tokio::task::spawn_blocking(move || {
        let result = match (method, body) {
            ("GET", _) => ureq::get(&url).call(),
            ("DELETE", _) => ureq::delete(&url).call(),
            ("POST", Some(b)) => {
                ureq::post(&url).header("content-type", "application/json").send(b.to_string().as_bytes())
            }
            ("PUT", Some(b)) => {
                ureq::put(&url).header("content-type", "application/json").send(b.to_string().as_bytes())
            }
            ("PATCH", Some(b)) => {
                ureq::patch(&url).header("content-type", "application/json").send(b.to_string().as_bytes())
            }
            _ => unreachable!(),
        };
        match result {
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

#[tokio::test]
async fn relay_writes_land_on_disk_and_on_the_server() {
    let (srv, state) = server().await;
    let dir = tempfile::tempdir().unwrap();
    let handle = relay(srv, dir.path()).await;
    let vault: VaultId = handle.vault_id;
    let base = format!("http://{}/api/v1/vaults/{vault}", handle.addr);

    let (s, note) = call(
        "POST",
        format!("{base}/notes"),
        Some(serde_json::json!({"path": "Inbox/Via relay", "content": "# Relay\n\nbody\n"})),
    )
    .await;
    assert_eq!(s, 201, "{note}");
    let id = note["id"].as_str().unwrap().to_owned();
    assert_eq!(note["path"], "Inbox/Via relay.md");
    let on_disk = std::fs::read_to_string(dir.path().join("Inbox/Via relay.md")).unwrap();
    assert!(on_disk.contains(&format!("id: {id}")) && on_disk.contains("# Relay"), "{on_disk}");
    assert_eq!(
        call("POST", format!("{base}/notes"), Some(serde_json::json!({"path": "Inbox/Via relay"}))).await.0,
        409
    );

    // The server receives it through the engine.
    for i in 0..100 {
        if state.store.lock().await.list_notes(vault).unwrap().len() == 1 {
            break;
        }
        assert!(i < 99, "timed out waiting for the server row");
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }

    let (s, replaced) = call(
        "PUT",
        format!("{base}/notes/{id}"),
        Some(serde_json::json!({"content": "# Relay!\n\nnew body\n"})),
    )
    .await;
    assert_eq!(s, 200);
    assert!(replaced["content"].as_str().unwrap().contains("new body"));
    assert!(std::fs::read_to_string(dir.path().join("Inbox/Via relay.md")).unwrap().contains("new body"));
    assert_eq!(call("GET", format!("{base}/notes/{id}"), None).await.1["title"], "Relay!");

    assert_eq!(
        call("PATCH", format!("{base}/notes/{id}"), Some(serde_json::json!({"path": "Archive/Moved"})))
            .await
            .0,
        204
    );
    assert!(dir.path().join("Archive/Moved.md").is_file() && !dir.path().join("Inbox/Via relay.md").exists());
    assert_eq!(call("GET", format!("{base}/notes/{id}"), None).await.1["path"], "Archive/Moved.md");

    let (s, daily) = call("GET", format!("{base}/daily/2026-08-30"), None).await;
    assert_eq!(s, 200);
    assert_eq!(daily["path"], "Daily/2026-08-30.md");
    assert_eq!(call("GET", format!("{base}/daily/2026-08-30"), None).await.1["id"], daily["id"]);

    assert_eq!(call("DELETE", format!("{base}/notes/{id}"), None).await.0, 204);
    assert!(!dir.path().join("Archive/Moved.md").exists());
    assert_eq!(call("GET", format!("{base}/notes/{id}"), None).await.0, 404);
    for i in 0..100 {
        if state.store.lock().await.list_notes(vault).unwrap().iter().all(|n| n.path != "Archive/Moved.md") {
            break;
        }
        assert!(i < 99, "timed out waiting for the server trash");
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    handle.abort();
}

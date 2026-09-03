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
    start(opts, LocalOptions { bind: "127.0.0.1:0".parse().unwrap(), web_dir: None, vault_root: None })
        .await
        .unwrap()
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

const BOUNDARY: &str = "----lemmaterelay";

/// A multipart body of (vault-relative path, bytes) parts, as a browser `FormData` of picked
/// files produces.
fn multipart(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut body = Vec::new();
    for (path, bytes) in files {
        body.extend_from_slice(format!("--{BOUNDARY}\r\n").as_bytes());
        body.extend_from_slice(
            format!("content-disposition: form-data; name=\"file\"; filename=\"{path}\"\r\n\r\n").as_bytes(),
        );
        body.extend_from_slice(bytes);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{BOUNDARY}--\r\n").as_bytes());
    body
}

async fn import(url: String, files: &[(&str, &[u8])]) -> (u16, Value) {
    let body = multipart(files);
    tokio::task::spawn_blocking(move || {
        let result = ureq::post(&url)
            .header("content-type", &format!("multipart/form-data; boundary={BOUNDARY}"))
            .send(&body[..]);
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

/// SPEC §11.4 through the relay: the same endpoint the server offers, except that here the
/// converted files land in the vault folder and travel on from there.
#[tokio::test]
async fn obsidian_import_writes_the_vault_folder_and_reaches_the_server() {
    let (srv, state) = server().await;
    let dir = tempfile::tempdir().unwrap();
    let handle = relay(srv, dir.path()).await;
    let vault: VaultId = handle.vault_id;
    let base = format!("http://{}/api/v1/vaults/{vault}", handle.addr);

    let (s, report) = import(
        format!("{base}/import"),
        &[
            ("Projects/plan.md", b"> [!warning] Careful\n> body\n\n![[logo.png]]\n"),
            ("logo.png", b"\x89PNG not really"),
            (".obsidian/workspace.json", b"{}"),
            (
                ".obsidian/bookmarks.json",
                br#"{"items":[{"type":"file","path":"Projects/plan.md","title":"Plan"}]}"#,
            ),
            (".obsidian/daily-notes.json", br#"{"folder":"Daily","format":"YYYY-MM-DD"}"#),
        ],
    )
    .await;
    assert_eq!(s, 200, "{report}");
    assert_eq!(report["notes"], 1);
    assert_eq!(report["attachments"], 1);
    assert_eq!(report["callouts"], 1);
    assert_eq!(report["embeds"], 1);
    assert_eq!(report["bookmarks"], 1);
    // Unlike the server, the relay has a sidecar to keep daily-note settings in.
    assert_eq!(report["daily_notes"], true);

    // Converted markdown, the attachment beside it, and the settings in the sidecar.
    let note = std::fs::read_to_string(dir.path().join("Projects/plan.md")).unwrap();
    assert!(note.contains("::: {.callout-warning title=\"Careful\"}"), "{note}");
    assert!(note.contains("![](logo.png)"), "{note}");
    assert!(dir.path().join("logo.png").is_file());
    let daily = std::fs::read_to_string(dir.path().join(".lemmate/daily.import.json")).unwrap();
    assert!(daily.contains("\"folder\": \"Daily\""), "{daily}");

    // Re-uploading the same batch is a no-op, not a second copy.
    let (s, again) =
        import(format!("{base}/import"), &[("Projects/plan.md", b"> [!warning] Careful\n> body\n")]).await;
    assert_eq!(s, 200);
    assert_eq!(again["notes"], 0);
    assert_eq!(again["skipped"], 1);

    // The note reaches the server like any other local edit.
    for i in 0..100 {
        if state.store.lock().await.list_notes(vault).unwrap().len() == 1 {
            break;
        }
        assert!(i < 99, "timed out waiting for the imported note to sync");
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let rows = state.store.lock().await.list_notes(vault).unwrap();
    assert_eq!(rows[0].path, "Projects/plan.md");
    handle.abort();
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

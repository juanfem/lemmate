//! The file manager against the server (SPEC §9): files at chosen paths, kept whether or not a
//! note uses them; conflicts; a move that carries the notes along; delete.

use lemmate_core::{Store, VaultId};
use lemmate_server::{ServerOptions, build_state, router};
use serde_json::Value;

async fn serve() -> String {
    let state = build_state(Store::open_in_memory().unwrap(), ServerOptions::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router(state)).await.unwrap() });
    format!("http://{addr}/api/v1/vaults/{}", VaultId::new())
}

async fn request(
    method: &'static str,
    url: String,
    headers: Vec<(&'static str, String)>,
    body: Vec<u8>,
) -> (u16, Value) {
    tokio::task::spawn_blocking(move || {
        let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).build().into();
        let mut r = match method {
            "GET" => agent.get(&url).call().unwrap(),
            "PUT" => {
                let mut req = agent.put(&url);
                for (k, v) in &headers {
                    req = req.header(*k, v);
                }
                req.send(&body[..]).unwrap()
            }
            "POST" => agent.post(&url).header("content-type", "application/json").send(&body[..]).unwrap(),
            "DELETE" => agent.delete(&url).call().unwrap(),
            _ => unreachable!(),
        };
        let text = r.body_mut().read_to_string().unwrap_or_default();
        (r.status().as_u16(), serde_json::from_str(&text).unwrap_or(Value::String(text)))
    })
    .await
    .unwrap()
}

async fn list(base: &str) -> Vec<Value> {
    request("GET", format!("{base}/files"), vec![], vec![]).await.1.as_array().unwrap().clone()
}

#[tokio::test]
async fn files_are_managed_by_path() {
    let base = serve().await;
    let url = |p: &str| format!("{base}/files?path={p}");

    let theme = b"/*-- scss:defaults --*/\n@import 'vars';\n".to_vec();
    let h = lemmate_core::attachments::hash_bytes(&theme);
    assert_eq!(request("PUT", url("Slides/2026/custom.scss"), vec![], theme.clone()).await.0, 201);
    assert_eq!(request("PUT", url("Slides/2026/_vars.scss"), vec![], b"$ink: #111;\n".to_vec()).await.0, 201);
    let files = list(&base).await;
    assert_eq!(files.len(), 2);
    assert!(files.iter().all(|f| f["kept"] == true && f["used_by"].as_array().unwrap().is_empty()));
    assert_eq!(files.iter().find(|f| f["path"] == "Slides/2026/custom.scss").unwrap()["size"], theme.len());

    // Taken: a conflict that says what is there, unless replacing from the copy that is.
    let (code, body) = request("PUT", url("Slides/2026/custom.scss"), vec![], b"x".to_vec()).await;
    assert_eq!((code, body["current"].as_str()), (409, Some(h.as_str())));
    let stale = vec![("x-replace", "true".to_owned()), ("x-base-hash", "0".repeat(64))];
    assert_eq!(request("PUT", url("Slides/2026/custom.scss"), stale, b"x".to_vec()).await.0, 409);
    let fresh = vec![("x-replace", "true".to_owned()), ("x-base-hash", h.clone())];
    let (code, body) = request("PUT", url("Slides/2026/custom.scss"), fresh, theme.clone()).await;
    assert_eq!(code, 200, "{body}");
    assert_eq!(request("PUT", url("Slides/Deck.md"), vec![], b"x".to_vec()).await.0, 400, "not for notes");

    // A note using the theme uses what it imports too.
    let (code, note) = request(
        "POST",
        format!("{base}/notes"),
        vec![],
        serde_json::json!({
            "path": "Slides/2026/Deck.qmd",
            "content": "---\nformat:\n  html:\n    theme: [cosmo, custom.scss]\n---\nBody.\n",
        })
        .to_string()
        .into_bytes(),
    )
    .await;
    assert_eq!(code, 201, "{note}");
    let id = note["id"].as_str().unwrap().to_owned();
    let files = list(&base).await;
    for path in ["Slides/2026/custom.scss", "Slides/2026/_vars.scss"] {
        let f = files.iter().find(|f| f["path"] == path).unwrap();
        assert_eq!(f["used_by"], serde_json::json!([id]), "{path}");
    }

    let (code, moved) = request(
        "POST",
        format!("{base}/files/move"),
        vec![],
        serde_json::json!({ "from": "Slides/2026/custom.scss", "to": "styles/deck.scss" })
            .to_string()
            .into_bytes(),
    )
    .await;
    assert_eq!(code, 200, "{moved}");
    assert_eq!(moved["rewritten"], 1);
    let (_, n) = request("GET", format!("{base}/notes/{id}"), vec![], vec![]).await;
    assert!(n["content"].as_str().unwrap().contains("theme: [cosmo, ../../styles/deck.scss]"), "{n}");
    let files = list(&base).await;
    let moved = files.iter().find(|f| f["path"] == "styles/deck.scss").unwrap();
    assert_eq!((moved["kept"].clone(), moved["hash"].as_str()), (Value::Bool(true), Some(h.as_str())));
    assert!(files.iter().all(|f| f["path"] != "Slides/2026/custom.scss"));
    let taken = serde_json::json!({ "from": "styles/deck.scss", "to": "Slides/2026/_vars.scss" });
    assert_eq!(
        request("POST", format!("{base}/files/move"), vec![], taken.to_string().into_bytes()).await.0,
        409
    );

    assert_eq!(request("DELETE", url("styles/deck.scss"), vec![], vec![]).await.0, 204);
    assert_eq!(request("DELETE", url("styles/deck.scss"), vec![], vec![]).await.0, 404);
    assert!(list(&base).await.iter().all(|f| f["path"] != "styles/deck.scss"));
}

async fn until(what: &str, mut f: impl AsyncFnMut() -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while std::time::Instant::now() < deadline {
        if f().await {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("timed out waiting for {what}");
}

/// A kept file deleted or moved on the server goes from a desktop's folder too — the copy that
/// was synced, not one edited there since, which is somebody's work.
#[tokio::test(flavor = "multi_thread")]
async fn a_desktop_follows_deletes_and_moves_of_kept_files() {
    let tmp = tempfile::tempdir().unwrap();
    let opts = ServerOptions { attachments_dir: tmp.path().join("blobs"), ..Default::default() };
    let state = build_state(Store::open_in_memory().unwrap(), opts);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router(state)).await.unwrap() });
    let sync = lemmate_core::client::SyncOptions {
        vault_dir: tmp.path().join("root/notes"),
        server_url: Some(format!("http://{addr}")),
        vault_id: None,
        once: false,
        ca_cert: None,
        token: None,
    };
    let local = lemmate_core::client::LocalOptions {
        bind: "127.0.0.1:0".parse().unwrap(),
        web_dir: None,
        vault_root: Some(tmp.path().join("root")),
        config_path: None,
    };
    let handle = lemmate_core::client::start_many(vec![sync], local).await.unwrap();
    let relay = format!("http://{}/api/v1/vaults/{}", handle.addr, handle.vault_id);
    let server = format!("http://{addr}/api/v1/vaults/{}", handle.vault_id);
    let dir = tmp.path().join("root/notes");

    for (path, text) in [("keep/a.css", "a{}"), ("keep/b.css", "b{}"), ("keep/c.css", "c{}")] {
        let (code, body) =
            request("PUT", format!("{relay}/files?path={path}"), vec![], text.as_bytes().to_vec()).await;
        assert_eq!(code, 201, "{body}");
    }
    until("the server to have all three", async || list(&server).await.len() == 3).await;

    // Deleted on the server: gone here too.
    assert_eq!(request("DELETE", format!("{server}/files?path=keep/a.css"), vec![], vec![]).await.0, 204);
    until("the copy here to go", async || !dir.join("keep/a.css").exists()).await;

    // Edited here and not synced yet — the watcher's debounce outlasts the request below — then
    // deleted there: the edit stays.
    std::fs::write(dir.join("keep/b.css"), "b { edited }").unwrap();
    assert_eq!(request("DELETE", format!("{server}/files?path=keep/b.css"), vec![], vec![]).await.0, 204);
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    assert_eq!(std::fs::read_to_string(dir.join("keep/b.css")).unwrap(), "b { edited }");

    // Moved there: arrives at the new path here, and leaves the old one.
    let body = serde_json::json!({ "from": "keep/c.css", "to": "moved/c.css" }).to_string().into_bytes();
    assert_eq!(request("POST", format!("{server}/files/move"), vec![], body).await.0, 200);
    until("the move to arrive", async || {
        dir.join("moved/c.css").is_file() && !dir.join("keep/c.css").exists()
    })
    .await;
    assert_eq!(std::fs::read_to_string(dir.join("moved/c.css")).unwrap(), "c{}");

    handle.abort();
}

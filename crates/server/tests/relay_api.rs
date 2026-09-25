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
        server_url: Some(format!("http://{server}")),
        vault_id: None,
        once: false,
        ca_cert: None,
        token: None,
    };
    start(
        opts,
        LocalOptions {
            bind: "127.0.0.1:0".parse().unwrap(),
            web_dir: None,
            vault_root: None,
            config_path: None,
        },
    )
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
            (".obsidian/daily-notes.json", br#"{"folder":"Journal","format":"DD.MM.YYYY"}"#),
        ],
    )
    .await;
    assert_eq!(s, 200, "{report}");
    assert_eq!(report["notes"], 1);
    assert_eq!(report["attachments"], 1);
    assert_eq!(report["callouts"], 1);
    assert_eq!(report["embeds"], 1);
    assert_eq!(report["bookmarks"], 1);
    assert_eq!(report["daily_notes"], true);

    // Converted markdown and the attachment beside it; the settings are in the vault doc, so the
    // relay files a day by them.
    let note = std::fs::read_to_string(dir.path().join("Projects/plan.md")).unwrap();
    assert!(note.contains("::: {.callout-warning title=\"Careful\"}"), "{note}");
    assert!(note.contains("![](logo.png)"), "{note}");
    assert!(dir.path().join("logo.png").is_file());
    let (s, daily) = call("GET", format!("{base}/daily/2026-09-25"), None).await;
    assert_eq!(s, 200);
    assert_eq!(daily["path"], "Journal/25.09.2026.md");
    assert!(dir.path().join("Journal/25.09.2026.md").is_file());

    // Re-uploading the same batch is a no-op, not a second copy.
    let (s, again) =
        import(format!("{base}/import"), &[("Projects/plan.md", b"> [!warning] Careful\n> body\n")]).await;
    assert_eq!(s, 200);
    assert_eq!(again["notes"], 0);
    assert_eq!(again["skipped"], 1);

    // The note reaches the server like any other local edit, and the settings with the vault doc.
    let day = lemmate_core::daily::Date::parse("2026-09-25").unwrap();
    for i in 0..100 {
        let store = state.store.lock().await;
        if store.list_notes(vault).unwrap().iter().any(|n| n.path == "Projects/plan.md")
            && store.load_vault_doc(vault).unwrap().daily().path_for(day) == "Journal/25.09.2026.md"
        {
            break;
        }
        drop(store);
        assert!(i < 99, "timed out waiting for the import to sync");
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
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

/// The desktop's account menu (`local::auth_me`, `local::sign_out`): the relay says who its token
/// belongs to by asking the server, calls a refused or missing token *signed out*, and hands a
/// sign-out to the shell.
#[tokio::test]
async fn the_relay_names_its_account_and_hands_sign_out_to_the_shell() {
    let options = ServerOptions {
        auth: lemmate_server::AuthMode::Enabled { allow_registration: false, secure_cookies: false },
        ..ServerOptions::default()
    };
    let state = build_state(Store::open_in_memory().unwrap(), options);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let srv = listener.local_addr().unwrap();
    let app = router(state.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).build().into();
    let send = move |method: &'static str, url: String, bearer: Option<String>, body: Option<Value>| {
        let agent = agent.clone();
        tokio::task::spawn_blocking(move || {
            let mut r = match method {
                "GET" => {
                    let mut req = agent.get(&url);
                    if let Some(t) = &bearer {
                        req = req.header("authorization", &format!("Bearer {t}"));
                    }
                    req.call().unwrap()
                }
                _ => {
                    let mut req = agent.post(&url).header("content-type", "application/json");
                    if let Some(t) = &bearer {
                        req = req.header("authorization", &format!("Bearer {t}"));
                    }
                    req.send(body.unwrap_or(Value::Null).to_string().as_bytes()).unwrap()
                }
            };
            let text = r.body_mut().read_to_string().unwrap_or_default();
            (r.status().as_u16(), serde_json::from_str::<Value>(&text).unwrap_or(Value::Null))
        })
    };
    let base = format!("http://{srv}");
    let (_, reg) = send(
        "POST",
        format!("{base}/api/v1/auth/register"),
        None,
        Some(serde_json::json!({"email": "ann@example.org", "password": "long enough"})),
    )
    .await
    .unwrap();
    let session = reg["token"].as_str().unwrap().to_owned();
    let (_, tok) = send(
        "POST",
        format!("{base}/api/v1/tokens"),
        Some(session),
        Some(serde_json::json!({"name": "desktop"})),
    )
    .await
    .unwrap();
    let token = tok["token"].as_str().unwrap().to_owned();

    let relay_with = |token: Option<String>, dir: std::path::PathBuf, config: Option<std::path::PathBuf>| {
        let base = base.clone();
        async move {
            start(
                SyncOptions {
                    vault_dir: dir,
                    server_url: Some(base),
                    vault_id: None,
                    once: false,
                    ca_cert: None,
                    token,
                },
                LocalOptions {
                    bind: "127.0.0.1:0".parse().unwrap(),
                    web_dir: None,
                    vault_root: None,
                    config_path: config,
                },
            )
            .await
            .unwrap()
        }
    };
    let (d1, d2, d3) =
        (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());

    // Signed in: the server's answer, as the server gives it.
    let mut signed_in =
        relay_with(Some(token.clone()), d1.path().into(), Some(d1.path().join("desktop.toml"))).await;
    let (s, me) = send("GET", format!("http://{}/api/v1/auth/me", signed_in.addr), None, None).await.unwrap();
    assert_eq!(s, 200, "{me}");
    assert_eq!(me["email"], "ann@example.org");
    assert_eq!(me["token"]["name"], "desktop");

    // No token for a server that wants one: signed out, and of which server.
    let none = relay_with(None, d2.path().into(), None).await;
    let (s, body) = send("GET", format!("http://{}/api/v1/auth/me", none.addr), None, None).await.unwrap();
    assert_eq!(s, 401);
    assert_eq!(body["signed_out"], base);
    // A relay nothing can reconfigure (`lemmate serve`) cannot sign out; the CLI does that.
    let (s, _) = send("POST", format!("http://{}/api/v1/auth/logout", none.addr), None, None).await.unwrap();
    assert_eq!(s, 501);

    // Standalone: no account at all.
    let alone = start(
        SyncOptions {
            vault_dir: d3.path().into(),
            server_url: None,
            vault_id: None,
            once: false,
            ca_cert: None,
            token: None,
        },
        LocalOptions {
            bind: "127.0.0.1:0".parse().unwrap(),
            web_dir: None,
            vault_root: None,
            config_path: None,
        },
    )
    .await
    .unwrap();
    let (s, _) = send("GET", format!("http://{}/api/v1/auth/me", alone.addr), None, None).await.unwrap();
    assert_eq!(s, 404);

    // A sign-out reaches the shell, and the page hears the shell's answer.
    let mut asks = signed_in.sign_out.take().unwrap();
    let shell = tokio::spawn(async move {
        let ask = asks.recv().await.unwrap();
        ask.reply.send(Ok(())).unwrap();
    });
    let (s, _) =
        send("POST", format!("http://{}/api/v1/auth/logout", signed_in.addr), None, None).await.unwrap();
    assert_eq!(s, 204);
    shell.await.unwrap();
    for h in [signed_in, none, alone] {
        h.abort();
    }
}

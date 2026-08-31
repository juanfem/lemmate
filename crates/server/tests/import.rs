//! Obsidian import over the API (SPEC §11.4): the browser uploads the picked folder as
//! multipart batches, the server converts them with `lemmate_core::import` and creates the
//! notes through the room docs.

use std::net::SocketAddr;
use std::sync::Arc;

use lemmate_core::{Store, VaultId};
use lemmate_server::{AppState, AuthMode, ServerOptions, build_state, router};
use serde_json::{Value, json};

async fn start(auth: AuthMode) -> (SocketAddr, Arc<AppState>) {
    let dir = tempfile::tempdir().unwrap();
    let options =
        ServerOptions { auth, attachments_dir: dir.keep().join("attachments"), ..ServerOptions::default() };
    let state = build_state(Store::open_in_memory().unwrap(), options);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router(state.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (addr, state)
}

const BOUNDARY: &str = "----lemmatetest";

/// A multipart body of (vault-relative path, bytes) parts, the shape a browser `FormData` of
/// picked files produces.
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

async fn import(
    addr: SocketAddr,
    vault: VaultId,
    files: &[(&str, &[u8])],
    token: Option<&str>,
) -> (u16, Value) {
    let mut req = ureq::post(format!("http://{addr}/api/v1/vaults/{vault}/import"))
        .header("content-type", &format!("multipart/form-data; boundary={BOUNDARY}"));
    if let Some(t) = token {
        req = req.header("authorization", &format!("Bearer {t}"));
    }
    let body = multipart(files);
    tokio::task::spawn_blocking(move || match req.send(&body[..]) {
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

#[tokio::test]
async fn uploaded_vault_becomes_notes_attachments_and_bookmarks() {
    let (addr, state) = start(AuthMode::Disabled).await;
    let vault = VaultId::new();

    let (status, report) = import(
        addr,
        vault,
        &[
            ("Projects/plan.md", b"> [!warning] Careful\n> body\n\n![[logo.png]]\n"),
            ("logo.png", b"\x89PNG not really"),
            (".obsidian/workspace.json", b"{\"ignored\": true}"),
            (
                ".obsidian/bookmarks.json",
                br#"{"items":[{"type":"file","path":"Projects/plan.md","title":"Plan"}]}"#,
            ),
            (".obsidian/daily-notes.json", br#"{"folder":"Daily","format":"YYYY-MM-DD"}"#),
        ],
        None,
    )
    .await;
    assert_eq!(status, 200, "{report}");
    assert_eq!(report["notes"], 1);
    assert_eq!(report["attachments"], 1);
    assert_eq!(report["callouts"], 1);
    assert_eq!(report["embeds"], 1);
    assert_eq!(report["bookmarks"], 1);
    assert_eq!(report["skipped"], 0);
    // Settings with nowhere to live on the server are reported as not stored, never as imported.
    assert_eq!(report["daily_notes"], false);

    // The note is a real note: derived metadata, and Obsidian syntax converted.
    let (_, notes) = get(addr, &format!("/api/v1/vaults/{vault}/notes"), None).await;
    let list = notes.as_array().unwrap();
    assert_eq!(list.len(), 1, "{notes}");
    assert_eq!(list[0]["path"], "Projects/plan.md");
    let id = list[0]["id"].as_str().unwrap();
    let (_, note) = get(addr, &format!("/api/v1/vaults/{vault}/notes/{id}"), None).await;
    let content = note["content"].as_str().unwrap();
    assert!(content.contains("::: {.callout-warning title=\"Careful\"}"), "{content}");
    assert!(content.contains("![](logo.png)"), "{content}");
    assert!(content.contains(&format!("id: {id}")), "front matter carries the id: {content}");

    // The attachment is in the blob store under its hash, and in the vault doc by path.
    let doc = state.store.lock().await.load_vault_doc(vault).unwrap();
    assert_eq!(doc.attachment_entries().len(), 1);
    let (path, hash) = doc.attachment_entries().into_iter().next().unwrap();
    assert_eq!(path, "logo.png");
    let (status, _) = get(addr, &format!("/api/v1/vaults/{vault}/attachments/{hash}"), None).await;
    assert_eq!(status, 200);
    assert_eq!(doc.bookmarks().len(), 1);
    assert_eq!(doc.bookmarks()[0].target, "Projects/plan.md");

    // Uploading the same batch again changes nothing: paths already taken are skipped.
    let (status, again) = import(
        addr,
        vault,
        &[
            ("Projects/plan.md", b"> [!warning] Careful\n> body\n"),
            (
                ".obsidian/bookmarks.json",
                br#"{"items":[{"type":"file","path":"Projects/plan.md","title":"Plan"}]}"#,
            ),
        ],
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(again["notes"], 0);
    assert_eq!(again["skipped"], 1);
    assert_eq!(again["bookmarks"], 0);
    let (_, notes) = get(addr, &format!("/api/v1/vaults/{vault}/notes"), None).await;
    assert_eq!(notes.as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn importing_claims_an_unowned_vault_but_a_viewer_is_refused() {
    let (addr, _state) = start(AuthMode::Enabled { allow_registration: false, secure_cookies: false }).await;
    let (_, body) = post(
        addr,
        "/api/v1/auth/register",
        json!({"email": "ann@example.org", "password": "one two three"}),
        None,
    )
    .await;
    let ann = body["token"].as_str().unwrap().to_owned();
    let (_, body) = post(
        addr,
        "/api/v1/auth/register",
        json!({"email": "bob@example.org", "password": "four five six"}),
        Some(&ann),
    )
    .await;
    assert_eq!(body["is_admin"], false);
    let (_, body) = post(
        addr,
        "/api/v1/auth/login",
        json!({"email": "bob@example.org", "password": "four five six"}),
        None,
    )
    .await;
    let bob = body["token"].as_str().unwrap().to_owned();

    // Ann imports into a vault nobody owns: the import claims it for her.
    let vault = VaultId::new();
    let (status, report) = import(addr, vault, &[("a.md", b"hello\n")], Some(&ann)).await;
    assert_eq!(status, 200, "{report}");
    assert_eq!(report["notes"], 1);

    // Bob is not a member: the vault does not exist as far as he is concerned.
    assert_eq!(import(addr, vault, &[("b.md", b"hi\n")], Some(&bob)).await.0, 404);

    // As a viewer he can read it, but importing is a write.
    let status = tokio::task::spawn_blocking({
        let url = format!("http://{addr}/api/v1/vaults/{vault}/members");
        let token = ann.clone();
        move || match ureq::put(&url)
            .header("content-type", "application/json")
            .header("authorization", &format!("Bearer {token}"))
            .send(json!({"email": "bob@example.org", "role": "viewer"}).to_string().as_bytes())
        {
            Ok(r) => r.status().as_u16(),
            Err(ureq::Error::StatusCode(c)) => c,
            Err(e) => panic!("{e}"),
        }
    })
    .await
    .unwrap();
    assert_eq!(status, 204);
    assert_eq!(import(addr, vault, &[("b.md", b"hi\n")], Some(&bob)).await.0, 403);

    // Nothing an unauthorised request sent was applied.
    let (_, notes) = get(addr, &format!("/api/v1/vaults/{vault}/notes"), Some(&ann)).await;
    assert_eq!(notes.as_array().unwrap().len(), 1);
}

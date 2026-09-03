//! A vault with no server at all (SPEC §3.2): the relay, the engines, the projection and the
//! search index, all on one machine and nothing on the wire.
//!
//! The rest of the relay's API is covered against a real server in `lemmate-server`'s tests;
//! what is specific here is that none of it needs one.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use lemmate_core::client::{LocalHandle, LocalOptions, SyncOptions, start};
use serde_json::Value;

async fn relay(root: &Path) -> LocalHandle {
    relay_with_config(root, None).await
}

/// `config_path` is what a shell that can rewrite its own configuration passes; without one the
/// relay refuses to be reconfigured from the page (`lemmate serve`).
async fn relay_with_config(root: &Path, config_path: Option<PathBuf>) -> LocalHandle {
    let opts = SyncOptions {
        vault_dir: root.join("notes"),
        server_url: None,
        vault_id: None,
        once: false,
        ca_cert: None,
        token: None,
    };
    let local = LocalOptions {
        bind: "127.0.0.1:0".parse().unwrap(),
        web_dir: None,
        vault_root: Some(root.to_path_buf()),
        config_path,
    };
    start(opts, local).await.unwrap()
}

async fn get(url: String) -> (u16, Value) {
    call("GET", url, None).await
}

/// Like [`call`] but keeps the body as text on failures too, which is where the reason is —
/// hence the agent that does not turn a 4xx/5xx into an error and throw the body away.
async fn post_text(url: String, body: Value) -> (u16, String) {
    tokio::task::spawn_blocking(move || {
        let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).build().into();
        let mut r = agent
            .post(&url)
            .header("content-type", "application/json")
            .send(body.to_string().as_bytes())
            .unwrap();
        (r.status().as_u16(), r.body_mut().read_to_string().unwrap_or_default())
    })
    .await
    .unwrap()
}

async fn call(method: &'static str, url: String, body: Option<Value>) -> (u16, Value) {
    tokio::task::spawn_blocking(move || {
        let result = match (method, body) {
            ("GET", _) => ureq::get(&url).call(),
            ("POST", Some(b)) => {
                ureq::post(&url).header("content-type", "application/json").send(b.to_string().as_bytes())
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

/// Poll until `f` holds, so a debounced write does not race the assertion.
async fn until(what: &str, mut f: impl AsyncFnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if f().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("timed out waiting for {what}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_vault_with_no_server_works_end_to_end() {
    let tmp = tempfile::tempdir().unwrap();
    let handle = relay(tmp.path()).await;
    let base = format!("http://{}", handle.addr);
    let vault = handle.vault_id.to_string();

    // The UI asks this to decide it is a standalone app, not a shell waiting for a setup form.
    let (code, setup) = get(format!("{base}/api/v1/local/setup")).await;
    assert_eq!(code, 200);
    assert_eq!(setup["configured"], true);
    assert_eq!(setup["mode"], "local");
    assert_eq!(setup["server"], Value::Null);
    // No configuration file to write into: the UI must not offer to connect a server here.
    assert_eq!(setup["can_connect"], false);
    let (code, _) = post_text(
        format!("{base}/api/v1/local/connect"),
        serde_json::json!({ "server_url": "https://notes.example.org" }),
    )
    .await;
    assert_eq!(code, 501, "a relay with no config file says so rather than pretending");

    let (code, note) = call(
        "POST",
        format!("{base}/api/v1/vaults/{vault}/notes"),
        Some(serde_json::json!({ "path": "Ideas/kettle.md", "content": "# Kettle\n\nboil #soon\n" })),
    )
    .await;
    assert_eq!(code, 201, "{note}");
    let id = note["id"].as_str().unwrap().to_owned();

    // Files are a projection of the CRDT, with or without a server.
    let file = tmp.path().join("notes").join("Ideas/kettle.md");
    until("the note to be written to disk", async || file.is_file()).await;
    assert!(std::fs::read_to_string(&file).unwrap().contains("boil #soon"));

    // Everything the server would otherwise answer, answered locally.
    let (_, hits) = get(format!("{base}/api/v1/vaults/{vault}/search?q=kettle")).await;
    assert_eq!(hits[0]["note_id"], id, "search should find the new note: {hits}");
    let (_, tags) = get(format!("{base}/api/v1/vaults/{vault}/tags")).await;
    assert_eq!(tags[0]["tag"], "soon", "{tags}");
    let (_, vaults) = get(format!("{base}/api/v1/vaults")).await;
    assert_eq!(vaults.as_array().unwrap().len(), 1);

    handle.abort();
}

/// An attachment has nowhere to be uploaded to, so the vault-doc entry that a completed upload
/// would write has to be written locally — otherwise the image is on disk and invisible.
#[tokio::test(flavor = "multi_thread")]
async fn attachments_are_recorded_without_an_upload() {
    let tmp = tempfile::tempdir().unwrap();
    let handle = relay(tmp.path()).await;
    let base = format!("http://{}", handle.addr);
    let vault = handle.vault_id.to_string();

    let bytes = b"not really a png".to_vec();
    let hash = lemmate_core::attachments::hash_bytes(&bytes);
    let url = format!("{base}/api/v1/vaults/{vault}/attachments/{hash}");
    let stored = tokio::task::spawn_blocking({
        let url = url.clone();
        let bytes = bytes.clone();
        move || {
            let mut r = ureq::put(&url).header("x-filename", "kettle.png").send(&bytes[..]).unwrap();
            serde_json::from_str::<Value>(&r.body_mut().read_to_string().unwrap()).unwrap()
        }
    })
    .await
    .unwrap();
    let path = stored["path"].as_str().unwrap().to_owned();
    assert_eq!(stored["hash"], hash);

    // Referencing it from a note is what makes it an attachment of this vault.
    let (code, _) = call(
        "POST",
        format!("{base}/api/v1/vaults/{vault}/notes"),
        Some(serde_json::json!({ "path": "kettle.md", "content": format!("![kettle]({path})\n") })),
    )
    .await;
    assert_eq!(code, 201);

    until("the attachment to be served by hash", async || get(url.clone()).await.0 == 200).await;
    let served = tokio::task::spawn_blocking(move || {
        ureq::get(&url).call().unwrap().body_mut().read_to_vec().unwrap()
    })
    .await
    .unwrap();
    assert_eq!(served, bytes, "the relay serves the bytes it stored");

    handle.abort();
}

/// Connecting a standalone app to a server (SPEC §3.2). The relay only carries the request: the
/// shell signs in, writes the configuration and restarts, and the HTTP answer is the shell's, so
/// the dialog can say what went wrong.
#[tokio::test(flavor = "multi_thread")]
async fn connecting_a_server_is_answered_by_the_shell() {
    let tmp = tempfile::tempdir().unwrap();
    let mut handle = relay_with_config(tmp.path(), Some(tmp.path().join("desktop.toml"))).await;
    let base = format!("http://{}", handle.addr);

    let (_, setup) = get(format!("{base}/api/v1/local/setup")).await;
    assert_eq!(setup["can_connect"], true);
    assert_eq!(setup["config_path"], tmp.path().join("desktop.toml").display().to_string());

    // A shell standing in for the desktop one: it refuses the first attempt and takes the second.
    let mut rx = handle.connect.take().expect("a relay with a config file offers connect requests");
    tokio::spawn(async move {
        let first = rx.recv().await.expect("first request");
        assert_eq!(first.request.server_url, "https://notes.example.org");
        assert_eq!(first.request.email.as_deref(), Some("me@example.org"));
        let _ = first.reply.send(Err("signing in: 401 Unauthorized".into()));
        let second = rx.recv().await.expect("second request");
        let _ = second.reply.send(Ok(()));
    });

    let (code, body) = post_text(
        format!("{base}/api/v1/local/connect"),
        serde_json::json!({
            "server_url": "https://notes.example.org",
            "email": "me@example.org",
            "password": "wrong",
        }),
    )
    .await;
    assert_eq!(code, 502);
    assert!(body.contains("401"), "the dialog is told what actually failed: {body:?}");

    let (code, _) = post_text(
        format!("{base}/api/v1/local/connect"),
        serde_json::json!({ "server_url": "https://notes.example.org" }),
    )
    .await;
    assert_eq!(code, 202, "accepted: the shell is about to restart onto it");

    // A URL that is not one never reaches the shell.
    let (code, _) = post_text(
        format!("{base}/api/v1/local/connect"),
        serde_json::json!({ "server_url": "notes.example.org" }),
    )
    .await;
    assert_eq!(code, 400);

    handle.abort();
}

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

/// "Render with Quarto" reads the vault's own folder: an image the note embeds from another
/// folder ends up inside the rendered page. With no quarto on this machine, a 501 says so.
#[tokio::test(flavor = "multi_thread")]
async fn a_note_renders_through_quarto_with_its_images() {
    let tmp = tempfile::tempdir().unwrap();
    let handle = relay(tmp.path()).await;
    let base = format!("http://{}", handle.addr);
    let vault = handle.vault_id.to_string();

    // A 1×1 PNG, so Quarto has a real image to embed.
    let png: Vec<u8> = vec![
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 13, 0x49, 0x48, 0x44, 0x52, 0, 0, 0, 1, 0,
        0, 0, 1, 8, 6, 0, 0, 0, 0x1f, 0x15, 0xc4, 0x89, 0, 0, 0, 13, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c,
        0x63, 0xf8, 0xcf, 0xc0, 0xf0, 0x1f, 0, 0x05, 0, 0x01, 0xff, 0x89, 0x99, 0x3d, 0x1d, 0, 0, 0, 0, 0x49,
        0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ];
    let hash = lemmate_core::attachments::hash_bytes(&png);
    let url = format!("{base}/api/v1/vaults/{vault}/attachments/{hash}");
    tokio::task::spawn_blocking({
        let url = url.clone();
        move || ureq::put(&url).header("x-filename", "dot.png").send(&png[..]).unwrap()
    })
    .await
    .unwrap();
    let (code, note) = call(
        "POST",
        format!("{base}/api/v1/vaults/{vault}/notes"),
        Some(serde_json::json!({
            "path": "Talks/dot.qmd",
            "content": "---\ntitle: A dot\n---\n\n![[dot.png]]\n\n```{python}\nprint(6 * 7)\n```\n",
        })),
    )
    .await;
    assert_eq!(code, 201, "{note}");
    let id = note["id"].as_str().unwrap().to_owned();
    until("the attachment to be recorded", async || get(url.clone()).await.0 == 200).await;

    let (code, html) = post_text(
        format!("{base}/api/v1/vaults/{vault}/notes/{id}/render"),
        serde_json::json!({ "format": "html" }),
    )
    .await;
    if lemmate_core::quarto::quarto_available(None) {
        assert_eq!(code, 200, "{html}");
        assert!(html.contains("A dot"), "the title");
        assert!(html.contains("data:image/png"), "the image from attachments/ is embedded");
        assert!(!html.contains(">42<"), "code cells are not executed");
    } else {
        assert_eq!(code, 501, "no quarto here, and the relay says so");
    }
    let (code, _) = post_text(
        format!("{base}/api/v1/vaults/{vault}/notes/{id}/render"),
        serde_json::json!({ "format": "odt" }),
    )
    .await;
    assert_eq!(code, 400, "an unknown format is the caller's mistake");

    handle.abort();
}

/// A note's companion files are attachments like any image: a theme its front matter names, the
/// partial that theme imports, the vault's `_quarto.yml` — recorded in the vault doc (which is
/// what syncs them, and what keeps a server from purging them). A partial added to the theme
/// later is picked up although no note changed.
#[tokio::test(flavor = "multi_thread")]
async fn companion_files_are_recorded_with_the_note() {
    let tmp = tempfile::tempdir().unwrap();
    let handle = relay(tmp.path()).await;
    let base = format!("http://{}", handle.addr);
    let vault = handle.vault_id.to_string();
    let dir = tmp.path().join("notes");
    let write = |rel: &str, text: &str| {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    };
    let recorded = |text: &'static str| {
        let url = format!(
            "{base}/api/v1/vaults/{vault}/attachments/{}",
            lemmate_core::attachments::hash_bytes(text.as_bytes())
        );
        async move { get(url).await.0 == 200 }
    };

    write("talks/custom.scss", "/*-- scss:defaults --*/\n@import 'vars';\n");
    write("talks/_vars.scss", "$marker: #fedcba;\n");
    write("_quarto.yml", "format:\n  html:\n    css: shared/site.css\n");
    write("shared/site.css", ".site { color: #123456; }\n");
    write("talks/unused.scss", "/* nobody imports me */\n");
    let (code, note) = call(
        "POST",
        format!("{base}/api/v1/vaults/{vault}/notes"),
        Some(serde_json::json!({
            "path": "talks/deck.qmd",
            "content": "---\ntitle: Deck\nformat:\n  html:\n    theme: [cosmo, custom.scss]\n---\nBody.\n",
        })),
    )
    .await;
    assert_eq!(code, 201, "{note}");

    until("the theme to be recorded", async || recorded("/*-- scss:defaults --*/\n@import 'vars';\n").await)
        .await;
    until("the partial it imports", async || recorded("$marker: #fedcba;\n").await).await;
    until("the vault's _quarto.yml", async || recorded("format:\n  html:\n    css: shared/site.css\n").await)
        .await;
    until("the stylesheet _quarto.yml names", async || recorded(".site { color: #123456; }\n").await).await;
    assert!(!recorded("/* nobody imports me */\n").await, "a stylesheet nothing uses stays out");

    // The theme grows an import; the note is untouched.
    write("talks/_more.scss", "$more: 1;\n");
    write("talks/custom.scss", "/*-- scss:defaults --*/\n@import 'vars', 'more';\n");
    until("the new partial", async || recorded("$more: 1;\n").await).await;

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

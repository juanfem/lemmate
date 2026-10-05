//! The relay and its engines against a real server, where things go wrong: paths another
//! replica names that must not be written, a path two devices both took while one was offline,
//! a remote rename racing a remote edit, a folder re-joined with a fresh sidecar, and a merge of
//! a vault the server already holds. Each of these once lost or doubled someone's text, or
//! stopped a vault from syncing at all.

use std::net::SocketAddr;
use std::path::Path;
use std::time::{Duration, Instant};

use futures_util::SinkExt;
use lemmate_core::Store;
use lemmate_core::client::{LocalHandle, LocalOptions, SyncOptions, start_many};
use lemmate_core::sync::{Frame, Message, SyncMessage};
use lemmate_core::{DocId, NoteDoc, NoteId, VaultDoc, VaultId};
use lemmate_server::{ServerOptions, build_state, router};
use serde_json::Value;

#[path = "../../core/tests/common/mod.rs"]
mod common;
use common::{keyed, remember};

async fn server(attachments: &Path) -> SocketAddr {
    let opts = ServerOptions { attachments_dir: attachments.to_path_buf(), ..Default::default() };
    let state = build_state(Store::open_in_memory().unwrap(), opts);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router(state);
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    addr
}

fn sync_opts(dir: &Path, url: &str, vault_id: Option<VaultId>) -> SyncOptions {
    SyncOptions {
        vault_dir: dir.to_path_buf(),
        server_url: Some(url.to_owned()),
        vault_id,
        once: false,
        ca_cert: None,
        token: None,
    }
}

async fn relay_of(opts: Vec<SyncOptions>, root: Option<&Path>) -> LocalHandle {
    let local = LocalOptions {
        bind: "127.0.0.1:0".parse().unwrap(),
        web_dir: None,
        vault_root: root.map(Path::to_path_buf),
        config_path: None,
        allow_remote: false,
    };
    remember(start_many(opts, local).await.unwrap())
}

async fn relay(root: &Path, dirs: &[&str], server: SocketAddr) -> LocalHandle {
    let url = format!("http://{server}");
    relay_of(dirs.iter().map(|d| sync_opts(&root.join(d), &url, None)).collect(), Some(root)).await
}

async fn send(method: &'static str, url: String, body: Option<Value>) -> (u16, Value) {
    tokio::task::spawn_blocking(move || {
        let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).build().into();
        let mut r = match (method, body) {
            ("GET", _) => keyed(agent.get(&url), &url).call().unwrap(),
            ("PUT", Some(b)) => keyed(agent.put(&url), &url)
                .header("content-type", "application/json")
                .send(b.to_string().as_bytes())
                .unwrap(),
            ("PATCH", Some(b)) => keyed(agent.patch(&url), &url)
                .header("content-type", "application/json")
                .send(b.to_string().as_bytes())
                .unwrap(),
            ("POST", Some(b)) => keyed(agent.post(&url), &url)
                .header("content-type", "application/json")
                .send(b.to_string().as_bytes())
                .unwrap(),
            _ => unreachable!(),
        };
        let text = r.body_mut().read_to_string().unwrap_or_default();
        (r.status().as_u16(), serde_json::from_str(&text).unwrap_or(Value::String(text)))
    })
    .await
    .unwrap()
}

async fn get(url: String) -> (u16, Value) {
    send("GET", url, None).await
}

async fn json(url: String, body: Value) -> Value {
    let (code, v) = send("POST", url, Some(body)).await;
    assert!((200..300).contains(&code), "{code}: {v}");
    v
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

fn read(path: impl AsRef<Path>) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

/// A vault the server already holds, merged into another: the destination adopts each note with
/// its history, so the text is not merged into itself — on the server or on disk.
#[tokio::test(flavor = "multi_thread")]
async fn merging_a_synced_vault_does_not_double_its_text() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let server_addr = server(&root.join("blobs")).await;
    let remote = format!("http://{server_addr}");
    let handle = relay(&root, &["Loose", "Work"], server_addr).await;
    let base = format!("http://{}", handle.addr);
    let (from, into) = (handle.vaults[0].to_string(), handle.vaults[1].to_string());
    let note = json(
        format!("{base}/api/v1/vaults/{from}/notes"),
        serde_json::json!({ "path": "Plan.md", "content": "# Plan\n\nunique body line\n" }),
    )
    .await;
    let id = note["id"].as_str().unwrap().to_owned();
    until("the server to have the note", async || {
        get(format!("{remote}/api/v1/vaults/{from}/notes/{id}")).await.1["content"]
            .as_str()
            .is_some_and(|c| c.contains("unique body line"))
    })
    .await;
    let done = json(
        format!("{base}/api/v1/local/merge"),
        serde_json::json!({ "from": from, "into": into, "folder": "Loose" }),
    )
    .await;
    assert_eq!(done["applied"], true, "{done}");
    until("the note to be in the destination on the server", async || {
        get(format!("{remote}/api/v1/vaults/{into}/notes/{id}")).await.0 == 200
    })
    .await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    let content = get(format!("{remote}/api/v1/vaults/{into}/notes/{id}")).await.1["content"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    assert_eq!(content.matches("unique body line").count(), 1, "server: {content}");
    let local = read(root.join("Work/Loose/Plan.md"));
    assert_eq!(local.matches("unique body line").count(), 1, "disk: {local}");
    handle.abort();
}

/// A folder whose sidecar was lost, joined again: its files name ids the server has, with their
/// history, and are not inserted into it a second time.
#[tokio::test(flavor = "multi_thread")]
async fn rejoining_a_folder_with_a_fresh_sidecar_does_not_double_its_text() {
    let tmp = tempfile::tempdir().unwrap();
    let server_addr = server(&tmp.path().join("blobs")).await;
    let remote = format!("http://{server_addr}");
    let dir = tmp.path().join("A");
    let handle = relay_of(vec![sync_opts(&dir, &remote, None)], None).await;
    let v = handle.vault_id;
    let base = format!("http://{}", handle.addr);
    let note = json(
        format!("{base}/api/v1/vaults/{v}/notes"),
        serde_json::json!({ "path": "Plan.md", "content": "# Plan\n\nunique body line\n" }),
    )
    .await;
    let id = note["id"].as_str().unwrap().to_owned();
    until("the server to have the note", async || {
        get(format!("{remote}/api/v1/vaults/{v}/notes/{id}")).await.1["content"]
            .as_str()
            .is_some_and(|c| c.contains("unique body line"))
    })
    .await;
    handle.abort();
    tokio::time::sleep(Duration::from_millis(300)).await;

    std::fs::remove_dir_all(dir.join(".lemmate")).unwrap();
    let handle = relay_of(vec![sync_opts(&dir, &remote, Some(v))], None).await;
    let base = format!("http://{}", handle.addr);
    until("the relay to have the note's text again", async || {
        get(format!("{base}/api/v1/vaults/{v}/notes/{id}")).await.1["content"]
            .as_str()
            .is_some_and(|c| c.contains("unique body line"))
    })
    .await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    let content = get(format!("{remote}/api/v1/vaults/{v}/notes/{id}")).await.1["content"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    assert_eq!(content.matches("unique body line").count(), 1, "server: {content}");
    let local = read(dir.join("Plan.md"));
    assert_eq!(local.matches("unique body line").count(), 1, "disk: {local}");
    handle.abort();
}

/// An edit made on the server, and a rename right behind it: the file lands at the new path
/// with the edit, and the watcher seeing that move does not undo it.
#[tokio::test(flavor = "multi_thread")]
async fn a_remote_edit_then_a_quick_rename_keeps_the_edit() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let server_addr = server(&root.join("blobs")).await;
    let remote = format!("http://{server_addr}");
    let handle = relay(&root, &["A"], server_addr).await;
    let base = format!("http://{}", handle.addr);
    let v = handle.vaults[0].to_string();
    let note = json(
        format!("{base}/api/v1/vaults/{v}/notes"),
        serde_json::json!({ "path": "T.md", "content": "# T\n\nbody\n" }),
    )
    .await;
    let id = note["id"].as_str().unwrap().to_owned();
    let on_server = format!("{remote}/api/v1/vaults/{v}/notes/{id}");
    until("the server to have the note", async || get(on_server.clone()).await.0 == 200).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    let (c, t) =
        send("PUT", on_server.clone(), Some(serde_json::json!({"content": "# T\n\nbody EDITED REMOTELY\n"})))
            .await;
    assert!(c < 300, "{c} {t}");
    let (c, t) = send("PATCH", on_server.clone(), Some(serde_json::json!({"path": "moved.md"}))).await;
    assert!(c < 300, "{c} {t}");
    until("the move to reach the disk", async || read(root.join("A/moved.md")).contains("EDITED REMOTELY"))
        .await;
    // Long enough for the watcher's events about that move to be processed.
    tokio::time::sleep(Duration::from_secs(3)).await;
    let content = get(on_server).await.1["content"].as_str().unwrap_or_default().to_owned();
    assert!(content.contains("EDITED REMOTELY"), "server lost the edit: {content}");
    assert!(read(root.join("A/moved.md")).contains("EDITED REMOTELY"), "disk lost the edit");
    assert!(!root.join("A/T.md").exists());
    handle.abort();
}

/// Paths a hostile peer names in the vault doc — a note in the sidecar, a file in `.git`, an
/// attachment over a note — write nothing; and the engine keeps syncing the rest.
#[tokio::test(flavor = "multi_thread")]
async fn hostile_vault_paths_write_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let server_addr = server(&root.join("blobs")).await;
    let remote = format!("http://{server_addr}");
    let handle = relay(&root, &["A"], server_addr).await;
    let base = format!("http://{}", handle.addr);
    let v: VaultId = handle.vaults[0];
    json(
        format!("{base}/api/v1/vaults/{v}/notes"),
        serde_json::json!({ "path": "T.md", "content": "# T\n" }),
    )
    .await;
    until("the server to have the vault", async || {
        get(format!("{remote}/api/v1/vaults/{v}/notes")).await.1.as_array().is_some_and(|n| n.len() == 1)
    })
    .await;
    let bytes = b"[core]\n\tfsmonitor = touch /tmp/pwned\n".to_vec();
    let hash = lemmate_core::attachments::hash_bytes(&bytes);
    tokio::task::spawn_blocking({
        let url = format!("{remote}/api/v1/vaults/{v}/attachments/{hash}");
        move || ureq::put(&url).header("x-filename", "config").send(&bytes[..]).unwrap()
    })
    .await
    .unwrap();
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{server_addr}/ws")).await.unwrap();
    let evil = NoteId::new();
    let text = NoteDoc::new().set_text("pwned note text\n");
    let vd = VaultDoc::new();
    let frames = [
        (DocId::Note(evil), text),
        (DocId::Vault(v), vd.set_path(evil, ".lemmate/evil.md")),
        (DocId::Vault(v), vd.set_attachment(".git/config", &hash)),
        (DocId::Vault(v), vd.set_attachment("T.md", &hash)),
    ];
    for (doc, update) in frames {
        let f = Frame::new(doc.to_string(), &Message::Sync(SyncMessage::Update(update))).encode();
        ws.send(tokio_tungstenite::tungstenite::Message::Binary(f.into())).await.unwrap();
    }
    // A note made afterwards still arrives: the engine is alive.
    json(
        format!("{remote}/api/v1/vaults/{v}/notes"),
        serde_json::json!({ "path": "later.md", "content": "# later\n" }),
    )
    .await;
    until("a later note to be written", async || root.join("A/later.md").is_file()).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    let a = root.join("A");
    assert!(!a.join(".lemmate/evil.md").exists(), "a note was written into the sidecar");
    assert!(!a.join(".git").exists(), "a file was written into .git");
    assert!(read(a.join("T.md")).contains("# T"), "an attachment overwrote a note");
    handle.abort();
}

/// A note at a path this disk cannot hold — below a file — does not stop the vault: notes made
/// after it are written.
#[tokio::test(flavor = "multi_thread")]
async fn an_unwritable_note_does_not_stop_the_engine() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let server_addr = server(&root.join("blobs")).await;
    let remote = format!("http://{server_addr}");
    let handle = relay(&root, &["A"], server_addr).await;
    let base = format!("http://{}", handle.addr);
    let v = handle.vaults[0].to_string();
    json(
        format!("{base}/api/v1/vaults/{v}/notes"),
        serde_json::json!({ "path": "a.md", "content": "# a\n" }),
    )
    .await;
    until("the server to have the note", async || {
        get(format!("{remote}/api/v1/vaults/{v}/notes")).await.1.as_array().is_some_and(|n| n.len() == 1)
    })
    .await;
    json(
        format!("{remote}/api/v1/vaults/{v}/notes"),
        serde_json::json!({ "path": "a.md/b.md", "content": "# b\n" }),
    )
    .await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    json(
        format!("{remote}/api/v1/vaults/{v}/notes"),
        serde_json::json!({ "path": "later.md", "content": "# later\n" }),
    )
    .await;
    until("a later note to be written", async || read(root.join("A/later.md")).contains("# later")).await;
    assert_eq!(get(format!("{base}/api/v1/vaults/{v}/notes")).await.0, 200, "the relay still answers");
    handle.abort();
}

/// Offline, this device made `D.md`; meanwhile another device made `D.md` too. On reconnect the
/// lower id keeps the path and the other note moves aside — on every replica — instead of the
/// sidecar refusing the second note at that path forever.
#[tokio::test(flavor = "multi_thread")]
async fn the_same_path_made_on_two_devices_offline_is_resolved() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("A");
    let server_addr = server(&tmp.path().join("blobs")).await;
    let remote = format!("http://{server_addr}");
    let h = relay_of(vec![sync_opts(&dir, &remote, None)], None).await;
    let v = h.vault_id;
    json(
        format!("http://{}/api/v1/vaults/{v}/notes", h.addr),
        serde_json::json!({ "path": "seed.md", "content": "# seed\n" }),
    )
    .await;
    until("the server to have the seed", async || {
        get(format!("{remote}/api/v1/vaults/{v}/notes")).await.1.as_array().is_some_and(|n| n.len() == 1)
    })
    .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    h.abort();
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Another device makes D.md first (so with the lower id)...
    json(
        format!("{remote}/api/v1/vaults/{v}/notes"),
        serde_json::json!({ "path": "D.md", "content": "# from the server\n" }),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    // ...and this one, offline, makes D.md too.
    let dead = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let dead_url = format!("http://{}", dead.local_addr().unwrap());
    drop(dead);
    let h = relay_of(vec![sync_opts(&dir, &dead_url, None)], None).await;
    json(
        format!("http://{}/api/v1/vaults/{v}/notes", h.addr),
        serde_json::json!({ "path": "D.md", "content": "# from this device\n" }),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    h.abort();
    tokio::time::sleep(Duration::from_millis(300)).await;

    let h = relay_of(vec![sync_opts(&dir, &remote, None)], None).await;
    let paths = async || -> Vec<String> {
        let (_, notes) = get(format!("{remote}/api/v1/vaults/{v}/notes")).await;
        let mut p: Vec<String> = notes
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|n| n["path"].as_str().map(str::to_owned))
            .collect();
        p.sort();
        p
    };
    until("both notes on the server, apart", async || paths().await == ["D (2).md", "D.md", "seed.md"]).await;
    until("both files on disk", async || {
        read(dir.join("D.md")).contains("# from the server")
            && read(dir.join("D (2).md")).contains("# from this device")
    })
    .await;
    // And the vault keeps syncing: a note made now still arrives.
    json(
        format!("{remote}/api/v1/vaults/{v}/notes"),
        serde_json::json!({ "path": "after.md", "content": "# after\n" }),
    )
    .await;
    until("a later note to be written", async || dir.join("after.md").is_file()).await;
    h.abort();
}

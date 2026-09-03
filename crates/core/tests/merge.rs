//! Folding one vault into another (SPEC §3.2), with no server in sight.
//!
//! The merge is a local operation between two engines behind one relay: the source's files are
//! written into the destination's folder, adopted there by the id in their front matter, and the
//! source is retired. What matters is that a note comes out the other side as *the same note* —
//! same id, same text, same image — in a new place.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use lemmate_core::attachments::hash_bytes;
use lemmate_core::client::{LocalHandle, LocalOptions, SyncOptions, start_many};
use serde_json::Value;

async fn relay(root: &Path, dirs: &[&str]) -> LocalHandle {
    let opts = dirs
        .iter()
        .map(|d| SyncOptions {
            vault_dir: root.join(d),
            server_url: None,
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

/// Keeps the body on failures, where the reason is.
async fn post(url: String, body: Value) -> (u16, String) {
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

async fn json(url: String, body: Value) -> Value {
    let (code, text) = post(url, body).await;
    assert!((200..300).contains(&code), "{code}: {text}");
    serde_json::from_str(&text).unwrap_or(Value::Null)
}

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
async fn one_vault_folds_into_another_and_the_notes_keep_their_identity() {
    let tmp = tempfile::tempdir().unwrap();
    let root: PathBuf = tmp.path().to_path_buf();
    let handle = relay(&root, &["Loose", "Work"]).await;
    let base = format!("http://{}", handle.addr);
    let (from, into) = (handle.vaults[0].to_string(), handle.vaults[1].to_string());

    // The source: two notes, one of them with an image.
    let bytes = b"a diagram, honestly".to_vec();
    let hash = hash_bytes(&bytes);
    tokio::task::spawn_blocking({
        let (url, bytes) = (format!("{base}/api/v1/vaults/{from}/attachments/{hash}"), bytes.clone());
        move || ureq::put(&url).header("x-filename", "diagram.png").send(&bytes[..]).unwrap()
    })
    .await
    .unwrap();
    let plan_note = json(
        format!("{base}/api/v1/vaults/{from}/notes"),
        serde_json::json!({ "path": "Plan.md", "content": "# Plan\n\n![d](attachments/diagram.png)\n" }),
    )
    .await;
    let plan_id = plan_note["id"].as_str().unwrap().to_owned();
    json(
        format!("{base}/api/v1/vaults/{from}/notes"),
        serde_json::json!({ "path": "Daily/2026-09-03.md", "content": "# Today\n\n#soon\n" }),
    )
    .await;
    // The destination already has a note by the same name, at the root we are merging into.
    json(
        format!("{base}/api/v1/vaults/{into}/notes"),
        serde_json::json!({ "path": "Plan.md", "content": "# Work plan\n" }),
    )
    .await;

    // ---- The dry run changes nothing and says what would happen.
    let dry = json(
        format!("{base}/api/v1/local/merge"),
        serde_json::json!({ "from": from, "into": into, "folder": "", "dry_run": true }),
    )
    .await;
    assert_eq!(dry["applied"], false);
    let targets: Vec<&str> =
        dry["plan"]["notes"].as_array().unwrap().iter().map(|n| n["to"].as_str().unwrap()).collect();
    assert!(targets.contains(&"Plan-2.md"), "the collision is resolved in the plan: {targets:?}");
    assert!(targets.contains(&"Daily/2026-09-03.md"));
    let (_, still_there) = get(format!("{base}/api/v1/vaults")).await;
    assert_eq!(still_there.as_array().unwrap().len(), 2, "a dry run merges nothing");

    // ---- The merge itself.
    let done = json(
        format!("{base}/api/v1/local/merge"),
        serde_json::json!({ "from": from, "into": into, "folder": "" }),
    )
    .await;
    assert_eq!(done["applied"], true);
    assert_eq!(done["folder_removed"], true, "the emptied folder is gone: {done}");
    assert_eq!(done["left"].as_array().unwrap().len(), 0);

    // The destination holds them, and the moved note is the *same* note.
    until("the destination to list four notes", async || {
        let (_, notes) = get(format!("{base}/api/v1/vaults/{into}/notes")).await;
        notes.as_array().is_some_and(|n| n.len() == 3)
    })
    .await;
    let (_, notes) = get(format!("{base}/api/v1/vaults/{into}/notes")).await;
    let moved = notes
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["path"] == "Plan-2.md")
        .expect("the renamed note is in the destination");
    assert_eq!(moved["id"], plan_id, "identity survives the move; nothing was re-created");
    let (_, body) = get(format!("{base}/api/v1/vaults/{into}/notes/{plan_id}")).await;
    assert!(body["content"].as_str().unwrap().contains("![d](attachments/diagram.png)"));

    // The image came too, under the same name (the destination had none), and the destination
    // serves it by hash — which means its own vault doc records it.
    assert!(root.join("Work/attachments/diagram.png").is_file());
    until("the destination to serve the attachment", async || {
        get(format!("{base}/api/v1/vaults/{into}/attachments/{hash}")).await.0 == 200
    })
    .await;

    // And the source is gone: from the relay, and from the disk.
    let (_, vaults) = get(format!("{base}/api/v1/vaults")).await;
    let ids: Vec<&str> = vaults.as_array().unwrap().iter().map(|v| v["id"].as_str().unwrap()).collect();
    assert_eq!(ids, vec![into.as_str()], "only the destination is left");
    assert!(!root.join("Loose").exists(), "the source folder was removed");
    assert!(root.join("Work/Plan.md").is_file(), "the destination's own note is untouched");
    assert!(root.join("Work/Daily/2026-09-03.md").is_file());

    handle.abort();
}

/// A merge into a folder — the default when the dialog offers one — and the refusals.
#[tokio::test(flavor = "multi_thread")]
async fn merging_into_a_folder_and_the_things_it_refuses() {
    let tmp = tempfile::tempdir().unwrap();
    let root: PathBuf = tmp.path().to_path_buf();
    let handle = relay(&root, &["Loose", "Work"]).await;
    let base = format!("http://{}", handle.addr);
    let (from, into) = (handle.vaults[0].to_string(), handle.vaults[1].to_string());

    json(
        format!("{base}/api/v1/vaults/{from}/notes"),
        serde_json::json!({ "path": "Plan.md", "content": "# Plan\n" }),
    )
    .await;

    let (code, body) =
        post(format!("{base}/api/v1/local/merge"), serde_json::json!({ "from": from, "into": from })).await;
    assert_eq!(code, 400, "a vault cannot swallow itself: {body}");
    let (code, _) = post(
        format!("{base}/api/v1/local/merge"),
        serde_json::json!({ "from": from, "into": "01ARZ3NDEKTSV4RRFFQ69G5FAV" }),
    )
    .await;
    assert_eq!(code, 404, "a vault this relay does not hold");

    let done = json(
        format!("{base}/api/v1/local/merge"),
        serde_json::json!({ "from": from, "into": into, "folder": "Loose notes" }),
    )
    .await;
    assert_eq!(done["plan"]["notes"][0]["to"], "Loose notes/Plan.md");
    until("the note to arrive in its folder", async || root.join("Work/Loose notes/Plan.md").is_file()).await;

    handle.abort();
}

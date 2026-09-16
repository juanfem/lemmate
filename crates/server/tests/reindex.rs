//! A store indexed by an older indexer is re-derived when the server starts
//! (`reindex_if_stale`), without dating every note as just edited.

use lemmate_core::markdown::NoteIndex;
use lemmate_core::{NoteId, Store, VaultId};
use lemmate_server::app::reindex_if_stale;
use lemmate_server::{ServerOptions, build_state, router};
use serde_json::{Value, json};

#[tokio::test]
async fn stale_notes_are_reindexed_without_moving_their_stamp() {
    let state = build_state(Store::open_in_memory().unwrap(), ServerOptions::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(axum::serve(listener, router(state.clone())).into_future());

    let vault = VaultId::new();
    let body = json!({ "path": "Target.md", "content": "# Target\n" }).to_string();
    let target = post(format!("http://{addr}/api/v1/vaults/{vault}/notes"), body).await;
    let content = "# Tables\n\n| note | tag |\n|---|---|\n| [[Target\\|the target]] | #in-table |\n";
    let body = json!({ "path": "Tables.md", "content": content }).to_string();
    let tables: NoteId = post(format!("http://{addr}/api/v1/vaults/{vault}/notes"), body).await["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();

    let mut store = state.store.lock().await;
    let target = store.note_by_id(target["id"].as_str().unwrap().parse().unwrap()).unwrap().unwrap();
    // What an older indexer left behind: nothing read out of the table, and no version recorded.
    store.reindex_note(tables, &NoteIndex::default()).unwrap();
    store.meta_clear("index_version").unwrap();
    assert!(store.tags_in_vault(vault).unwrap().is_empty());
    let stamps =
        |s: &Store| s.list_notes(vault).unwrap().into_iter().map(|r| r.updated_at).collect::<Vec<_>>();
    let before = stamps(&store);

    std::thread::sleep(std::time::Duration::from_millis(2));
    assert_eq!(reindex_if_stale(&mut store).unwrap(), Some(2));
    assert_eq!(store.tags_in_vault(vault).unwrap(), vec![("in-table".to_owned(), 1)]);
    let backlinks: Vec<NoteId> = store.backlinks_to(&target).unwrap().into_iter().map(|r| r.id).collect();
    assert_eq!(backlinks, vec![tables]);
    assert_eq!(stamps(&store), before, "re-deriving unchanged text is not an edit");

    // Once current, a restart does nothing.
    assert_eq!(reindex_if_stale(&mut store).unwrap(), None);
}

async fn post(url: String, body: String) -> Value {
    tokio::task::spawn_blocking(move || {
        let mut r = ureq::post(url).header("content-type", "application/json").send(body.as_bytes()).unwrap();
        serde_json::from_str(&r.body_mut().read_to_string().unwrap()).unwrap()
    })
    .await
    .unwrap()
}

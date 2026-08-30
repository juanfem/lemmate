//! The REST client and the MCP dispatcher against a real `lemmate-server`, in-process.
//!
//! The server runs on its own runtime thread because `Remote` is blocking (`ureq`), which is
//! exactly how the CLI and `lemmate mcp` use it.

use std::net::SocketAddr;

use lemmate_cli::mcp;
use lemmate_cli::remote::{NotesApi, Remote, resolve_vault};
use lemmate_core::{Store, VaultId};
use lemmate_server::{ServerOptions, build_state, router};
use serde_json::{Value, json};

/// A server with authentication off (`ServerOptions::default()`), on a port of its own.
fn start_server() -> (String, tempfile::TempDir) {
    let blobs = tempfile::tempdir().unwrap();
    let options = ServerOptions { attachments_dir: blobs.path().to_path_buf(), ..ServerOptions::default() };
    let (tx, rx) = std::sync::mpsc::channel::<SocketAddr>();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        rt.block_on(async move {
            let state = build_state(Store::open_in_memory().unwrap(), options);
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            tx.send(listener.local_addr().unwrap()).unwrap();
            axum::serve(listener, router(state)).await.unwrap();
        });
    });
    (format!("http://{}", rx.recv().unwrap()), blobs)
}

/// Note body without the `id:` front matter the server adds (SPEC §6.3).
fn body(text: &str) -> String {
    match lemmate_core::frontmatter::block(text) {
        Some((_, end)) => text[end..].to_owned(),
        None => text.to_owned(),
    }
}

#[test]
fn remote_client_drives_a_vault_end_to_end() {
    let (server, _blobs) = start_server();
    let remote = Remote::from_args(&server, Some("ignored-when-auth-is-off".into()), None).unwrap();
    assert_eq!(remote.base(), server);

    // A vault comes into being with its first note (there is no "create vault" call).
    let vault = VaultId::new().to_string();
    assert!(remote.vaults().unwrap().is_empty(), "the store starts empty");
    let plan = remote.create(&vault, "Projects/plan", "# Plan\n\nstep one\n").unwrap();
    assert_eq!(plan.path, "Projects/plan.md");
    assert_eq!(body(&plan.content), "# Plan\n\nstep one\n");

    // vaults / ls / cat
    let vaults = remote.vaults().unwrap();
    assert_eq!(vaults.len(), 1);
    assert_eq!(vaults[0].id, vault);
    assert_eq!(resolve_vault(&remote, None).unwrap(), vault, "one vault needs no --vault");
    assert!(resolve_vault(&remote, Some("not-a-ulid")).is_err());
    let listed = remote.notes(&vault).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].path, "Projects/plan.md");
    assert_eq!(body(&remote.note(&vault, &plan.id).unwrap().content), "# Plan\n\nstep one\n");

    // resolve_note takes a ULID, a path, a path without `.md`, or a bare file name.
    for key in [plan.id.as_str(), "Projects/plan.md", "Projects/plan", "plan"] {
        assert_eq!(remote.resolve_note(&vault, key).unwrap().id, plan.id, "resolving {key}");
    }
    assert!(remote.lookup_note(&vault, "Projects/nope.md").unwrap().is_none());
    assert!(remote.lookup_note(&vault, &lemmate_core::NoteId::new().to_string()).unwrap().is_none());
    assert!(remote.resolve_note(&vault, "Projects/nope.md").is_err());

    // replace (diff-merged by the server) and rename
    let edited = remote.replace(&vault, &plan.id, "# Plan\n\nstep one\nstep two #project\n").unwrap();
    assert_eq!(body(&edited.content), "# Plan\n\nstep one\nstep two #project\n");
    remote.rename(&vault, &plan.id, "Projects/roadmap").unwrap();
    assert_eq!(remote.note(&vault, &plan.id).unwrap().path, "Projects/roadmap.md");

    // search over the derived index
    let hits = remote.search(&vault, "step", 10).unwrap();
    assert!(hits.iter().any(|h| h.note_id == plan.id), "expected a hit for `step`: {hits:?}");
    assert!(remote.search(&vault, "zzzznothing", 10).unwrap().is_empty());

    // tags
    let tags = remote.tags(&vault).unwrap();
    assert!(tags.iter().any(|t| t.tag == "project"), "{tags:?}");
    let tagged = remote.tagged(&vault, "project").unwrap();
    assert_eq!(tagged.len(), 1);
    assert_eq!(tagged[0].id, plan.id);

    // backlinks: a second note wikilinks the first by its new name
    let inbox = remote.create(&vault, "Inbox/today.md", "See [[roadmap]] for the plan.\n").unwrap();
    let links = remote.backlinks(&vault, &plan.id).unwrap();
    assert_eq!(links.len(), 1, "{links:?}");
    assert_eq!(links[0].id, inbox.id);

    // daily: get-or-create, and idempotent
    let daily = remote.daily(&vault, "2026-01-02").unwrap();
    assert_eq!(daily.path, "Daily/2026-01-02.md");
    assert!(body(&daily.content).starts_with("# 2026-01-02"));
    assert_eq!(remote.daily(&vault, "2026-01-02").unwrap().id, daily.id);

    // delete moves the note out of the listing
    remote.delete(&vault, &inbox.id).unwrap();
    let paths: Vec<String> = remote.notes(&vault).unwrap().into_iter().map(|n| n.path).collect();
    assert!(!paths.contains(&"Inbox/today.md".to_owned()), "{paths:?}");
    assert!(paths.contains(&"Projects/roadmap.md".to_owned()), "{paths:?}");
}

/// `tools/call` for `write_note`, `read_note`, and `search_notes` against the same server.
#[test]
fn mcp_tools_work_against_a_real_server() {
    let (server, _blobs) = start_server();
    let remote = Remote::from_args(&server, None, None).unwrap();
    let vault = VaultId::new().to_string();
    remote.create(&vault, "seed.md", "# Seed\n").unwrap();
    let session = mcp::Server::new(&remote, vault.clone());

    let call = |name: &str, arguments: Value| -> Value {
        let msg = json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
                         "params":{"name":name,"arguments":arguments}});
        session.dispatch(&msg).expect("tools/call is a request")
    };
    let text = |v: &Value| v["result"]["content"][0]["text"].as_str().unwrap().to_owned();

    // initialize first, the way a client would.
    let init = session
        .dispatch(&json!({"jsonrpc":"2.0","id":0,"method":"initialize",
                          "params":{"protocolVersion":"2025-06-18"}}))
        .unwrap();
    assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(session.dispatch(&json!({"jsonrpc":"2.0","method":"notifications/initialized"})), None);

    // write_note creates a missing path…
    let r = call(
        "write_note",
        json!({"path_or_id": "Meetings/standup", "content": "# Standup\n\npair on sync\n"}),
    );
    assert_eq!(r["result"]["isError"], false, "{r}");
    assert!(text(&r).contains("Meetings/standup.md"), "{}", text(&r));

    // …read_note reads it back…
    let r = call("read_note", json!({"path_or_id": "Meetings/standup.md"}));
    assert_eq!(r["result"]["isError"], false, "{r}");
    assert_eq!(body(&text(&r)), "# Standup\n\npair on sync\n");

    // …and write_note replaces it in place (same note id, new content).
    let before = remote.resolve_note(&vault, "Meetings/standup.md").unwrap();
    let r =
        call("write_note", json!({"path_or_id": "Meetings/standup.md", "content": "# Standup\n\nshipped\n"}));
    assert_eq!(r["result"]["isError"], false, "{r}");
    assert_eq!(remote.resolve_note(&vault, "Meetings/standup.md").unwrap().id, before.id);
    assert_eq!(body(&text(&call("read_note", json!({"path_or_id": before.id})))), "# Standup\n\nshipped\n");

    // search_notes finds it and names its path.
    let r = call("search_notes", json!({"query": "shipped"}));
    assert_eq!(r["result"]["isError"], false, "{r}");
    let found = text(&r);
    assert!(found.contains("Meetings/standup.md"), "{found}");

    // A tool failure is a result with isError, not a JSON-RPC error.
    let r = call("read_note", json!({"path_or_id": "no/such/note.md"}));
    assert_eq!(r["result"]["isError"], true, "{r}");

    // resources/list and resources/read expose the same notes.
    let r = session.dispatch(&json!({"jsonrpc":"2.0","id":2,"method":"resources/list"})).unwrap();
    let resources = r["result"]["resources"].as_array().unwrap();
    assert_eq!(resources.len(), 2, "{resources:?}");
    assert!(resources.iter().all(|x| x["mimeType"] == "text/markdown"));
    let uri = resources
        .iter()
        .find(|x| x["description"] == "Meetings/standup.md")
        .map(|x| x["uri"].clone())
        .expect("standup resource");
    assert_eq!(uri, json!(format!("note://{vault}/Meetings/standup.md")));
    let r = session
        .dispatch(&json!({"jsonrpc":"2.0","id":3,"method":"resources/read","params":{"uri":uri}}))
        .unwrap();
    assert_eq!(body(r["result"]["contents"][0]["text"].as_str().unwrap()), "# Standup\n\nshipped\n");
}

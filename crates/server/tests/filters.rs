//! Approved Lua filters (SPEC §5.6): who may approve one, what an approval names, and what a
//! render then runs. The rendering half runs only where quarto is installed and a render can
//! be confined, like every render test.

use std::collections::HashMap;
use std::net::SocketAddr;

use lemmate_core::attachments::hash_bytes;
use lemmate_core::store::Role;
use lemmate_core::{NoteId, Store, VaultId};
use lemmate_server::{AppState, AuthMode, ServerOptions, build_state, router};
use serde_json::{Value, json};

async fn start(filters_enabled: bool) -> (SocketAddr, std::sync::Arc<AppState>) {
    let options = ServerOptions {
        auth: AuthMode::Enabled { allow_registration: false, secure_cookies: false },
        attachments_dir: tempfile::tempdir().unwrap().keep(),
        filters_enabled,
        ..ServerOptions::default()
    };
    let state = build_state(Store::open_in_memory().unwrap(), options);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router(state.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (addr, state)
}

/// Make the account behind `token` the owner of `vault`, as syncing it first would.
async fn own(addr: SocketAddr, state: &AppState, vault: VaultId, token: &str) {
    let me = call("GET", addr, "/api/v1/auth/me", None, Some(token)).await.json();
    state.store.lock().await.set_membership(vault, me["id"].as_str().unwrap(), Role::Owner).unwrap();
}

struct Reply {
    status: u16,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

impl Reply {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
    /// The render's `x-lemmate-filters` report.
    fn filters(&self) -> Value {
        self.headers.get("x-lemmate-filters").map_or(Value::Null, |h| serde_json::from_str(h).unwrap())
    }
}

async fn call(
    method: &str,
    addr: SocketAddr,
    path: &str,
    body: Option<Vec<u8>>,
    token: Option<&str>,
) -> Reply {
    let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).build().into();
    let (url, method, token) = (format!("http://{addr}{path}"), method.to_owned(), token.map(str::to_owned));
    tokio::task::spawn_blocking(move || {
        let mut req = ureq::http::Request::builder().method(method.as_str()).uri(&url);
        if let Some(t) = &token {
            req = req.header("authorization", format!("Bearer {t}"));
        }
        if body.is_some() {
            req = req.header("content-type", "application/json");
        }
        let mut r = agent.run(req.body(body.unwrap_or_default()).unwrap()).unwrap();
        let headers = r
            .headers()
            .iter()
            .map(|(k, v)| (k.as_str().to_owned(), v.to_str().unwrap_or_default().to_owned()))
            .collect();
        let body = r.body_mut().with_config().limit(u64::MAX).read_to_vec().unwrap_or_default();
        Reply { status: r.status().as_u16(), headers, body }
    })
    .await
    .unwrap()
}

async fn send(method: &str, addr: SocketAddr, path: &str, body: Value, token: &str) -> Reply {
    call(method, addr, path, Some(body.to_string().into_bytes()), Some(token)).await
}

/// Ann (who registers first, and so is the admin) and Bob, each signed in.
async fn ann_and_bob(addr: SocketAddr) -> (String, String) {
    let r = call(
        "POST",
        addr,
        "/api/v1/auth/register",
        Some(json!({"email": "ann@example.org", "password": "first pass"}).to_string().into_bytes()),
        None,
    )
    .await;
    let ann = r.json()["token"].as_str().unwrap().to_owned();
    let r = send(
        "POST",
        addr,
        "/api/v1/auth/register",
        json!({"email": "bob@example.org", "password": "bobs pass"}),
        &ann,
    )
    .await;
    assert_eq!(r.status, 200);
    let r = call(
        "POST",
        addr,
        "/api/v1/auth/login",
        Some(json!({"email": "bob@example.org", "password": "bobs pass"}).to_string().into_bytes()),
        None,
    )
    .await;
    (ann, r.json()["token"].as_str().unwrap().to_owned())
}

/// Write a vault file; `over` is what it held before, when it is being replaced.
async fn put_file(
    addr: SocketAddr,
    vault: VaultId,
    path: &str,
    bytes: &str,
    over: Option<&str>,
    token: &str,
) {
    let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).build().into();
    let url = format!("http://{addr}/api/v1/vaults/{vault}/files?path={path}");
    let (bytes, token) = (bytes.as_bytes().to_vec(), token.to_owned());
    let base = over.map(|o| hash_bytes(o.as_bytes()));
    let status = tokio::task::spawn_blocking(move || {
        let mut req = agent.put(&url).header("authorization", format!("Bearer {token}"));
        if let Some(base) = &base {
            req = req.header("x-replace", "true").header("x-base-hash", base);
        }
        req.send(&bytes[..]).unwrap().status().as_u16()
    })
    .await
    .unwrap();
    assert!(status == 201 || status == 200, "{status}");
}

async fn create_note(addr: SocketAddr, vault: VaultId, path: &str, content: &str, token: &str) -> NoteId {
    let r = send(
        "POST",
        addr,
        &format!("/api/v1/vaults/{vault}/notes"),
        json!({"path": path, "content": content}),
        token,
    )
    .await;
    assert_eq!(r.status, 201, "{}", String::from_utf8_lossy(&r.body));
    r.json()["id"].as_str().unwrap().parse().unwrap()
}

async fn render(addr: SocketAddr, vault: VaultId, note: NoteId, token: &str) -> Reply {
    send(
        "POST",
        addr,
        &format!("/api/v1/vaults/{vault}/notes/{note}/render"),
        json!({"format": "html"}),
        token,
    )
    .await
}

fn renders_here() -> bool {
    if !lemmate_core::quarto::quarto_available(None) {
        eprintln!("skipped the renders: no quarto");
        return false;
    }
    if let Err(why) = lemmate_core::sandbox::confinement() {
        eprintln!("skipped the renders: {why}");
        return false;
    }
    true
}

const SHORTCODE: &str = "return { stamp = function() return pandoc.Str('STAMPED-BY-LUA') end }\n";
const NOTE: &str = "---\ntitle: Deck\nshortcodes: [lua/stamp.lua]\n---\n\n{{< stamp >}}\n";

#[tokio::test]
async fn an_owner_in_a_session_approves_these_bytes_for_this_vault() {
    let (addr, state) = start(true).await;
    let (ann, bob) = ann_and_bob(addr).await;
    let (a, b) = (VaultId::new(), VaultId::new());
    own(addr, &state, a, &ann).await;
    own(addr, &state, b, &ann).await;
    let note = create_note(addr, a, "Deck.qmd", NOTE, &ann).await;
    put_file(addr, a, "lua/stamp.lua", SHORTCODE, None, &ann).await;
    let hash = hash_bytes(SHORTCODE.as_bytes());
    let approvals = format!("/api/v1/vaults/{a}/filter-approvals");
    let r = send(
        "PUT",
        addr,
        &format!("/api/v1/vaults/{a}/members"),
        json!({"email": "bob@example.org", "role": "editor"}),
        &ann,
    )
    .await;
    assert_eq!(r.status, 204);
    let r = send("POST", addr, "/api/v1/tokens", json!({"name": "agent"}), &ann).await;
    let ann_token = r.json()["token"].as_str().unwrap().to_owned();
    let wants = json!({"path": "lua/stamp.lua", "hash": hash});

    if renders_here() {
        let r = render(addr, a, note, &ann).await;
        assert_eq!(r.status, 200, "{}", String::from_utf8_lossy(&r.body));
        assert!(!String::from_utf8_lossy(&r.body).contains("STAMPED-BY-LUA"), "not approved: not run");
        assert_eq!(r.filters()["unapproved"], json!([{"path": "lua/stamp.lua", "hash": hash}]));
    }

    // Anyone in the vault may see what is approved; only an owner, signed in, may approve.
    assert_eq!(call("GET", addr, &approvals, None, Some(&bob)).await.json(), json!([]));
    assert_eq!(send("POST", addr, &approvals, wants.clone(), &bob).await.status, 403, "an editor");
    assert_eq!(
        send("POST", addr, &approvals, wants.clone(), &ann_token).await.status,
        403,
        "an owner's token"
    );
    let stale = json!({"path": "lua/stamp.lua", "hash": hash_bytes(b"what ann read yesterday")});
    assert_eq!(send("POST", addr, &approvals, stale, &ann).await.status, 409, "not the bytes reviewed");
    let elsewhere = json!({"path": "lua/none.lua", "hash": hash});
    assert_eq!(send("POST", addr, &approvals, elsewhere, &ann).await.status, 404);
    let not_lua = json!({"path": "Deck.qmd", "hash": hash});
    assert_eq!(send("POST", addr, &approvals, not_lua, &ann).await.status, 400);
    let r = send("POST", addr, &approvals, wants.clone(), &ann).await;
    assert_eq!(r.status, 200, "{}", String::from_utf8_lossy(&r.body));
    assert_eq!(r.json()["approved_by"], "ann", "by display name");
    let listed = call("GET", addr, &approvals, None, Some(&ann_token)).await.json();
    assert_eq!(listed[0]["hash"], json!(hash), "a token may read the list");
    // Ann's desktop app holds a full-access token, which may approve as she would.
    let r = send("POST", addr, "/api/v1/tokens", json!({"name": "desktop", "full_access": true}), &ann).await;
    let app_token = r.json()["token"].as_str().unwrap().to_owned();
    assert_eq!(
        send("POST", addr, &approvals, wants.clone(), &app_token).await.status,
        200,
        "the owner's app"
    );

    // The same bytes in another vault are not approved there.
    let other = create_note(addr, b, "Deck.qmd", NOTE, &ann).await;
    put_file(addr, b, "lua/stamp.lua", SHORTCODE, None, &ann).await;
    assert_eq!(
        call("GET", addr, &format!("/api/v1/vaults/{b}/filter-approvals"), None, Some(&ann)).await.json(),
        json!([])
    );

    if renders_here() {
        let r = render(addr, a, note, &bob).await;
        assert!(String::from_utf8_lossy(&r.body).contains("STAMPED-BY-LUA"), "approved: run, for any reader");
        assert_eq!(r.filters()["ran"], json!(["lua/stamp.lua"]));
        let r = render(addr, b, other, &ann).await;
        assert!(!String::from_utf8_lossy(&r.body).contains("STAMPED-BY-LUA"), "vault B approved nothing");

        // A byte changed is another file, unapproved.
        let changed = SHORTCODE.replace("STAMPED", "CHANGED");
        put_file(addr, a, "lua/stamp.lua", &changed, Some(SHORTCODE), &bob).await;
        let r = render(addr, a, note, &ann).await;
        let page = String::from_utf8_lossy(&r.body);
        assert!(!page.contains("STAMPED") && !page.contains("CHANGED-BY-LUA"));
        put_file(addr, a, "lua/stamp.lua", SHORTCODE, Some(&changed), &bob).await;
    }

    // Revoked, by the owner in a session; it does not run again.
    let revoke = format!("{approvals}/{hash}");
    assert_eq!(call("DELETE", addr, &revoke, None, Some(&bob)).await.status, 403);
    assert_eq!(call("DELETE", addr, &revoke, None, Some(&ann_token)).await.status, 403);
    assert_eq!(call("DELETE", addr, &revoke, None, Some(&ann)).await.status, 204);
    assert_eq!(call("DELETE", addr, &revoke, None, Some(&ann)).await.status, 404);
    if renders_here() {
        let r = render(addr, a, note, &ann).await;
        assert!(!String::from_utf8_lossy(&r.body).contains("STAMPED-BY-LUA"));
    }
}

#[tokio::test]
async fn a_server_with_filters_switched_off_runs_none() {
    let (addr, state) = start(false).await;
    let (ann, _) = ann_and_bob(addr).await;
    let a = VaultId::new();
    own(addr, &state, a, &ann).await;
    let note = create_note(addr, a, "Deck.qmd", NOTE, &ann).await;
    put_file(addr, a, "lua/stamp.lua", SHORTCODE, None, &ann).await;
    let wants = json!({"path": "lua/stamp.lua", "hash": hash_bytes(SHORTCODE.as_bytes())});
    assert_eq!(
        send("POST", addr, &format!("/api/v1/vaults/{a}/filter-approvals"), wants, &ann).await.status,
        200
    );
    if renders_here() {
        let r = render(addr, a, note, &ann).await;
        assert!(!String::from_utf8_lossy(&r.body).contains("STAMPED-BY-LUA"));
        let report = r.filters();
        assert_eq!(report["unapproved"], json!([]), "nothing to ask for");
        assert_eq!(report["withheld"], "Lua filters are switched off here");
    }
}

#[path = "../../core/tests/common/mod.rs"]
mod common;

/// A desktop's relay (SPEC §5.6): the server's approvals run there too, and still offline; one
/// made on the device — which signs in with an access token, and so cannot approve upstream —
/// stays the device's; a server approval is not the device's to revoke.
#[tokio::test(flavor = "multi_thread")]
async fn a_relay_runs_the_servers_approvals_and_its_own() {
    let options = ServerOptions {
        auth: AuthMode::Enabled { allow_registration: false, secure_cookies: false },
        attachments_dir: tempfile::tempdir().unwrap().keep(),
        ..ServerOptions::default()
    };
    let state = build_state(Store::open_in_memory().unwrap(), options);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router(state.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let (ann, _) = ann_and_bob(addr).await;
    let token = send("POST", addr, "/api/v1/tokens", json!({"name": "desktop"}), &ann).await.json()["token"]
        .as_str()
        .unwrap()
        .to_owned();

    // Ann's vault, which a limited token can sync but could not have created.
    let vault = VaultId::new();
    own(addr, &state, vault, &ann).await;
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("vault");
    std::fs::create_dir_all(dir.join("lua")).unwrap();
    let other = "return { other = function() return pandoc.Str('OTHER-BY-LUA') end }\n";
    std::fs::write(dir.join("lua/stamp.lua"), SHORTCODE).unwrap();
    std::fs::write(dir.join("lua/other.lua"), other).unwrap();
    std::fs::write(
        dir.join("Deck.qmd"),
        "---\ntitle: Deck\nshortcodes: [lua/stamp.lua, lua/other.lua]\n---\n\n{{< stamp >}} {{< other >}}\n",
    )
    .unwrap();
    let opts = lemmate_core::client::SyncOptions {
        vault_dir: dir.clone(),
        server_url: Some(format!("http://{addr}")),
        vault_id: Some(vault),
        once: false,
        ca_cert: None,
        token: Some(token),
    };
    let local = lemmate_core::client::LocalOptions {
        bind: "127.0.0.1:0".parse().unwrap(),
        web_dir: None,
        vault_root: None,
        config_path: None,
        allow_remote: false,
        approvals_dir: Some(tmp.path().join("approvals")),
    };
    let handle = common::remember(lemmate_core::client::start(opts, local).await.unwrap());
    assert_eq!(handle.vault_id, vault);
    let base = format!("http://{}/api/v1/vaults/{vault}", handle.addr);
    let relay = |method: &'static str, url: String, body: Option<Value>| async move {
        tokio::task::spawn_blocking(move || {
            let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).build().into();
            let mut r = match body {
                Some(b) => common::keyed(agent.post(&url), &url)
                    .header("content-type", "application/json")
                    .send(b.to_string().as_bytes())
                    .unwrap(),
                None if method == "DELETE" => common::keyed(agent.delete(&url), &url).call().unwrap(),
                None => common::keyed(agent.get(&url), &url).call().unwrap(),
            };
            let filters =
                r.headers().get("x-lemmate-filters").and_then(|h| h.to_str().ok()).map(str::to_owned);
            let body = r.body_mut().read_to_string().unwrap_or_default();
            (r.status().as_u16(), body, filters)
        })
        .await
        .unwrap()
    };

    // Wait for the note and both files to reach the server.
    let note = loop {
        let notes = call("GET", addr, &format!("/api/v1/vaults/{vault}/notes"), None, Some(&ann)).await;
        let files = call("GET", addr, &format!("/api/v1/vaults/{vault}/files"), None, Some(&ann)).await;
        if notes.status == 200
            && files.json().as_array().is_some_and(|f| f.len() == 2)
            && let Some(id) = notes.json()[0]["id"].as_str()
        {
            break id.to_owned();
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    };
    let (stamp, other_hash) = (hash_bytes(SHORTCODE.as_bytes()), hash_bytes(other.as_bytes()));

    // Ann approves one on the server, in her browser session.
    let r = send(
        "POST",
        addr,
        &format!("/api/v1/vaults/{vault}/filter-approvals"),
        json!({"path": "lua/stamp.lua", "hash": stamp}),
        &ann,
    )
    .await;
    assert_eq!(r.status, 200);
    // The desktop approves the other: its token is a limited one, which cannot approve
    // upstream, so it is the device's.
    let (code, body, _) = relay(
        "POST",
        format!("{base}/filter-approvals"),
        Some(json!({"path": "lua/other.lua", "hash": other_hash})),
    )
    .await;
    assert_eq!(code, 200, "{body}");
    assert_eq!(serde_json::from_str::<Value>(&body).unwrap()["source"], "device");
    let (_, listed, _) = relay("GET", format!("{base}/filter-approvals"), None).await;
    let listed: Value = serde_json::from_str(&listed).unwrap();
    assert_eq!(listed.as_array().unwrap().len(), 2);
    assert_eq!(listed[0]["source"], "server");
    assert_eq!(listed[0]["hash"], json!(stamp));
    assert_eq!(listed[1]["source"], "device");
    assert!(tmp.path().join("approvals").join(format!("{vault}.json")).is_file());
    let device_only =
        call("GET", addr, &format!("/api/v1/vaults/{vault}/filter-approvals"), None, Some(&ann)).await;
    assert_eq!(device_only.json().as_array().unwrap().len(), 1, "the server has only its own");

    let render = format!("{base}/notes/{note}/render");
    if renders_here() {
        let (code, page, filters) = relay("POST", render.clone(), Some(json!({"format": "html"}))).await;
        assert_eq!(code, 200, "{page}");
        assert!(page.contains("STAMPED-BY-LUA") && page.contains("OTHER-BY-LUA"), "both run here");
        let filters: Value = serde_json::from_str(&filters.unwrap()).unwrap();
        assert_eq!(filters["ran"], json!(["lua/stamp.lua", "lua/other.lua"]));
    }

    // A server approval is the server's: a token cannot revoke it from here.
    let (code, _, _) = relay("DELETE", format!("{base}/filter-approvals/{stamp}"), None).await;
    assert_eq!(code, 403);

    // Offline: what was approved still runs.
    server.abort();
    let _ = server.await;
    if renders_here() {
        let (code, page, _) = relay("POST", render.clone(), Some(json!({"format": "html"}))).await;
        assert_eq!(code, 200, "{page}");
        assert!(page.contains("STAMPED-BY-LUA") && page.contains("OTHER-BY-LUA"), "offline, from the copy");
    }
    let (code, _, _) = relay("DELETE", format!("{base}/filter-approvals/{other_hash}"), None).await;
    assert_eq!(code, 204, "the device's own goes at once");
    if renders_here() {
        let (_, page, filters) = relay("POST", render, Some(json!({"format": "html"}))).await;
        assert!(page.contains("STAMPED-BY-LUA") && !page.contains("OTHER-BY-LUA"));
        let filters: Value = serde_json::from_str(&filters.unwrap()).unwrap();
        assert_eq!(filters["unapproved"][0]["path"], "lua/other.lua");
    }
    handle.abort();
}

//! Who may use the local relay (SPEC §3.2): loopback is not a boundary, so every request needs
//! the relay's per-launch key, names the relay by a loopback Host, and comes from the relay's
//! own origin when it comes from a page at all.

use std::path::Path;
use std::time::{Duration, Instant};

use lemmate_core::client::{LocalHandle, LocalOptions, SyncOptions, start};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn options(root: &Path) -> SyncOptions {
    SyncOptions {
        vault_dir: root.join("notes"),
        server_url: None,
        vault_id: None,
        once: false,
        ca_cert: None,
        token: None,
    }
}

async fn relay(root: &Path) -> LocalHandle {
    let local = LocalOptions {
        bind: "127.0.0.1:0".parse().unwrap(),
        web_dir: None,
        vault_root: Some(root.to_path_buf()),
        config_path: None,
        allow_remote: false,
    };
    start(options(root), local).await.unwrap()
}

/// One HTTP/1.1 exchange, written by hand so the Host and Origin are exactly what a hostile
/// page or a rebound DNS name would send. Returns the status and the raw head + body.
async fn raw(
    handle: &LocalHandle,
    method: &str,
    target: &str,
    headers: &[(&str, String)],
    body: &[u8],
) -> (u16, String) {
    let mut stream = tokio::net::TcpStream::connect(handle.addr).await.unwrap();
    let mut req =
        format!("{method} {target} HTTP/1.1\r\nConnection: close\r\nContent-Length: {}\r\n", body.len());
    if !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("host")) {
        req.push_str(&format!("Host: {}\r\n", handle.addr));
    }
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("\r\n");
    stream.write_all(req.as_bytes()).await.unwrap();
    stream.write_all(body).await.unwrap();
    let mut out = Vec::new();
    stream.read_to_end(&mut out).await.unwrap();
    let text = String::from_utf8_lossy(&out).into_owned();
    let status = text.split(' ').nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    (status, text)
}

fn bearer(handle: &LocalHandle) -> (&'static str, String) {
    ("Authorization", format!("Bearer {}", handle.key))
}

fn header<'a>(response: &'a str, name: &str) -> Option<&'a str> {
    response
        .split("\r\n\r\n")
        .next()?
        .lines()
        .find_map(|l| l.split_once(':').filter(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.trim()))
}

#[tokio::test(flavor = "multi_thread")]
async fn every_request_needs_the_key_the_host_and_the_origin() {
    let tmp = tempfile::tempdir().unwrap();
    let handle = relay(tmp.path()).await;
    let port = handle.addr.port();

    // Without the key: nothing, not even the page. `/healthz` says only that it is alive.
    assert_eq!(raw(&handle, "GET", "/api/v1/vaults", &[], b"").await.0, 401);
    assert_eq!(raw(&handle, "GET", "/", &[], b"").await.0, 401);
    assert_eq!(raw(&handle, "GET", "/healthz", &[], b"").await.0, 200);
    let wrong = ("Authorization", format!("Bearer {}", "0".repeat(64)));
    assert_eq!(raw(&handle, "GET", "/api/v1/vaults", &[wrong], b"").await.0, 401);

    // A program: the key as a bearer token.
    assert_eq!(raw(&handle, "GET", "/api/v1/vaults", &[bearer(&handle)], b"").await.0, 200);

    // A page: opened once with `?key=`, which becomes a cookie and leaves the address.
    let (code, opened) = raw(&handle, "GET", &format!("/?x=1&key={}", handle.key), &[], b"").await;
    assert_eq!(code, 303, "{opened}");
    assert_eq!(header(&opened, "location"), Some("/?x=1"));
    let cookie = header(&opened, "set-cookie").unwrap().to_owned();
    assert!(cookie.starts_with(&format!("lemmate_relay_{port}={}", handle.key)), "{cookie}");
    assert!(cookie.contains("HttpOnly") && cookie.contains("SameSite=Lax") && cookie.contains("Path=/"));
    let jar = ("Cookie", format!("theme=dark; lemmate_relay_{port}={}", handle.key));
    assert_eq!(raw(&handle, "GET", "/api/v1/vaults", std::slice::from_ref(&jar), b"").await.0, 200);
    assert_eq!(raw(&handle, "GET", &format!("/?key={}", "f".repeat(64)), &[], b"").await.0, 401);
    // Another relay's cookie (another port) is not this one's.
    let elsewhere = ("Cookie", format!("lemmate_relay_{}={}", port.wrapping_add(1), handle.key));
    assert_eq!(raw(&handle, "GET", "/api/v1/vaults", &[elsewhere], b"").await.0, 401);

    // DNS rebinding: the right key is no use under somebody else's name.
    let rebound = ("Host", format!("attacker.example:{port}"));
    assert_eq!(raw(&handle, "GET", "/api/v1/vaults", &[rebound, bearer(&handle)], b"").await.0, 421);
    assert_eq!(
        raw(&handle, "GET", "/api/v1/vaults", &[("Host", format!("localhost:{port}")), bearer(&handle)], b"")
            .await
            .0,
        200
    );

    // Another site's page, even one that somehow had the cookie sent along.
    let foreign = ("Origin", "https://attacker.example".to_owned());
    assert_eq!(raw(&handle, "POST", "/api/v1/local/merge", &[foreign, jar.clone()], b"{}").await.0, 403);
    let null = ("Origin", "null".to_owned());
    assert_eq!(raw(&handle, "GET", "/api/v1/vaults", &[null, jar.clone()], b"").await.0, 403);
    let own = ("Origin", format!("http://{}", handle.addr));
    assert_eq!(raw(&handle, "GET", "/api/v1/vaults", &[own, jar], b"").await.0, 200);

    // The socket: refused without the key, open with it.
    let ws = format!("ws://{}/ws", handle.addr);
    assert!(tokio_tungstenite::connect_async(ws.clone()).await.is_err());
    assert!(tokio_tungstenite::connect_async(format!("{ws}?key={}", handle.key)).await.is_ok());
    handle.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_relay_reachable_from_the_network_has_to_be_asked_for() {
    let tmp = tempfile::tempdir().unwrap();
    let local = |allow_remote| LocalOptions {
        bind: "0.0.0.0:0".parse().unwrap(),
        web_dir: None,
        vault_root: None,
        config_path: None,
        allow_remote,
    };
    assert!(start(options(tmp.path()), local(false)).await.is_err());
    let handle = start(options(tmp.path()), local(true)).await.unwrap();
    // Any name it is reached by, but still the key.
    let named = ("Host", format!("my-desktop.lan:{}", handle.addr.port()));
    assert_eq!(raw(&handle, "GET", "/api/v1/vaults", std::slice::from_ref(&named), b"").await.0, 401);
    assert_eq!(raw(&handle, "GET", "/api/v1/vaults", &[named, bearer(&handle)], b"").await.0, 200);
    handle.abort();
}

/// Whatever a vault holds is served from the relay's own origin, where a script could use the
/// relay's cookie — so nothing is sniffed or run, and anything a browser would open as a
/// document of its own is a download.
#[tokio::test(flavor = "multi_thread")]
async fn attachments_are_served_inert() {
    let tmp = tempfile::tempdir().unwrap();
    let handle = relay(tmp.path()).await;
    let vault = handle.vault_id;
    let svg = b"<svg xmlns='http://www.w3.org/2000/svg'><script>alert(1)</script></svg>".to_vec();
    let png = b"\x89PNG\r\n\x1a\nnot really".to_vec();
    let mut paths = Vec::new();
    for (bytes, name) in [(&svg, "x.svg"), (&png, "x.png")] {
        let hash = lemmate_core::attachments::hash_bytes(bytes);
        let target = format!("/api/v1/vaults/{vault}/attachments/{hash}");
        let (code, body) =
            raw(&handle, "PUT", &target, &[bearer(&handle), ("x-filename", name.to_owned())], bytes).await;
        assert_eq!(code, 200, "{body}");
        let json: serde_json::Value = serde_json::from_str(body.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        paths.push((target, json["path"].as_str().unwrap().to_owned()));
    }
    let content = paths.iter().map(|(_, p)| format!("![]({p})\n")).collect::<String>();
    let note = serde_json::json!({ "path": "pics.md", "content": content }).to_string();
    let json = ("Content-Type", "application/json".to_owned());
    let (code, _) = raw(
        &handle,
        "POST",
        &format!("/api/v1/vaults/{vault}/notes"),
        &[bearer(&handle), json],
        note.as_bytes(),
    )
    .await;
    assert_eq!(code, 201);

    let deadline = Instant::now() + Duration::from_secs(10);
    let fetch = async |target: &str| loop {
        let (code, response) = raw(&handle, "GET", target, &[bearer(&handle)], b"").await;
        if code == 200 || Instant::now() > deadline {
            return response;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    let svg_response = fetch(&paths[0].0).await;
    assert_eq!(header(&svg_response, "x-content-type-options"), Some("nosniff"), "{svg_response}");
    assert_eq!(header(&svg_response, "content-security-policy"), Some("sandbox"));
    assert_eq!(header(&svg_response, "content-disposition"), Some("attachment"));
    assert!(header(&svg_response, "cache-control").is_some_and(|c| c.contains("private")));
    let png_response = fetch(&paths[1].0).await;
    assert_eq!(header(&png_response, "content-type"), Some("image/png"), "{png_response}");
    assert_eq!(header(&png_response, "content-disposition"), None, "an image may be shown in place");
    assert_eq!(header(&png_response, "content-security-policy"), Some("sandbox"));
    handle.abort();
}

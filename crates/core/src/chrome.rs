//! A rendered deck printed to PDF by headless Chrome (SPEC §5.6). The browser's print dialog
//! sizes the paper itself — on macOS it picks A4 whatever the deck asks — so a deck's PDF is made
//! here instead, on pages the size of its slides, from the same print layout the dialog would
//! have shown ([`crate::quarto::for_print`]).
//!
//! The deck is a note's author's HTML and JavaScript, and here it runs on the server, so Chrome is
//! driven over its DevTools protocol and holds every request it makes: the deck itself is served
//! from memory under a made-up address, with a Content-Security-Policy that lets it load nothing
//! that is not inline or `data:`; MathJax may come from the CDN Quarto names (the viewer's browser
//! loads it from there too); everything else — a file, a local service, any other site, a
//! navigation away — is refused. Names other than that CDN do not even resolve. There is no
//! listening port of ours, and Chrome gets a fresh profile each time.

use std::io::{BufRead, BufReader};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::{self, Message};

use crate::error::{Error, Result};

/// Where Chrome is asked to fetch the deck from; answered from memory, never looked up.
const DECK_URL: &str = "http://lemmate-print.invalid/deck.html";
/// The one host a deck may reach: Quarto's MathJax and KaTeX come from it.
const CDN: &str = "cdn.jsdelivr.net";
const CSP: &str = "default-src 'none'; \
    script-src 'unsafe-inline' 'unsafe-eval' data: blob: https://cdn.jsdelivr.net; \
    style-src 'unsafe-inline' data: blob: https://cdn.jsdelivr.net; \
    img-src data: blob: https://cdn.jsdelivr.net; font-src data: blob: https://cdn.jsdelivr.net; \
    media-src data: blob:; connect-src https://cdn.jsdelivr.net; \
    frame-src 'none'; worker-src 'none'; form-action 'none'; base-uri 'none'";

#[derive(Debug, Clone)]
pub struct PrintOptions {
    /// Chrome or chrome-headless-shell; `None` → [`chrome_bin`]'s search.
    pub chrome: Option<PathBuf>,
    /// For the whole run: starting Chrome, laying the deck out, printing it.
    pub timeout: Duration,
}

impl Default for PrintOptions {
    fn default() -> Self {
        Self { chrome: None, timeout: Duration::from_secs(90) }
    }
}

/// The Chrome to print with: `explicit`, else `$LEMMATE_CHROME`, else the
/// chrome-headless-shell `quarto install chrome-headless-shell` puts in Quarto's data directory,
/// else a Chrome or Chromium on `PATH` or in its usual place.
pub fn chrome_bin(explicit: Option<&Path>) -> Option<PathBuf> {
    if let Some(p) = explicit {
        return Some(p.to_path_buf());
    }
    if let Some(p) = std::env::var_os("LEMMATE_CHROME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(p));
    }
    let exe = |name: &str| if cfg!(windows) { format!("{name}.exe") } else { name.to_owned() };
    // Quarto's own install: <data dir>/quarto/chrome-headless-shell/chrome-headless-shell-<platform>/.
    let data_dirs = [
        std::env::var_os("XDG_DATA_HOME").filter(|v| !v.is_empty()).map(PathBuf::from),
        dirs::data_dir(),
        dirs::data_local_dir(),
    ];
    for dir in data_dirs.into_iter().flatten() {
        let root = dir.join("quarto").join("chrome-headless-shell");
        if let Ok(entries) = std::fs::read_dir(&root) {
            for e in entries.flatten() {
                let bin = e.path().join(exe("chrome-headless-shell"));
                if bin.is_file() {
                    return Some(bin);
                }
            }
        }
    }
    let names =
        ["chrome-headless-shell", "google-chrome-stable", "google-chrome", "chromium", "chromium-browser"];
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            for name in names {
                let bin = dir.join(exe(name));
                if bin.is_file() {
                    return Some(bin);
                }
            }
        }
    }
    let usual: &[&str] = if cfg!(target_os = "macos") {
        &[
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
            "/Applications/Chromium.app/Contents/MacOS/Chromium",
        ]
    } else if cfg!(windows) {
        &[
            r"C:\Program Files\Google\Chrome\Application\chrome.exe",
            r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
        ]
    } else {
        &[]
    };
    usual.iter().map(PathBuf::from).find(|p| p.is_file())
}

pub fn chrome_available(explicit: Option<&Path>) -> bool {
    chrome_bin(explicit).is_some_and(|p| p.is_file() || which(&p))
}

fn which(name: &Path) -> bool {
    name.components().count() == 1
        && std::env::var_os("PATH")
            .is_some_and(|path| std::env::split_paths(&path).any(|d| d.join(name).is_file()))
}

/// Chrome, killed with everything it started when this goes.
struct Browser(Child);

impl Drop for Browser {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Ok(pid) = libc::pid_t::try_from(self.0.id()) {
            // SAFETY: kill(2) on the process group this child leads; no memory involved.
            unsafe { libc::kill(-pid, libc::SIGKILL) };
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Print `page` — a render with [`crate::quarto::for_print`]'s script, which marks the document
/// `data-lemmate-printed` once it is laid out — to PDF, on the page size it asks for.
pub fn print_pdf(page: &[u8], opts: &PrintOptions) -> Result<Vec<u8>> {
    let bin = chrome_bin(opts.chrome.as_deref())
        .ok_or_else(|| Error::Export("no Chrome to print with (set LEMMATE_CHROME)".into()))?;
    let deadline = Instant::now() + opts.timeout;
    let profile = crate::pandoc::tempdir()?;
    let mut cmd = Command::new(&bin);
    cmd.args([
        "--headless",
        "--remote-debugging-port=0",
        "--no-first-run",
        "--no-default-browser-check",
        "--disable-gpu",
        "--disable-extensions",
        "--disable-sync",
        "--disable-background-networking",
        "--disable-component-update",
        "--mute-audio",
        "--hide-scrollbars",
        // Belt and braces under the request interception below: nothing but the CDN resolves,
        // and WebRTC does not get round it by UDP.
        "--host-resolver-rules=MAP * ~NOTFOUND, EXCLUDE cdn.jsdelivr.net",
        "--force-webrtc-ip-handling-policy=disable_non_proxied_udp",
        "--webrtc-ip-handling-policy=disable_non_proxied_udp",
    ])
    .arg(format!("--user-data-dir={}", profile.display()));
    // Chrome's own sandbox needs user namespaces, which a container often lacks; there the
    // operator says so (the Docker image does), and the container is the sandbox.
    if std::env::var("LEMMATE_CHROME_NO_SANDBOX").is_ok_and(|v| matches!(v.as_str(), "1" | "true" | "yes")) {
        cmd.arg("--no-sandbox");
    }
    cmd.arg("about:blank").stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped());
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
    let mut child = cmd.spawn().map_err(|e| Error::Export(format!("running {}: {e}", bin.display())))?;
    let stderr = child.stderr.take();
    let browser = Browser(child);

    // "DevTools listening on ws://127.0.0.1:PORT/devtools/browser/…", on stderr; what else it
    // says is kept for the error when it never does.
    let (tx, rx) = std::sync::mpsc::channel::<std::result::Result<String, String>>();
    if let Some(stderr) = stderr {
        std::thread::spawn(move || {
            let mut said = Vec::new();
            for line in BufReader::new(stderr).lines().map_while(std::io::Result::ok) {
                if let Some(at) = line.find("ws://") {
                    let _ = tx.send(Ok(line[at..].trim().to_owned()));
                    return;
                }
                said.push(line);
            }
            let tail = said.iter().rev().take(5).rev().cloned().collect::<Vec<_>>().join("\n");
            let _ = tx.send(Err(tail));
        });
    }
    let ws_url = match rx.recv_timeout(remaining(deadline)?) {
        Ok(Ok(url)) => url,
        Ok(Err(said)) => return Err(Error::Export(format!("Chrome did not start: {said}"))),
        Err(_) => return Err(Error::Export("Chrome did not start in time".into())),
    };
    let mut cdp = Cdp::connect(&ws_url, deadline)?;

    let target = cdp.call(None, "Target.createTarget", json!({ "url": "about:blank" }))?;
    let target = target["targetId"].as_str().unwrap_or_default().to_owned();
    let session = cdp.call(None, "Target.attachToTarget", json!({ "targetId": target, "flatten": true }))?;
    let session = session["sessionId"].as_str().unwrap_or_default().to_owned();
    cdp.page = Some(base64::engine::general_purpose::STANDARD.encode(page));
    cdp.call(Some(&session), "Fetch.enable", json!({ "patterns": [{ "urlPattern": "*" }] }))?;
    cdp.call(Some(&session), "Page.navigate", json!({ "url": format!("{DECK_URL}?print-pdf") }))?;
    loop {
        let ready = cdp.call(
            Some(&session),
            "Runtime.evaluate",
            json!({
                "expression": "document.documentElement.hasAttribute('data-lemmate-printed')",
                "returnByValue": true,
            }),
        )?;
        if ready["result"]["value"] == Value::Bool(true) {
            break;
        }
        cdp.pause(Duration::from_millis(150))?;
    }
    let pdf = cdp.call(
        Some(&session),
        "Page.printToPDF",
        json!({ "preferCSSPageSize": true, "printBackground": true }),
    )?;
    let data = pdf["data"].as_str().ok_or_else(|| Error::Export("Chrome returned no PDF".into()))?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data)
        .map_err(|e| Error::Export(format!("Chrome's PDF: {e}")))?;
    drop(browser);
    Ok(bytes)
}

fn remaining(deadline: Instant) -> Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|d| !d.is_zero())
        .ok_or_else(|| Error::Export("printing the deck took too long".into()))
}

/// A DevTools connection, answering the page's requests as they come while it waits on its own.
struct Cdp {
    ws: tungstenite::WebSocket<TcpStream>,
    next: u64,
    deadline: Instant,
    /// The deck, base64, once there is one to serve.
    page: Option<String>,
}

impl Cdp {
    fn connect(url: &str, deadline: Instant) -> Result<Self> {
        let host = url
            .strip_prefix("ws://")
            .and_then(|rest| rest.split('/').next())
            .ok_or_else(|| Error::Export(format!("Chrome's DevTools address: {url}")))?;
        let stream = TcpStream::connect(host).map_err(|e| Error::Export(format!("DevTools: {e}")))?;
        stream.set_read_timeout(Some(Duration::from_millis(500)))?;
        let config = tungstenite::protocol::WebSocketConfig::default()
            .max_message_size(Some(1 << 30))
            .max_frame_size(Some(1 << 30));
        let (ws, _) = tungstenite::client::client_with_config(url, stream, Some(config))
            .map_err(|e| Error::Export(format!("DevTools: {e}")))?;
        Ok(Self { ws, next: 0, deadline, page: None })
    }

    fn send(&mut self, session: Option<&str>, method: &str, params: Value) -> Result<u64> {
        self.next += 1;
        let mut msg = json!({ "id": self.next, "method": method, "params": params });
        if let Some(s) = session {
            msg["sessionId"] = s.into();
        }
        self.ws.send(Message::text(msg.to_string())).map_err(|e| Error::Export(format!("DevTools: {e}")))?;
        Ok(self.next)
    }

    /// Send a command and wait for its answer, handling events meanwhile.
    fn call(&mut self, session: Option<&str>, method: &str, params: Value) -> Result<Value> {
        let id = self.send(session, method, params)?;
        loop {
            let Some(msg) = self.read()? else { continue };
            if msg["id"].as_u64() == Some(id) {
                if let Some(err) = msg.get("error") {
                    return Err(Error::Export(format!("DevTools {method}: {err}")));
                }
                return Ok(msg["result"].clone());
            }
            self.event(&msg)?;
        }
    }

    /// Keep answering the page for a while.
    fn pause(&mut self, d: Duration) -> Result<()> {
        let until = Instant::now() + d;
        while Instant::now() < until {
            if let Some(msg) = self.read()? {
                self.event(&msg)?;
            }
        }
        Ok(())
    }

    /// One message, or `None` when none came within the socket's read timeout.
    fn read(&mut self) -> Result<Option<Value>> {
        remaining(self.deadline)?;
        match self.ws.read() {
            Ok(Message::Text(t)) => Ok(serde_json::from_str(t.as_str()).ok()),
            Ok(_) => Ok(None),
            Err(tungstenite::Error::Io(e))
                if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) =>
            {
                Ok(None)
            }
            Err(e) => Err(Error::Export(format!("DevTools: {e}"))),
        }
    }

    /// A request the page made, held until we say: the deck, the CDN, or nothing.
    fn event(&mut self, msg: &Value) -> Result<()> {
        if msg["method"] != "Fetch.requestPaused" {
            return Ok(());
        }
        let session = msg["sessionId"].as_str().map(str::to_owned);
        let params = &msg["params"];
        let id = params["requestId"].clone();
        let url = params["request"]["url"].as_str().unwrap_or_default();
        let (method, reply) = match decide(url) {
            Decision::Deck => (
                "Fetch.fulfillRequest",
                json!({
                    "requestId": id,
                    "responseCode": 200,
                    "responseHeaders": [
                        { "name": "Content-Type", "value": "text/html; charset=utf-8" },
                        { "name": "Content-Security-Policy", "value": CSP },
                    ],
                    "body": self.page.clone().unwrap_or_default(),
                }),
            ),
            Decision::Cdn => ("Fetch.continueRequest", json!({ "requestId": id })),
            Decision::Refuse => {
                ("Fetch.failRequest", json!({ "requestId": id, "errorReason": "BlockedByClient" }))
            }
        };
        self.send(session.as_deref(), method, reply)?;
        Ok(())
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Decision {
    Deck,
    Cdn,
    Refuse,
}

/// What a request from the deck gets: the deck itself, the CDN over https, or nothing.
fn decide(url: &str) -> Decision {
    let Ok(u) = url::Url::parse(url) else { return Decision::Refuse };
    let deck = url::Url::parse(DECK_URL).expect("a URL");
    if u.scheme() == deck.scheme() && u.host_str() == deck.host_str() && u.path() == deck.path() {
        return Decision::Deck;
    }
    if u.scheme() == "https" && u.host_str() == Some(CDN) && u.port().is_none() && u.username().is_empty() {
        return Decision::Cdn;
    }
    Decision::Refuse
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_deck_may_reach_itself_and_the_cdn_and_nothing_else() {
        assert_eq!(decide("http://lemmate-print.invalid/deck.html?print-pdf"), Decision::Deck);
        assert_eq!(decide("https://cdn.jsdelivr.net/npm/mathjax@2.7.9/MathJax.js"), Decision::Cdn);
        for url in [
            "http://lemmate-print.invalid/other.html",
            "https://lemmate-print.invalid/deck.html",
            "http://cdn.jsdelivr.net/npm/x.js",
            "https://cdn.jsdelivr.net:8443/x.js",
            "https://user@cdn.jsdelivr.net/x.js",
            "https://cdn.jsdelivr.net.evil.example/x.js",
            "file:///etc/passwd",
            "http://127.0.0.1:8080/api/v1/vaults",
            "http://169.254.169.254/latest/meta-data/",
            "not a url",
        ] {
            assert_eq!(decide(url), Decision::Refuse, "{url}");
        }
    }

    /// Runs only where a Chrome is found. A page that tries every way out — an image, a fetch, an
    /// iframe, a beacon, a stylesheet — to a service on this host's loopback reaches nothing, and
    /// still prints.
    #[test]
    fn a_page_reaches_nothing_but_itself() {
        if !chrome_available(None) {
            eprintln!("skipped: no Chrome");
            return;
        }
        let canary = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = canary.local_addr().unwrap().port();
        canary.set_nonblocking(true).unwrap();
        let at = format!("http://127.0.0.1:{port}");
        let page = format!(
            r#"<html><head><link rel="stylesheet" href="{at}/css"></head><body>
            <p>Printed page</p>
            <img src="{at}/img"><img src="file:///etc/hostname"><iframe src="{at}/frame"></iframe>
            <iframe src="file:///etc/hostname"></iframe>
            <script>
              fetch("{at}/fetch").catch(function () {{}})
              navigator.sendBeacon("{at}/beacon")
              var i = new Image(); i.src = "{at}/js-img"
              try {{ var x = new XMLHttpRequest(); x.open("GET", "{at}/xhr"); x.send() }} catch (e) {{}}
            </script></body></html>"#
        );
        let page = crate::quarto::for_print(page.into_bytes(), false);
        let opts = PrintOptions { timeout: Duration::from_secs(30), ..Default::default() };
        let pdf = print_pdf(&page, &opts).unwrap();
        assert!(pdf.starts_with(b"%PDF"));
        assert!(canary.accept().is_err(), "nothing reached the loopback service");
    }

    /// Runs only where a Chrome is found. A page that navigates away is refused the navigation —
    /// Chrome then shows its own error page, so nothing prints, and nothing was reached.
    #[test]
    fn a_page_cannot_navigate_away() {
        if !chrome_available(None) {
            eprintln!("skipped: no Chrome");
            return;
        }
        let canary = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = canary.local_addr().unwrap().port();
        canary.set_nonblocking(true).unwrap();
        let page = format!(
            r#"<html><body><script>location.href = "http://127.0.0.1:{port}/away"</script></body></html>"#
        );
        let opts = PrintOptions { timeout: Duration::from_secs(8), ..Default::default() };
        assert!(print_pdf(page.as_bytes(), &opts).is_err());
        assert!(canary.accept().is_err(), "the navigation reached nothing");
    }

    #[test]
    fn no_chrome_is_a_clear_error() {
        let opts = PrintOptions { chrome: Some(PathBuf::from("/nonexistent/chrome")), ..Default::default() };
        let err = print_pdf(b"<html></html>", &opts).unwrap_err().to_string();
        assert!(err.contains("running /nonexistent/chrome"), "{err}");
        assert!(!chrome_available(Some(Path::new("/nonexistent/chrome"))));
    }
}

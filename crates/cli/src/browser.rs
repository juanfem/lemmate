//! `lemmate login --browser`: sign in through the server's web page in the system browser
//! (`lemmate_core::credentials::BrowserSignIn`, the server's `apps.rs`). The answer comes back
//! as a redirect to a one-request HTTP listener on a loopback port, so the browser's existing
//! session with the identity provider is used and nothing is typed into the terminal.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use lemmate_core::credentials::BrowserSignIn;

/// How long to wait for the person to finish in the browser.
const WAIT: Duration = Duration::from_secs(5 * 60);
const CALLBACK_PATH: &str = "/lemmate-callback";

/// Run the whole sign-in; returns the account's email. The token is saved like any other.
pub fn login(server: &str, device: &str, ca_cert: Option<&Path>) -> anyhow::Result<String> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).context("opening a loopback port for the answer")?;
    let redirect = format!("http://127.0.0.1:{}{CALLBACK_PATH}", listener.local_addr()?.port());
    let flow = BrowserSignIn::start(server, &redirect, device)?;
    eprintln!("Opening your browser to sign in. If it does not open, visit:\n\n  {}\n", flow.url);
    open(&flow.url);
    let query = wait_for_callback(&listener, WAIT)?;
    Ok(flow.finish(&query, ca_cert)?)
}

/// Serve requests on `listener` until one arrives for [`CALLBACK_PATH`]; answer it with a page
/// saying the browser may be closed, and return its query. Anything else (a favicon) is a 404.
pub fn wait_for_callback(listener: &TcpListener, wait: Duration) -> anyhow::Result<String> {
    listener.set_nonblocking(true)?;
    let deadline = Instant::now() + wait;
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream.set_nonblocking(false)?;
                if let Some(query) = answer(stream)? {
                    return Ok(query);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if Instant::now() > deadline {
                    bail!("gave up waiting for the browser after {} minutes", wait.as_secs() / 60);
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => return Err(e.into()),
        }
    }
}

fn answer(mut stream: TcpStream) -> anyhow::Result<Option<String>> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut line = String::new();
    BufReader::new(&stream).read_line(&mut line)?;
    // "GET /lemmate-callback?code=…&state=… HTTP/1.1"
    let target = line.split_whitespace().nth(1).unwrap_or("");
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    if path != CALLBACK_PATH {
        stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")?;
        return Ok(None);
    }
    let cancelled = query.split('&').any(|p| p.starts_with("error="));
    let body = format!(
        "<!doctype html><meta charset=utf-8><title>Lemmate</title>\
         <body style=\"font-family:system-ui;display:grid;place-items:center;height:90vh\">\
         <p>{}</p>",
        if cancelled {
            "Sign-in cancelled. You can close this tab."
        } else {
            "Signed in to Lemmate. You can close this tab and go back to the terminal."
        }
    );
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )?;
    Ok(Some(query.to_owned()))
}

/// Hand a URL to the browser — `$BROWSER` if set, as other command-line tools honour it, else
/// the system's — and failing that, the printed link is the way.
fn open(url: &str) {
    if let Some(browser) = std::env::var_os("BROWSER").filter(|b| !b.is_empty()) {
        if let Err(e) = std::process::Command::new(&browser).arg(url).spawn() {
            eprintln!("(could not run $BROWSER: {e})");
        }
        return;
    }
    #[cfg(target_os = "macos")]
    let r = std::process::Command::new("open").arg(url).spawn();
    #[cfg(windows)]
    let r = std::process::Command::new("cmd").args(["/C", "start", "", url]).spawn();
    #[cfg(not(any(target_os = "macos", windows)))]
    let r = std::process::Command::new("xdg-open").arg(url).spawn();
    if let Err(e) = r {
        eprintln!("(could not open a browser: {e})");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn get(port: u16, path: &str) -> String {
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        write!(s, "GET {path} HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
        let mut out = String::new();
        std::io::Read::read_to_string(&mut s, &mut out).unwrap();
        out
    }

    #[test]
    fn the_callback_is_caught_and_other_requests_are_not() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let browser = std::thread::spawn(move || {
            let favicon = get(port, "/favicon.ico");
            let page = get(port, "/lemmate-callback?code=abc&state=xyz");
            (favicon, page)
        });
        let query = wait_for_callback(&listener, Duration::from_secs(10)).unwrap();
        assert_eq!(query, "code=abc&state=xyz");
        let (favicon, page) = browser.join().unwrap();
        assert!(favicon.starts_with("HTTP/1.1 404"));
        assert!(page.contains("Signed in"), "{page}");
    }

    #[test]
    fn nobody_coming_back_is_a_timeout() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        assert!(wait_for_callback(&listener, Duration::from_millis(200)).is_err());
    }
}

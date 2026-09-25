//! Saved sessions for native clients: `credentials.toml` in the per-user configuration directory
//! (`crate::paths::config_dir`), one `[servers."<base url>"]` table per server. Written by
//! `lemmate login`, read by `lemmate sync` and the desktop app.
//!
//! The token itself goes into the operating system's keychain when there is one (SPEC §11.1) —
//! macOS Keychain, Windows Credential Manager, the Secret Service (GNOME Keyring, KWallet) on
//! Linux — and the table then says only `keychain = true`. Where there is none (a headless box
//! with no D-Bus session, a build without the `keychain` feature, or `LEMMATE_KEYCHAIN=0`) the
//! table holds `token` instead, in a file only its owner can read. Either shape is read back, so
//! a file written before the keychain existed keeps working.

use std::path::PathBuf;

use crate::error::{Error, Result};

/// The keychain service name entries are filed under; the account is the server's [`key`].
pub const KEYCHAIN_SERVICE: &str = "lemmate";

/// Somewhere to keep a secret by name: the system keychain, or a stand-in in tests.
pub trait Secrets {
    fn set(&self, account: &str, secret: &str) -> std::result::Result<(), String>;
    fn get(&self, account: &str) -> std::result::Result<Option<String>, String>;
    fn delete(&self, account: &str) -> std::result::Result<(), String>;
}

/// The operating system's keychain, via the `keyring` crate.
#[cfg(feature = "keychain")]
pub struct Keychain {
    pub service: &'static str,
}

#[cfg(feature = "keychain")]
impl Secrets for Keychain {
    fn set(&self, account: &str, secret: &str) -> std::result::Result<(), String> {
        keyring::Entry::new(self.service, account)
            .and_then(|e| e.set_password(secret))
            .map_err(|e| e.to_string())
    }
    fn get(&self, account: &str) -> std::result::Result<Option<String>, String> {
        match keyring::Entry::new(self.service, account).and_then(|e| e.get_password()) {
            Ok(s) => Ok(Some(s)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }
    fn delete(&self, account: &str) -> std::result::Result<(), String> {
        match keyring::Entry::new(self.service, account).and_then(|e| e.delete_credential()) {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }
}

/// The keychain this process should use, if any: compiled in, and not turned off with
/// `LEMMATE_KEYCHAIN=0` (or `no`, `false`, `off`).
pub fn system_secrets() -> Option<&'static dyn Secrets> {
    let off = std::env::var("LEMMATE_KEYCHAIN")
        .is_ok_and(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "0" | "no" | "false" | "off"));
    if off {
        return None;
    }
    #[cfg(feature = "keychain")]
    {
        static KEYCHAIN: Keychain = Keychain { service: KEYCHAIN_SERVICE };
        Some(&KEYCHAIN)
    }
    #[cfg(not(feature = "keychain"))]
    None
}

pub fn path() -> PathBuf {
    crate::paths::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("credentials.toml")
}

fn read() -> toml::Table {
    std::fs::read_to_string(path()).ok().and_then(|s| s.parse::<toml::Table>().ok()).unwrap_or_default()
}

/// Normalised key: scheme + host, no trailing slash or `/ws`.
pub fn key(server: &str) -> String {
    let s = server.trim_end_matches('/');
    s.strip_suffix("/ws").unwrap_or(s).to_owned()
}

pub fn load(server: &str) -> Option<String> {
    load_with(system_secrets(), server)
}

pub fn load_with(secrets: Option<&dyn Secrets>, server: &str) -> Option<String> {
    let key = key(server);
    let root = read();
    let entry = root.get("servers")?.get(&key)?;
    if let Some(t) = entry.get("token").and_then(|v| v.as_str()) {
        return Some(t.to_owned());
    }
    if entry.get("keychain").and_then(|v| v.as_bool()) == Some(true) {
        let Some(secrets) = secrets else {
            tracing::warn!(server = %key, "the token for this server is in the keychain, which is turned off here");
            return None;
        };
        return match secrets.get(&key) {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(server = %key, %e, "could not read the token from the keychain");
                None
            }
        };
    }
    None
}

/// Where the saved token for `server` lives, for messages: the keychain or the file's path.
pub fn location(server: &str) -> String {
    let in_keychain = read().get("servers").and_then(|v| v.get(key(server))).and_then(|e| e.get("keychain"))
        == Some(&toml::Value::Boolean(true));
    if in_keychain { "the system keychain".to_owned() } else { path().display().to_string() }
}

pub fn save(server: &str, token: &str) -> Result<()> {
    save_with(system_secrets(), server, token)
}

/// Save a token: into `secrets` if it takes it, else into the file.
pub fn save_with(secrets: Option<&dyn Secrets>, server: &str, token: &str) -> Result<()> {
    let key = key(server);
    let mut root = read();
    let servers = root.entry("servers").or_insert_with(|| toml::Value::Table(toml::Table::new()));
    let Some(servers) = servers.as_table_mut() else {
        return Err(Error::Sync("credentials file is malformed".into()));
    };
    let mut entry = toml::Table::new();
    match secrets.map(|s| s.set(&key, token)) {
        Some(Ok(())) => {
            entry.insert("keychain".into(), toml::Value::Boolean(true));
        }
        other => {
            if let Some(Err(e)) = other {
                tracing::info!(server = %key, %e, "no keychain to hand; keeping the token in the credentials file");
            }
            entry.insert("token".into(), toml::Value::String(token.to_owned()));
        }
    }
    servers.insert(key, toml::Value::Table(entry));
    write(&root)
}

fn write(root: &toml::Table) -> Result<()> {
    let p = path();
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&p, toml::to_string(root).map_err(|e| Error::Sync(e.to_string()))?)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// Sign in (or register) on a server and save the session token. Returns the token.
///
/// `invite` is a registration invite (SPEC §11.1) and only means anything with `register`: it is
/// what gets an account created on a server where registration is otherwise closed.
pub fn login(
    server: &str,
    email: &str,
    password: &str,
    register: bool,
    invite: Option<&str>,
    ca_cert: Option<&std::path::Path>,
    device: &str,
) -> Result<String> {
    let agent = crate::tls::http_agent(ca_cert)?;
    let base = key(server);
    let path = if register { "/api/v1/auth/register" } else { "/api/v1/auth/login" };
    let mut body = serde_json::json!({ "email": email, "password": password, "device": device });
    if let Some(t) = invite.map(invite_token).filter(|t| !t.is_empty()) {
        body["invite"] = t.into();
    }
    let mut resp = agent
        .post(format!("{base}{path}"))
        .header("content-type", "application/json")
        .send(body.to_string().as_bytes())
        .map_err(|e| {
            Error::Sync(format!("{}: {e}", if register { "registration failed" } else { "login failed" }))
        })?;
    let text = resp.body_mut().read_to_string().map_err(|e| Error::Sync(e.to_string()))?;
    let json: serde_json::Value = serde_json::from_str(&text).map_err(|e| Error::Sync(e.to_string()))?;
    let token =
        json["token"].as_str().ok_or_else(|| Error::Sync("server returned no token".into()))?.to_owned();
    save(&base, &token)?;
    Ok(token)
}

/// Save a token made elsewhere — a personal access token from the web client's account dialog,
/// which is how a server with password sign-in turned off (OIDC only) is reached from here —
/// once the server has confirmed it. Returns the email of the account it belongs to.
pub fn login_with_token(server: &str, token: &str, ca_cert: Option<&std::path::Path>) -> Result<String> {
    let token = token.trim();
    if token.is_empty() {
        return Err(Error::Sync("the token is empty".into()));
    }
    let agent = crate::tls::http_agent(ca_cert)?;
    let base = key(server);
    let mut resp = agent
        .get(format!("{base}/api/v1/auth/me"))
        .header("authorization", &format!("Bearer {token}"))
        .call()
        .map_err(|e| Error::Sync(format!("the server did not accept the token: {e}")))?;
    let text = resp.body_mut().read_to_string().map_err(|e| Error::Sync(e.to_string()))?;
    let json: serde_json::Value = serde_json::from_str(&text).map_err(|e| Error::Sync(e.to_string()))?;
    let email =
        json["email"].as_str().ok_or_else(|| Error::Sync("the server named no account".into()))?.to_owned();
    save(&base, token)?;
    Ok(email)
}

/// A sign-in through the browser in progress (the server's `apps.rs`): open [`Self::url`] in a
/// browser or a webview, catch the redirect to `redirect_uri`, and hand its query to
/// [`BrowserSignIn::finish`]. The verifier never leaves this process; the browser only ever
/// sees its hash.
#[derive(Debug, Clone)]
pub struct BrowserSignIn {
    pub server: String,
    pub redirect_uri: String,
    pub url: String,
    state: String,
    verifier: String,
}

fn random_hex(bytes: usize) -> Result<String> {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).map_err(|e| Error::Sync(format!("no randomness: {e}")))?;
    Ok(buf.iter().map(|b| format!("{b:02x}")).collect())
}

impl BrowserSignIn {
    /// Start one. `redirect_uri` is where this program will catch the answer — `http://` on a
    /// loopback address; `device` is how the token will be named in the account's list.
    pub fn start(server: &str, redirect_uri: &str, device: &str) -> Result<Self> {
        use base64::Engine as _;
        use sha2::Digest as _;
        let server = key(server);
        let (state, verifier) = (random_hex(16)?, random_hex(32)?);
        let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(sha2::Sha256::digest(verifier.as_bytes()));
        let mut url = url::Url::parse(&format!("{server}/"))
            .map_err(|e| Error::Sync(format!("{server} is not a URL: {e}")))?;
        url.query_pairs_mut()
            .append_pair("authorize", "app")
            .append_pair("redirect_uri", redirect_uri)
            .append_pair("state", &state)
            .append_pair("code_challenge", &challenge)
            .append_pair("device", device);
        Ok(Self { server, redirect_uri: redirect_uri.to_owned(), url: url.into(), state, verifier })
    }

    /// Whether `url` is the redirect this sign-in is waiting for.
    pub fn is_callback(&self, url: &str) -> bool {
        let strip = |u: &str| u.split(['?', '#']).next().unwrap_or("").trim_end_matches('/').to_owned();
        strip(url) == strip(&self.redirect_uri)
    }

    /// Finish with the query of the redirect (`code=…&state=…`, or `error=…`): trade the code for
    /// a token, save it for the server, and return the account's email.
    pub fn finish(&self, query: &str, ca_cert: Option<&std::path::Path>) -> Result<String> {
        let pairs: std::collections::HashMap<String, String> =
            url::form_urlencoded::parse(query.trim_start_matches('?').as_bytes()).into_owned().collect();
        if let Some(e) = pairs.get("error") {
            return Err(Error::Sync(if e == "access_denied" {
                "sign-in was cancelled".into()
            } else {
                e.clone()
            }));
        }
        if pairs.get("state") != Some(&self.state) {
            return Err(Error::Sync("the answer does not belong to this sign-in".into()));
        }
        let code = pairs.get("code").ok_or_else(|| Error::Sync("the answer carries no code".into()))?;
        let agent = crate::tls::http_agent(ca_cert)?;
        let body = serde_json::json!({
            "code": code,
            "code_verifier": self.verifier,
            "redirect_uri": self.redirect_uri,
        });
        let mut resp = agent
            .post(format!("{}/api/v1/auth/app/token", self.server))
            .header("content-type", "application/json")
            .send(body.to_string().as_bytes())
            .map_err(|e| Error::Sync(format!("the server did not hand out a token: {e}")))?;
        let text = resp.body_mut().read_to_string().map_err(|e| Error::Sync(e.to_string()))?;
        let json: serde_json::Value = serde_json::from_str(&text).map_err(|e| Error::Sync(e.to_string()))?;
        let token = json["token"].as_str().ok_or_else(|| Error::Sync("the server sent no token".into()))?;
        save(&self.server, token)?;
        Ok(json["email"].as_str().unwrap_or_default().to_owned())
    }
}

/// This machine's name, for naming what it signs in as.
pub fn hostname() -> String {
    std::env::var("HOSTNAME")
        .ok()
        .or_else(|| std::env::var("COMPUTERNAME").ok())
        .or_else(|| std::fs::read_to_string("/etc/hostname").ok())
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "this computer".into())
}

/// The token out of an invite the admin sent, which may be the whole URL
/// (`https://notes.example.org/#/invite/<token>`) or just the token. Pasting the link is what
/// people actually do, so accept both rather than making them edit it.
pub fn invite_token(invite: &str) -> String {
    let s = invite.trim();
    s.rsplit_once("/#/invite/").map_or(s, |(_, t)| t).trim().to_owned()
}

pub fn forget(server: &str) -> Result<()> {
    forget_with(system_secrets(), server)
}

pub fn forget_with(secrets: Option<&dyn Secrets>, server: &str) -> Result<()> {
    let key = key(server);
    let mut root = read();
    let in_keychain = root
        .get("servers")
        .and_then(|v| v.get(&key))
        .and_then(|e| e.get("keychain"))
        .and_then(|v| v.as_bool())
        == Some(true);
    if in_keychain && let Some(Err(e)) = secrets.map(|s| s.delete(&key)) {
        tracing::warn!(server = %key, %e, "could not remove the token from the keychain");
    }
    if let Some(servers) = root.get_mut("servers").and_then(|v| v.as_table_mut()) {
        servers.remove(&key);
    }
    write(&root)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A keychain that remembers in memory, or refuses everything (a headless box).
    struct Mem(std::sync::Mutex<std::collections::HashMap<String, String>>, bool);
    impl Secrets for Mem {
        fn set(&self, a: &str, s: &str) -> std::result::Result<(), String> {
            if !self.1 {
                return Err("no secret service".into());
            }
            self.0.lock().unwrap().insert(a.into(), s.into());
            Ok(())
        }
        fn get(&self, a: &str) -> std::result::Result<Option<String>, String> {
            Ok(self.0.lock().unwrap().get(a).cloned())
        }
        fn delete(&self, a: &str) -> std::result::Result<(), String> {
            self.0.lock().unwrap().remove(a);
            Ok(())
        }
    }

    /// One test, because every case shares the one credentials file.
    #[test]
    fn round_trip_in_a_temp_home() {
        let dir = tempfile::tempdir().unwrap();
        // SAFETY: this is the only test in the crate that touches this variable, so nothing else
        // races with it. It is also what makes the test platform-independent: without it `path()`
        // would resolve to the real `~/.config`, `~/Library/Application Support` or `%APPDATA%`.
        unsafe { std::env::set_var("LEMMATE_CONFIG_DIR", dir.path()) };
        assert_eq!(path().parent(), Some(dir.path()));

        // No keychain: the token is in the file.
        assert_eq!(load_with(None, "https://x.example"), None);
        save_with(None, "https://x.example/", "tok1").unwrap();
        save_with(None, "https://y.example", "tok2").unwrap();
        assert_eq!(load_with(None, "https://x.example").as_deref(), Some("tok1"));
        assert_eq!(load_with(None, "https://x.example/ws").as_deref(), Some("tok1"));
        forget_with(None, "https://x.example").unwrap();
        assert_eq!(load_with(None, "https://x.example"), None);
        assert_eq!(load_with(None, "https://y.example").as_deref(), Some("tok2"));

        // A keychain that works: the file only says where the token is.
        let kc = Mem(Default::default(), true);
        save_with(Some(&kc), "https://k.example", "secret-k").unwrap();
        let text = std::fs::read_to_string(path()).unwrap();
        assert!(!text.contains("secret-k"), "{text}");
        assert!(text.contains("keychain = true"), "{text}");
        assert_eq!(load_with(Some(&kc), "https://k.example").as_deref(), Some("secret-k"));
        assert_eq!(load_with(None, "https://k.example"), None, "the keychain turned off afterwards");
        // …and a file token written before the keychain is still read with one.
        assert_eq!(load_with(Some(&kc), "https://y.example").as_deref(), Some("tok2"));
        forget_with(Some(&kc), "https://k.example").unwrap();
        assert!(kc.0.lock().unwrap().is_empty(), "forget empties the keychain too");

        // A keychain that refuses (no D-Bus session): the file, as before.
        let broken = Mem(Default::default(), false);
        save_with(Some(&broken), "https://z.example", "tok3").unwrap();
        assert_eq!(load_with(Some(&broken), "https://z.example").as_deref(), Some("tok3"));
    }

    /// The real keychain, with a throwaway service name. Ignored by default — it writes to the
    /// keychain of whoever runs it; `cargo test -p lemmate-core --features keychain
    /// credentials::real -- --ignored` on a desktop session.
    #[cfg(feature = "keychain")]
    #[test]
    #[ignore]
    fn real_keychain_round_trip() {
        let kc = Keychain { service: "lemmate-test" };
        let account = format!("https://test-{}.invalid", std::process::id());
        kc.set(&account, "s3cret").unwrap();
        assert_eq!(kc.get(&account).unwrap().as_deref(), Some("s3cret"));
        kc.delete(&account).unwrap();
        assert_eq!(kc.get(&account).unwrap(), None);
        kc.delete(&account).unwrap(); // idempotent
    }

    #[test]
    fn an_invite_may_be_pasted_as_a_url_or_a_bare_token() {
        assert_eq!(invite_token("  abc123 "), "abc123");
        assert_eq!(invite_token("https://notes.example.org/#/invite/abc123"), "abc123");
        assert_eq!(invite_token("http://127.0.0.1:8080/#/invite/abc123\n"), "abc123");
        assert_eq!(invite_token(""), "");
    }
}

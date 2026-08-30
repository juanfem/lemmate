//! Saved sessions for native clients: `$XDG_CONFIG_HOME/notes/credentials.toml`, one
//! `[servers."<base url>"]` table per server holding `token`. Written by `lemmate login`, read by
//! `lemmate sync` and the desktop app.

use std::path::PathBuf;

use crate::error::{Error, Result};

pub fn path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("lemmate").join("credentials.toml")
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
    read().get("servers")?.get(key(server))?.get("token")?.as_str().map(str::to_owned)
}

pub fn save(server: &str, token: &str) -> Result<()> {
    let mut root = read();
    let servers = root.entry("servers").or_insert_with(|| toml::Value::Table(toml::Table::new()));
    let Some(servers) = servers.as_table_mut() else {
        return Err(Error::Sync("credentials file is malformed".into()));
    };
    let mut entry = toml::Table::new();
    entry.insert("token".into(), toml::Value::String(token.to_owned()));
    servers.insert(key(server), toml::Value::Table(entry));
    let p = path();
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&p, toml::to_string(&root).map_err(|e| Error::Sync(e.to_string()))?)?;
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

/// The token out of an invite the admin sent, which may be the whole URL
/// (`https://notes.example.org/#/invite/<token>`) or just the token. Pasting the link is what
/// people actually do, so accept both rather than making them edit it.
pub fn invite_token(invite: &str) -> String {
    let s = invite.trim();
    s.rsplit_once("/#/invite/").map_or(s, |(_, t)| t).trim().to_owned()
}

pub fn forget(server: &str) -> Result<()> {
    let mut root = read();
    if let Some(servers) = root.get_mut("servers").and_then(|v| v.as_table_mut()) {
        servers.remove(&key(server));
    }
    std::fs::write(path(), toml::to_string(&root).map_err(|e| Error::Sync(e.to_string()))?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_in_a_temp_home() {
        let dir = tempfile::tempdir().unwrap();
        // SAFETY: tests in this module run single-threaded with respect to this variable.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", dir.path()) };
        assert_eq!(load("https://x.example"), None);
        save("https://x.example/", "tok1").unwrap();
        save("https://y.example", "tok2").unwrap();
        assert_eq!(load("https://x.example").as_deref(), Some("tok1"));
        assert_eq!(load("https://x.example/ws").as_deref(), Some("tok1"));
        forget("https://x.example").unwrap();
        assert_eq!(load("https://x.example"), None);
        assert_eq!(load("https://y.example").as_deref(), Some("tok2"));
    }

    #[test]
    fn an_invite_may_be_pasted_as_a_url_or_a_bare_token() {
        assert_eq!(invite_token("  abc123 "), "abc123");
        assert_eq!(invite_token("https://notes.example.org/#/invite/abc123"), "abc123");
        assert_eq!(invite_token("http://127.0.0.1:8080/#/invite/abc123\n"), "abc123");
        assert_eq!(invite_token(""), "");
    }
}

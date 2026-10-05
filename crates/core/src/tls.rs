//! TLS for `wss://` sync and `https://` transfers (SPEC §7, §14).
//!
//! Public CAs are trusted via the bundled Mozilla roots. Self-hosters with a private CA pass its
//! PEM file (`--ca-cert`); it then becomes the *only* trusted root for that connection, which is
//! the right shape for "my server, my CA".

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use rustls::{ClientConfig, RootCertStore};
use rustls_pki_types::CertificateDer;
use rustls_pki_types::pem::PemObject;

use crate::error::{Error, Result};

/// Make ring the process-wide rustls provider so every rustls user (tungstenite, ureq, tests)
/// agrees. Idempotent.
pub fn install_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

fn load_ca(path: &Path) -> Result<Vec<CertificateDer<'static>>> {
    let certs: Vec<_> = CertificateDer::pem_file_iter(path)
        .map_err(|e| Error::Sync(format!("reading CA certificate {}: {e}", path.display())))?
        .collect::<std::result::Result<_, _>>()
        .map_err(|e| Error::Sync(format!("parsing CA certificate {}: {e}", path.display())))?;
    if certs.is_empty() {
        return Err(Error::Sync(format!("no certificates found in {}", path.display())));
    }
    Ok(certs)
}

/// rustls client config for the WebSocket connector.
pub fn client_config(ca_cert: Option<&Path>) -> Result<Arc<ClientConfig>> {
    let mut roots = RootCertStore::empty();
    match ca_cert {
        Some(path) => {
            for c in load_ca(path)? {
                roots.add(c).map_err(|e| Error::Sync(format!("invalid CA certificate: {e}")))?;
            }
        }
        None => roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned()),
    }
    let config = ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(|e| Error::Sync(e.to_string()))?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(Arc::new(config))
}

/// How long to wait for a TCP (and TLS) connection to a server before giving up.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// The whole of one API call — vault lists, sign-in, note reads and writes. Generous for a JSON
/// answer, short enough that an unreachable or wedged server never hangs a start-up.
pub const API_TIMEOUT: Duration = Duration::from_secs(30);
/// Waiting for the first byte of an answer during an attachment transfer.
pub const TRANSFER_RESPONSE_TIMEOUT: Duration = Duration::from_secs(60);
/// The whole of one attachment transfer: 100 MiB at under 30 KiB/s still fits.
pub const TRANSFER_TIMEOUT: Duration = Duration::from_secs(60 * 60);

/// HTTP agent for REST calls, trusting the same roots as [`client_config`]. Every call is bounded
/// ([`CONNECT_TIMEOUT`], [`API_TIMEOUT`]); attachment bodies use [`transfer_agent`].
pub fn http_agent(ca_cert: Option<&Path>) -> Result<ureq::Agent> {
    agent_with(ca_cert, Some(API_TIMEOUT), None)
}

/// HTTP agent for attachment uploads and downloads (up to the 100 MiB limit), which a 30-second
/// ceiling would cut off on a slow line: the same connect timeout, a longer overall one.
pub fn transfer_agent(ca_cert: Option<&Path>) -> Result<ureq::Agent> {
    agent_with(ca_cert, Some(TRANSFER_TIMEOUT), Some(TRANSFER_RESPONSE_TIMEOUT))
}

fn agent_with(
    ca_cert: Option<&Path>,
    global: Option<Duration>,
    recv_response: Option<Duration>,
) -> Result<ureq::Agent> {
    let mut builder = ureq::Agent::config_builder()
        .timeout_connect(Some(CONNECT_TIMEOUT))
        .timeout_global(global)
        .timeout_recv_response(recv_response);
    if let Some(path) = ca_cert {
        let certs: Vec<ureq::tls::Certificate<'static>> = load_ca(path)?
            .into_iter()
            .map(|c| ureq::tls::Certificate::from_der(c.as_ref()).to_owned())
            .collect();
        let tls = ureq::tls::TlsConfig::builder()
            .root_certs(ureq::tls::RootCerts::Specific(Arc::new(certs)))
            .build();
        builder = builder.tls_config(tls);
    }
    Ok(ureq::Agent::new_with_config(builder.build()))
}

/// A warning when `server` would carry a bearer token in cleartext: an `http://` or `ws://` URL
/// whose host is not this machine. Not refused — a LAN server without TLS is a choice people
/// make — but said out loud. `None` for `https://`/`wss://` and for loopback.
pub fn cleartext_warning(server: &str) -> Option<String> {
    let url = url::Url::parse(server.trim()).ok()?;
    if !matches!(url.scheme(), "http" | "ws") {
        return None;
    }
    let loopback = match url.host()? {
        url::Host::Domain(d) => d.eq_ignore_ascii_case("localhost") || d.ends_with(".localhost"),
        url::Host::Ipv4(ip) => ip.is_loopback(),
        url::Host::Ipv6(ip) => ip.is_loopback(),
    };
    (!loopback).then(|| {
        format!(
            "{} is not encrypted ({}://): your password and session token cross the network in \
             cleartext. Use https:// unless this network is entirely yours.",
            crate::credentials::key(server),
            url.scheme()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_or_empty_ca_is_an_error() {
        assert!(client_config(Some(Path::new("/nonexistent.pem"))).is_err());
        let f = tempfile::NamedTempFile::new().unwrap();
        assert!(client_config(Some(f.path())).is_err());
        assert!(http_agent(Some(f.path())).is_err());
        assert!(client_config(None).is_ok());
        assert!(http_agent(None).is_ok());
        assert!(transfer_agent(Some(f.path())).is_err());
    }

    #[test]
    fn api_calls_are_bounded_and_transfers_get_longer() {
        let api = http_agent(None).unwrap();
        let t = &api.config().timeouts();
        assert_eq!(t.connect, Some(CONNECT_TIMEOUT));
        assert_eq!(t.global, Some(API_TIMEOUT));
        let transfer = transfer_agent(None).unwrap();
        let t = &transfer.config().timeouts();
        assert_eq!(t.connect, Some(CONNECT_TIMEOUT));
        assert!(t.global.is_some_and(|g| g > API_TIMEOUT));
    }

    #[test]
    fn cleartext_is_flagged_off_this_machine_only() {
        assert!(cleartext_warning("http://notes.example.org").is_some());
        assert!(cleartext_warning("ws://192.168.1.5:8080/ws").is_some());
        assert!(cleartext_warning("https://notes.example.org").is_none());
        assert!(cleartext_warning("wss://notes.example.org/ws").is_none());
        for local in ["http://localhost:8080", "http://127.0.0.1:1/", "http://[::1]:8080", "http://LOCALHOST"]
        {
            assert!(cleartext_warning(local).is_none(), "{local}");
        }
        assert!(cleartext_warning("not a url").is_none());
    }

    /// A server that accepts the connection and then never answers must not hang the caller.
    #[test]
    fn a_silent_server_times_out() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let _hold = std::thread::spawn(move || {
            let conns: Vec<_> = listener.incoming().take(1).collect();
            std::thread::sleep(Duration::from_secs(5));
            drop(conns);
        });
        let agent = agent_with(None, Some(Duration::from_millis(300)), None).unwrap();
        let started = std::time::Instant::now();
        let err = agent.get(format!("http://{addr}/")).call().unwrap_err();
        assert!(matches!(err, ureq::Error::Timeout(_)), "{err}");
        assert!(started.elapsed() < Duration::from_secs(3));
    }
}

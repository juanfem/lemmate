//! TLS for `wss://` sync and `https://` transfers (SPEC §7, §14).
//!
//! Public CAs are trusted via the bundled Mozilla roots. Self-hosters with a private CA pass its
//! PEM file (`--ca-cert`); it then becomes the *only* trusted root for that connection, which is
//! the right shape for "my server, my CA".

use std::path::Path;
use std::sync::Arc;

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

/// HTTP agent for attachment transfers, trusting the same roots as [`client_config`].
pub fn http_agent(ca_cert: Option<&Path>) -> Result<ureq::Agent> {
    let mut builder = ureq::Agent::config_builder();
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
    }
}

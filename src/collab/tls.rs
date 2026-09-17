use std::io::BufReader;
use std::sync::Arc;
use std::sync::Once;
use std::time::Duration;

use anyhow::Context;
use rcgen::generate_simple_self_signed;
use reqwest::blocking::Client;
use rustls::{ClientConfig, RootCertStore};
use tungstenite::Connector;

const HTTP_TIMEOUT_SECS: u64 = 5;

#[derive(Debug, Clone)]
pub struct TlsMaterial {
    pub cert_pem: String,
    pub key_pem: String,
}

pub fn ensure_crypto_provider() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    });
}

pub fn generate_tls_material(subject_alt_names: Vec<String>) -> anyhow::Result<TlsMaterial> {
    ensure_crypto_provider();
    let certified = generate_simple_self_signed(subject_alt_names)
        .context("failed to generate self-signed certificate")?;
    Ok(TlsMaterial {
        cert_pem: certified.cert.pem(),
        key_pem: certified.key_pair.serialize_pem(),
    })
}

pub fn http_client(tls_cert_pem: Option<&str>) -> anyhow::Result<Client> {
    ensure_crypto_provider();
    let mut builder = Client::builder()
        .timeout(Duration::from_secs(HTTP_TIMEOUT_SECS))
        .https_only(true)
        // Credentials in JSON bodies must not follow a broker redirect.
        .redirect(reqwest::redirect::Policy::none());
    if let Some(cert_pem) = tls_cert_pem {
        // Use exactly the same exclusive trust store as WebSocket. Adding a
        // root to reqwest's default store is not certificate pinning.
        builder = builder.use_preconfigured_tls(pinned_tls_config(cert_pem)?);
    }
    builder.build().context("failed to build HTTP client")
}

pub fn websocket_connector(tls_cert_pem: Option<&str>) -> anyhow::Result<Option<Connector>> {
    ensure_crypto_provider();
    let Some(cert_pem) = tls_cert_pem else {
        return Ok(None);
    };

    Ok(Some(Connector::Rustls(Arc::new(pinned_tls_config(cert_pem)?))))
}

fn pinned_roots(cert_pem: &str) -> anyhow::Result<RootCertStore> {
    let mut reader = BufReader::new(cert_pem.as_bytes());
    let certs = rustls_pemfile::certs(&mut reader)
        .collect::<Result<Vec<_>, _>>()
        .context("failed to parse pinned certificate")?;
    anyhow::ensure!(certs.len() == 1, "invite must contain exactly one pinned certificate");
    let mut roots = RootCertStore::empty();
    roots.add(certs.into_iter().next().expect("one pinned certificate"))
        .context("failed to add pinned certificate")?;
    Ok(roots)
}

fn pinned_tls_config(cert_pem: &str) -> anyhow::Result<ClientConfig> {
    Ok(ClientConfig::builder()
        .with_root_certificates(pinned_roots(cert_pem)?)
        .with_no_client_auth())
}

#[cfg(test)]
#[path = "tls_security_tests.rs"]
mod security_tests;

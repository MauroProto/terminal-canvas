use std::io::BufReader;
use std::sync::Arc;
use std::sync::Once;
use std::time::Duration;

use anyhow::Context;
use rcgen::generate_simple_self_signed;
use reqwest::blocking::Client;
use reqwest::Certificate;
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
    let mut builder = Client::builder().timeout(Duration::from_secs(HTTP_TIMEOUT_SECS));
    if let Some(cert_pem) = tls_cert_pem {
        let cert = Certificate::from_pem(cert_pem.as_bytes())
            .context("failed to parse pinned TLS certificate")?;
        builder = builder.add_root_certificate(cert).https_only(true);
    }
    builder.build().context("failed to build HTTP client")
}

pub fn websocket_connector(tls_cert_pem: Option<&str>) -> anyhow::Result<Option<Connector>> {
    ensure_crypto_provider();
    let Some(cert_pem) = tls_cert_pem else {
        return Ok(None);
    };

    let mut reader = BufReader::new(cert_pem.as_bytes());
    let certs = rustls_pemfile::certs(&mut reader)
        .collect::<Result<Vec<_>, _>>()
        .context("failed to parse pinned websocket certificate")?;

    let mut roots = RootCertStore::empty();
    for cert in certs {
        roots
            .add(cert)
            .context("failed to add pinned websocket certificate")?;
    }

    let config = ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();

    Ok(Some(Connector::Rustls(Arc::new(config))))
}

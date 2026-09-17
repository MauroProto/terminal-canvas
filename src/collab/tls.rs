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
    let mut builder = Client::builder()
        .timeout(Duration::from_secs(HTTP_TIMEOUT_SECS))
        .https_only(true)
        .redirect(reqwest::redirect::Policy::none());
    if let Some(cert_pem) = tls_cert_pem {
        let cert = Certificate::from_pem(cert_pem.as_bytes())
            .context("failed to parse pinned TLS certificate")?;
        builder = builder
            .tls_built_in_root_certs(false)
            .add_root_certificate(cert);
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

    anyhow::ensure!(certs.len() == 1, "Expected one pinned certificate");
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

#[cfg(test)]
mod security_tls_tests {
    use super::*;
    use crate::collab::server::EmbeddedCollabServer;
    #[test]
    fn pinned_http_accepts_its_host_and_rejects_a_different_certificate() {
        let material = generate_tls_material(vec!["127.0.0.1".to_owned()]).unwrap();
        let other = generate_tls_material(vec!["127.0.0.1".to_owned()]).unwrap();
        let mut server = EmbeddedCollabServer::start(
            "127.0.0.1:0".parse().unwrap(),
            material.cert_pem.clone(),
            material.key_pem,
        )
        .unwrap();
        let response = http_client(Some(&material.cert_pem))
            .unwrap()
            .get(server.local_api_url())
            .send();
        assert!(
            response.is_ok(),
            "the pinned host must be reachable: {response:?}"
        );
        assert!(http_client(Some(&other.cert_pem))
            .unwrap()
            .get(server.local_api_url())
            .send()
            .is_err());
        assert!(http_client(None)
            .unwrap()
            .get(server.local_api_url())
            .send()
            .is_err());
        server.stop().unwrap();
    }
    #[test]
    fn malformed_pins_fail_closed_for_http_and_websocket() {
        assert!(http_client(Some("invalid certificate")).is_err());
        assert!(websocket_connector(Some("invalid certificate")).is_err());
    }
}

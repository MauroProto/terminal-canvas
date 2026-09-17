use super::*;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;
use std::time::Instant;

// One bounded loopback request. No real certificates or external endpoint.
fn fixture_server(
    material: &TlsMaterial,
    status: &'static str,
) -> (String, thread::JoinHandle<()>) {
    let certs = rustls_pemfile::certs(&mut material.cert_pem.as_bytes())
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let key = rustls_pemfile::private_key(&mut material.key_pem.as_bytes())
        .unwrap()
        .unwrap();
    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let handle = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        let stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(
                        Instant::now() < deadline,
                        "fixture did not receive a connection"
                    );
                    thread::sleep(Duration::from_millis(5));
                }
                Err(err) => panic!("fixture accept: {err}"),
            }
        };
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let connection = rustls::ServerConnection::new(Arc::new(config)).unwrap();
        let mut tls = rustls::StreamOwned::new(connection, stream);
        let mut buffer = [0u8; 4096];
        if tls.read(&mut buffer).is_ok() {
            let _ = tls.write_all(status.as_bytes());
            let _ = tls.flush();
        }
    });
    (format!("https://{address}"), handle)
}

fn certificate() -> TlsMaterial {
    generate_tls_material(vec!["127.0.0.1".to_owned(), "localhost".to_owned()]).unwrap()
}

#[test]
fn security_http_accepts_only_the_invite_certificate() {
    let pinned = certificate();
    let other = certificate();
    let client = http_client(Some(&pinned.cert_pem)).unwrap();
    let (url, server) = fixture_server(
        &other,
        "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
    );
    assert!(client.get(url).send().is_err());
    server.join().unwrap();
    let (url, server) = fixture_server(
        &pinned,
        "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
    );
    assert_eq!(
        client.get(url).send().unwrap().status(),
        reqwest::StatusCode::OK
    );
    server.join().unwrap();
}

#[test]
fn security_broker_http_never_follows_credential_redirects() {
    let material = certificate();
    let (url, server) = fixture_server(&material, "HTTP/1.1 307 Temporary Redirect\r\nLocation: https://never-contact.example.invalid/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
    let response = http_client(Some(&material.cert_pem))
        .unwrap()
        .post(url)
        .json(&serde_json::json!({"passphrase": "dummy-fixture"}))
        .send()
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::TEMPORARY_REDIRECT);
    server.join().unwrap();
}

#[test]
fn security_pin_store_is_exclusive_and_rejects_missing_or_multiple_certificates() {
    let material = certificate();
    assert_eq!(pinned_roots(&material.cert_pem).unwrap().len(), 1);
    for pem in [
        String::new(),
        "not a certificate".to_owned(),
        format!("{}{}", material.cert_pem, material.cert_pem),
    ] {
        assert!(http_client(Some(&pem)).is_err());
        assert!(websocket_connector(Some(&pem)).is_err());
    }
}

use super::*;
use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener};
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
        // accept() does not have portable flag-inheritance semantics. The
        // listener polls, but rustls StreamOwned below requires blocking I/O.
        stream.set_nonblocking(false).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let connection = rustls::ServerConnection::new(Arc::new(config)).unwrap();
        let mut tls = rustls::StreamOwned::new(connection, stream);
        let mut byte = [0u8; 1];
        // Rejection of the other certificate is expected before HTTP starts.
        // Retain the error in failed-test output instead of hiding I/O errors.
        if let Err(error) = tls.read_exact(&mut byte) {
            eprintln!("TLS fixture stopped before HTTP: {error:?}");
            return;
        }
        let mut headers = vec![byte[0]];
        while !headers.ends_with(b"\r\n\r\n") {
            assert!(headers.len() < 8192, "fixture request headers too large");
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(!remaining.is_zero(), "fixture request deadline exceeded");
            tls.sock.set_read_timeout(Some(remaining)).unwrap();
            tls.read_exact(&mut byte).expect("complete HTTP headers");
            headers.push(byte[0]);
        }
        let headers = std::str::from_utf8(&headers).unwrap();
        let mut content_length = None;
        for line in headers.lines().skip(1) {
            let Some((name, value)) = line.split_once(':') else {
                continue;
            };
            assert!(
                !name.eq_ignore_ascii_case("transfer-encoding"),
                "fixture expects a bounded Content-Length body"
            );
            if name.eq_ignore_ascii_case("content-length") {
                assert!(content_length.is_none(), "duplicate fixture Content-Length");
                content_length = Some(value.trim().parse::<usize>().unwrap());
            }
        }
        let content_length = content_length.unwrap_or(0);
        assert!(content_length <= 4096, "fixture request body too large");
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(!remaining.is_zero(), "fixture request deadline exceeded");
        tls.sock.set_read_timeout(Some(remaining)).unwrap();
        let mut body = vec![0u8; content_length];
        tls.read_exact(&mut body).expect("complete HTTP body");

        // Closing with unread request bytes can reset the socket on macOS.
        // Consume the body first, then flush both the response and TLS alert.
        tls.write_all(status.as_bytes()).expect("fixture response");
        tls.conn.send_close_notify();
        tls.flush().expect("fixture TLS close_notify");
        tls.sock.shutdown(Shutdown::Write).unwrap();
        // Allow the peer to consume the response and close. Cleanup is bounded
        // and never changes the client-side certificate/status assertions.
        let mut ignored = [0u8; 1024];
        let mut drained = 0usize;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() || drained >= 8192 {
                break;
            }
            // The peer may already have closed after consuming the response.
            // macOS can reject this cleanup-only socket option with EINVAL.
            // Stop draining rather than failing an already completed exchange;
            // request reads, response writes and client assertions stay strict.
            if tls.sock.set_read_timeout(Some(remaining)).is_err() {
                break;
            }
            match tls.sock.read(&mut ignored) {
                Ok(0) | Err(_) => break,
                Ok(count) => drained += count,
            }
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
        "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
    );
    assert!(client.get(url).send().is_err());
    server.join().unwrap();
    let (url, server) = fixture_server(
        &pinned,
        "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
    );
    let response = client.get(url).send().unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(response.text().unwrap(), "ok");
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
    assert!(response.bytes().unwrap().is_empty());
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

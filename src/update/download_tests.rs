//! The production transport against a bounded real HTTPS server. Every cache
//! is disposable; no test changes the process-wide application profile.
use super::*;
use std::net::TcpListener;
use std::thread::JoinHandle;

const NAME: &str = "TerminalCanvas-1.3.0-windows-x86_64.zip";

struct Cache(PathBuf);

impl Cache {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("tc-download-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn assert_empty(&self) {
        assert_eq!(std::fs::read_dir(&self.0).unwrap().count(), 0);
    }
}

impl Drop for Cache {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Reply {
    body: Vec<u8>,
    advertised_length: Option<u64>,
    redirect: bool,
    stall: bool,
}

impl Reply {
    fn body(body: impl Into<Vec<u8>>) -> Self {
        Self {
            body: body.into(),
            advertised_length: None,
            redirect: false,
            stall: false,
        }
    }
}

struct Server {
    url: String,
    client: reqwest::Client,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<usize>>,
}

impl Server {
    fn new(replies: Vec<Reply>) -> Self {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let material = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
        let certificate = material.cert.der().clone();
        let key = rustls::pki_types::PrivateKeyDer::Pkcs8(material.key_pair.serialize_der().into());
        let config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![certificate.clone()], key)
            .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let worker = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut handled = 0;
            for (index, reply) in replies.into_iter().enumerate() {
                let stream = loop {
                    if worker_stop.load(Ordering::Acquire) || Instant::now() >= deadline {
                        return handled;
                    }
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(5))
                        }
                        Err(error) => panic!("HTTPS fixture accept: {error}"),
                    }
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let connection = rustls::ServerConnection::new(Arc::new(config.clone())).unwrap();
                let mut tls = rustls::StreamOwned::new(connection, stream);
                let mut headers = Vec::new();
                while !headers.ends_with(b"\r\n\r\n") {
                    let mut byte = [0];
                    if tls.read_exact(&mut byte).is_err() {
                        return handled;
                    }
                    headers.push(byte[0]);
                    assert!(headers.len() <= 8192);
                }
                let path = if index == 0 {
                    format!("/{NAME}.sha256")
                } else {
                    format!("/{NAME}")
                };
                assert!(String::from_utf8(headers)
                    .unwrap()
                    .starts_with(&format!("GET {path} HTTP/1.1\r\n")));
                handled += 1;
                let length = reply.advertised_length.unwrap_or(reply.body.len() as u64);
                let header = if reply.redirect {
                    "HTTP/1.1 302 Found\r\nLocation: https://example.invalid/update\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned()
                } else {
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n"
                    )
                };
                if tls.write_all(header.as_bytes()).is_err() {
                    return handled;
                }
                // A client that rejects the size/checksum is allowed to close
                // before the fixture finishes sending the response.
                if tls.write_all(&reply.body).is_err() || tls.flush().is_err() {
                    return handled;
                }
                while reply.stall
                    && !worker_stop.load(Ordering::Acquire)
                    && Instant::now() < deadline
                {
                    thread::sleep(Duration::from_millis(5));
                }
                tls.conn.send_close_notify();
                let _ = tls.flush();
            }
            handled
        });
        let client = reqwest::Client::builder()
            .https_only(true)
            .no_proxy()
            .tls_built_in_root_certs(false)
            .add_root_certificate(reqwest::Certificate::from_der(certificate.as_ref()).unwrap())
            .timeout(Duration::from_secs(3))
            .redirect(release_redirect_policy())
            .build()
            .unwrap();
        Self {
            url: format!("https://localhost:{}/{NAME}", address.port()),
            client,
            stop,
            worker: Some(worker),
        }
    }

    fn finish(&mut self) -> usize {
        self.stop.store(true, Ordering::Release);
        self.worker.take().unwrap().join().unwrap()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn checksum_reply(bytes: &[u8]) -> Reply {
    Reply::body(format!("{}  {NAME}\n", checksum_string(bytes)).into_bytes())
}

fn download(
    server: &Server,
    cache: &Cache,
    state: &Mutex<UpdateState>,
    cancellation: &AtomicBool,
) -> Result<(PathBuf, String), String> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(download_with_client(
            &server.url,
            &server.client,
            state,
            cancellation,
            &egui::Context::default(),
            &cache.0,
        ))
}

#[test]
fn https_download_publishes_only_the_complete_verified_file_and_reports_progress() {
    let body = b"A complete release payload with Unicode: \xc3\xb1";
    let cache = Cache::new();
    let mut server = Server::new(vec![checksum_reply(body), Reply::body(body.to_vec())]);
    let state = Mutex::new(UpdateState::default());
    let (path, hash) = download(&server, &cache, &state, &AtomicBool::new(false)).unwrap();
    assert_eq!(server.finish(), 2);
    assert_eq!(std::fs::read(&path).unwrap(), body);
    assert_eq!(hash, checksum_string(body));
    assert!(path.starts_with(&cache.0));
    assert_eq!(state.lock().unwrap().downloaded_bytes, body.len() as u64);
    assert_eq!(state.lock().unwrap().total_bytes, Some(body.len() as u64));
    let files: Vec<_> = std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .map(|file| file.unwrap().file_name())
        .collect();
    assert_eq!(files.len(), 2);
    assert!(!files
        .iter()
        .any(|file| file.to_string_lossy().ends_with(".part")));
    assert!(verify_checksum(&path, &hash));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[test]
fn https_download_rejects_corruption_truncation_size_and_wrong_manifest_without_retaining_files() {
    let body = b"release bytes";
    let scenarios = [
        (
            vec![
                checksum_reply(b"different bytes"),
                Reply::body(body.to_vec()),
            ],
            "checksum mismatch",
            2,
        ),
        (
            vec![
                checksum_reply(body),
                Reply {
                    advertised_length: Some(100),
                    ..Reply::body(body.to_vec())
                },
            ],
            "interrupted",
            2,
        ),
        (
            vec![
                checksum_reply(body),
                Reply {
                    advertised_length: Some(MAX_DOWNLOAD_BYTES + 1),
                    ..Reply::body(body.to_vec())
                },
            ],
            "exceeds",
            2,
        ),
        (
            vec![Reply::body(
                format!("{} other.zip\n", checksum_string(body)).into_bytes(),
            )],
            "exact release file",
            1,
        ),
        (
            vec![Reply::body(vec![b'x'; MAX_CHECKSUM_BYTES + 1])],
            "too large",
            1,
        ),
        (
            vec![Reply {
                redirect: true,
                ..Reply::body(Vec::new())
            }],
            "checksum",
            1,
        ),
    ];
    for (replies, reason, expected_requests) in scenarios {
        let cache = Cache::new();
        let mut server = Server::new(replies);
        let error = download(
            &server,
            &cache,
            &Mutex::new(UpdateState::default()),
            &AtomicBool::new(false),
        )
        .unwrap_err();
        assert!(error.contains(reason), "Expected {reason}: {error}");
        assert_eq!(server.finish(), expected_requests);
        cache.assert_empty();
    }
}

#[test]
fn cancellation_interrupts_a_stalled_https_body_and_removes_its_partial_file() {
    let cache = Cache::new();
    let mut server = Server::new(vec![
        checksum_reply(b"longer complete release"),
        Reply {
            advertised_length: Some(1024),
            stall: true,
            ..Reply::body(b"first chunk".to_vec())
        },
    ]);
    let state = Arc::new(Mutex::new(UpdateState::default()));
    let cancellation = Arc::new(AtomicBool::new(false));
    let worker_state = Arc::clone(&state);
    let worker_cancellation = Arc::clone(&cancellation);
    let url = server.url.clone();
    let client = server.client.clone();
    let root = cache.0.clone();
    let worker = thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(download_with_client(
                &url,
                &client,
                &worker_state,
                &worker_cancellation,
                &egui::Context::default(),
                &root,
            ))
    });
    let deadline = Instant::now() + Duration::from_secs(2);
    while state.lock().unwrap().downloaded_bytes == 0 && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(5));
    }
    let received_body = state.lock().unwrap().downloaded_bytes > 0;
    let cancelled_at = Instant::now();
    cancellation.store(true, Ordering::Release);
    let result = worker.join().unwrap();
    server.finish();
    assert!(
        received_body,
        "Fixture must deliver bytes before cancellation"
    );
    assert!(cancelled_at.elapsed() < Duration::from_secs(1));
    assert!(result.unwrap_err().contains("cancelled"));
    cache.assert_empty();
}

#[test]
fn production_transport_rejects_an_untrusted_url_before_network_or_cache_access() {
    let error = download_release_asset(
        "https://example.invalid/release.zip",
        &Mutex::new(UpdateState::default()),
        &AtomicBool::new(false),
        &egui::Context::default(),
    )
    .unwrap_err();
    assert!(error.contains("official HTTPS"));
}

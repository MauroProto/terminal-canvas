//! Despliegue embebido del broker de colaboración: lo sirve sobre TLS en un
//! hilo propio dentro de la app. Toda la lógica de sesiones/relay vive en
//! [`super::broker`]; acá solo está el arranque y el apagado.

use std::net::{SocketAddr, TcpListener};
use std::thread;
use std::time::Duration;

use axum_server::from_tcp_rustls;
use axum_server::tls_rustls::RustlsConfig;
use axum_server::Handle;
use tokio::runtime::Runtime;

use super::broker::{build_router, spawn_cleanup_task, BrokerConfig, BrokerState};
use super::tls::ensure_crypto_provider;

pub struct EmbeddedCollabServer {
    local_api_url: String,
    handle: Handle,
    thread_handle: Option<thread::JoinHandle<()>>,
}

impl EmbeddedCollabServer {
    pub fn start(bind_addr: SocketAddr, cert_pem: String, key_pem: String) -> anyhow::Result<Self> {
        ensure_crypto_provider();
        let listener = TcpListener::bind(bind_addr)?;
        listener.set_nonblocking(true)?;
        let local_addr = listener.local_addr()?;

        let state = BrokerState::new(BrokerConfig {
            // La API puede quedar expuesta a la LAN para que se conecten los
            // guests, pero crear sesiones es privilegio del host local.
            require_loopback_session_creation: true,
        });
        let router = build_router(state.clone());
        let handle = Handle::new();
        let handle_for_thread = handle.clone();

        // No expect/unwrap in this thread: a panic here would take down the
        // whole app just to fail starting a share session.
        let thread_handle = thread::spawn(move || {
            let runtime = match Runtime::new() {
                Ok(runtime) => runtime,
                Err(err) => {
                    log::error!("failed to create embedded collab server runtime: {err}");
                    return;
                }
            };
            runtime.block_on(async move {
                spawn_cleanup_task(state);
                let tls_config =
                    match RustlsConfig::from_pem(cert_pem.into_bytes(), key_pem.into_bytes()).await
                    {
                        Ok(config) => config,
                        Err(err) => {
                            log::error!(
                                "failed to build rustls config for embedded collab server: {err}"
                            );
                            return;
                        }
                    };
                if let Err(err) = from_tcp_rustls(listener, tls_config)
                    .handle(handle_for_thread)
                    .serve(router.into_make_service_with_connect_info::<SocketAddr>())
                    .await
                {
                    log::error!("embedded collab server stopped with error: {err}");
                }
            });
        });

        Ok(Self {
            local_api_url: format!("https://127.0.0.1:{}", local_addr.port()),
            handle,
            thread_handle: Some(thread_handle),
        })
    }

    pub fn local_api_url(&self) -> &str {
        &self.local_api_url
    }

    pub fn stop(&mut self) -> anyhow::Result<()> {
        self.handle.graceful_shutdown(Some(Duration::from_secs(1)));
        if let Some(thread_handle) = self.thread_handle.take() {
            let _ = thread_handle.join();
        }
        Ok(())
    }
}

impl Drop for EmbeddedCollabServer {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
    use std::time::Duration;

    use crate::collab::auth::hash_passphrase;
    use crate::collab::protocol::{
        CreateShareSessionRequest, CreateShareSessionResponse, EndShareSessionRequest,
        JoinShareSessionRequest, JoinShareSessionResponse, RotateInviteRequest,
    };
    use crate::collab::tls::{generate_tls_material, http_client};
    use crate::collab::transport::json_post;
    use chrono::Utc;

    use super::EmbeddedCollabServer;

    fn start_test_server() -> (EmbeddedCollabServer, String) {
        let tls_material =
            generate_tls_material(vec!["127.0.0.1".to_owned(), "localhost".to_owned()])
                .expect("generate tls material");
        let cert_pem = tls_material.cert_pem.clone();
        let server = EmbeddedCollabServer::start(
            SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)),
            tls_material.cert_pem,
            tls_material.key_pem,
        )
        .expect("start embedded collab server");
        (server, cert_pem)
    }

    fn join_status(
        server: &EmbeddedCollabServer,
        cert_pem: &str,
        session_id: uuid::Uuid,
        body: &JoinShareSessionRequest,
    ) -> reqwest::StatusCode {
        http_client(Some(cert_pem))
            .expect("http client")
            .post(format!(
                "{}/v1/share-sessions/{}/join",
                server.local_api_url(),
                session_id
            ))
            .json(body)
            .send()
            .expect("join response")
            .status()
    }

    #[test]
    fn embedded_server_serves_create_join_and_end_session() {
        let (mut server, cert_pem) = start_test_server();
        let create: CreateShareSessionResponse = json_post(
            &format!("{}/v1/share-sessions", server.local_api_url()),
            &CreateShareSessionRequest {
                session_secret: "secret".to_owned(),
                invite_secret: "invite-secret".to_owned(),
                invite_expires_at: None,
                passphrase_hash: None,
                trusted_devices: Vec::new(),
            },
            Some(cert_pem.as_str()),
        )
        .expect("create session");
        let join: JoinShareSessionResponse = json_post(
            &format!(
                "{}/v1/share-sessions/{}/join",
                server.local_api_url(),
                create.session_id.0
            ),
            &JoinShareSessionRequest {
                display_name: "Guest".to_owned(),
                invite_secret: "invite-secret".to_owned(),
                device_id: "device-1".to_owned(),
                passphrase: None,
            },
            Some(cert_pem.as_str()),
        )
        .expect("join session");
        assert_ne!(join.guest_id.0, uuid::Uuid::nil());
        assert!(!join.auto_approved);

        let end_response: serde_json::Value = json_post(
            &format!(
                "{}/v1/share-sessions/{}/end",
                server.local_api_url(),
                create.session_id.0
            ),
            &EndShareSessionRequest {
                host_token: create.host_token.clone(),
            },
            Some(cert_pem.as_str()),
        )
        .expect("end session");
        assert_eq!(end_response["ok"], true);

        server.stop().expect("stop server");
    }

    #[test]
    fn embedded_server_rejects_join_with_invalid_invite_secret() {
        let (mut server, cert_pem) = start_test_server();
        let create: CreateShareSessionResponse = json_post(
            &format!("{}/v1/share-sessions", server.local_api_url()),
            &CreateShareSessionRequest {
                session_secret: "secret".to_owned(),
                invite_secret: "invite-secret".to_owned(),
                invite_expires_at: None,
                passphrase_hash: None,
                trusted_devices: Vec::new(),
            },
            Some(cert_pem.as_str()),
        )
        .expect("create session");
        let join_result = json_post::<_, JoinShareSessionResponse>(
            &format!(
                "{}/v1/share-sessions/{}/join",
                server.local_api_url(),
                create.session_id.0
            ),
            &JoinShareSessionRequest {
                display_name: "Guest".to_owned(),
                invite_secret: "wrong-secret".to_owned(),
                device_id: "device-1".to_owned(),
                passphrase: None,
            },
            Some(cert_pem.as_str()),
        );
        assert!(join_result.is_err());
        server.stop().expect("stop server");
    }

    #[test]
    fn embedded_server_requires_matching_session_passphrase_when_configured() {
        let (mut server, cert_pem) = start_test_server();
        let create: CreateShareSessionResponse = json_post(
            &format!("{}/v1/share-sessions", server.local_api_url()),
            &CreateShareSessionRequest {
                session_secret: "secret".to_owned(),
                invite_secret: "invite-secret".to_owned(),
                invite_expires_at: None,
                passphrase_hash: Some(hash_passphrase("clave-super-segura").expect("hash")),
                trusted_devices: Vec::new(),
            },
            Some(cert_pem.as_str()),
        )
        .expect("create session");

        let missing = join_status(
            &server,
            cert_pem.as_str(),
            create.session_id.0,
            &JoinShareSessionRequest {
                display_name: "Guest".to_owned(),
                invite_secret: "invite-secret".to_owned(),
                device_id: "device-1".to_owned(),
                passphrase: None,
            },
        );
        assert_eq!(missing, reqwest::StatusCode::UNAUTHORIZED);

        std::thread::sleep(Duration::from_millis(1100));

        let joined: JoinShareSessionResponse = json_post(
            &format!(
                "{}/v1/share-sessions/{}/join",
                server.local_api_url(),
                create.session_id.0
            ),
            &JoinShareSessionRequest {
                display_name: "Guest".to_owned(),
                invite_secret: "invite-secret".to_owned(),
                device_id: "device-1".to_owned(),
                passphrase: Some("clave-super-segura".to_owned()),
            },
            Some(cert_pem.as_str()),
        )
        .expect("join with passphrase");
        assert_ne!(joined.guest_id.0, uuid::Uuid::nil());
        server.stop().expect("stop server");
    }

    #[test]
    fn embedded_server_rate_limits_repeated_failed_joins() {
        let (mut server, cert_pem) = start_test_server();
        let create: CreateShareSessionResponse = json_post(
            &format!("{}/v1/share-sessions", server.local_api_url()),
            &CreateShareSessionRequest {
                session_secret: "secret".to_owned(),
                invite_secret: "invite-secret".to_owned(),
                invite_expires_at: None,
                passphrase_hash: Some(hash_passphrase("clave-super-segura").expect("hash")),
                trusted_devices: Vec::new(),
            },
            Some(cert_pem.as_str()),
        )
        .expect("create session");

        let first = join_status(
            &server,
            cert_pem.as_str(),
            create.session_id.0,
            &JoinShareSessionRequest {
                display_name: "Guest".to_owned(),
                invite_secret: "invite-secret".to_owned(),
                device_id: "device-1".to_owned(),
                passphrase: Some("incorrecta".to_owned()),
            },
        );
        assert_eq!(first, reqwest::StatusCode::UNAUTHORIZED);

        let second = join_status(
            &server,
            cert_pem.as_str(),
            create.session_id.0,
            &JoinShareSessionRequest {
                display_name: "Guest".to_owned(),
                invite_secret: "invite-secret".to_owned(),
                device_id: "device-1".to_owned(),
                passphrase: Some("incorrecta".to_owned()),
            },
        );
        assert_eq!(second, reqwest::StatusCode::TOO_MANY_REQUESTS);
        server.stop().expect("stop server");
    }

    #[test]
    fn embedded_server_rejects_expired_invites() {
        let (mut server, cert_pem) = start_test_server();
        let create: CreateShareSessionResponse = json_post(
            &format!("{}/v1/share-sessions", server.local_api_url()),
            &CreateShareSessionRequest {
                session_secret: "session-secret".to_owned(),
                invite_secret: "invite-secret".to_owned(),
                invite_expires_at: Some(Utc::now() - chrono::Duration::minutes(1)),
                passphrase_hash: None,
                trusted_devices: Vec::new(),
            },
            Some(cert_pem.as_str()),
        )
        .expect("create session");

        let status = join_status(
            &server,
            cert_pem.as_str(),
            create.session_id.0,
            &JoinShareSessionRequest {
                display_name: "Guest".to_owned(),
                invite_secret: "invite-secret".to_owned(),
                device_id: "device-1".to_owned(),
                passphrase: None,
            },
        );
        assert_eq!(status, reqwest::StatusCode::GONE);
        server.stop().expect("stop server");
    }

    #[test]
    fn embedded_server_rotates_invite_without_breaking_session() {
        let (mut server, cert_pem) = start_test_server();
        let create: CreateShareSessionResponse = json_post(
            &format!("{}/v1/share-sessions", server.local_api_url()),
            &CreateShareSessionRequest {
                session_secret: "session-secret".to_owned(),
                invite_secret: "invite-secret-1".to_owned(),
                invite_expires_at: None,
                passphrase_hash: None,
                trusted_devices: Vec::new(),
            },
            Some(cert_pem.as_str()),
        )
        .expect("create session");

        let _: serde_json::Value = json_post(
            &format!(
                "{}/v1/share-sessions/{}/rotate-invite",
                server.local_api_url(),
                create.session_id.0
            ),
            &RotateInviteRequest {
                host_token: create.host_token.clone(),
                invite_secret: "invite-secret-2".to_owned(),
                invite_expires_at: Some(Utc::now() + chrono::Duration::hours(24)),
            },
            Some(cert_pem.as_str()),
        )
        .expect("rotate invite");

        let old_status = join_status(
            &server,
            cert_pem.as_str(),
            create.session_id.0,
            &JoinShareSessionRequest {
                display_name: "Guest".to_owned(),
                invite_secret: "invite-secret-1".to_owned(),
                device_id: "device-1".to_owned(),
                passphrase: None,
            },
        );
        assert_eq!(old_status, reqwest::StatusCode::UNAUTHORIZED);

        let joined: JoinShareSessionResponse = json_post(
            &format!(
                "{}/v1/share-sessions/{}/join",
                server.local_api_url(),
                create.session_id.0
            ),
            &JoinShareSessionRequest {
                display_name: "Guest".to_owned(),
                invite_secret: "invite-secret-2".to_owned(),
                device_id: "device-1".to_owned(),
                passphrase: None,
            },
            Some(cert_pem.as_str()),
        )
        .expect("join with rotated invite");
        assert_ne!(joined.guest_id.0, uuid::Uuid::nil());
        server.stop().expect("stop server");
    }

    #[test]
    fn embedded_server_auto_approves_trusted_devices() {
        let (mut server, cert_pem) = start_test_server();
        let create: CreateShareSessionResponse = json_post(
            &format!("{}/v1/share-sessions", server.local_api_url()),
            &CreateShareSessionRequest {
                session_secret: "session-secret".to_owned(),
                invite_secret: "invite-secret".to_owned(),
                invite_expires_at: None,
                passphrase_hash: None,
                trusted_devices: vec![crate::collab::TrustedDevice {
                    device_id: "device-1".to_owned(),
                    last_display_name: "Mauro".to_owned(),
                    approved_at: Utc::now(),
                    last_seen_at: Utc::now(),
                }],
            },
            Some(cert_pem.as_str()),
        )
        .expect("create session");

        let joined: JoinShareSessionResponse = json_post(
            &format!(
                "{}/v1/share-sessions/{}/join",
                server.local_api_url(),
                create.session_id.0
            ),
            &JoinShareSessionRequest {
                display_name: "Mauro".to_owned(),
                invite_secret: "invite-secret".to_owned(),
                device_id: "device-1".to_owned(),
                passphrase: None,
            },
            Some(cert_pem.as_str()),
        )
        .expect("join trusted device");
        assert!(joined.auto_approved);
        server.stop().expect("stop server");
    }
}

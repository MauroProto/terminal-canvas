//! Broker de colaboración standalone: sirve el núcleo compartido
//! (`mi_terminal::collab::broker`) como HTTP plano, pensado para correr
//! detrás de un proxy TLS. El servidor embebido de la app usa exactamente la
//! misma lógica, así que cualquier fix aplica a ambos despliegues.

use std::net::SocketAddr;

use mi_terminal::collab::broker::{build_router, spawn_cleanup_task, BrokerConfig, BrokerState};

#[tokio::main]
async fn main() {
    let state = BrokerState::new(BrokerConfig {
        // Los hosts crean sesiones de forma remota; la restricción de loopback
        // es exclusiva del servidor embebido.
        require_loopback_session_creation: false,
    });

    spawn_cleanup_task(state.clone());
    let router = build_router(state);

    let port = std::env::var("TERMINAL_CANVAS_BROKER_PORT")
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(8787);
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    println!("collab broker listening on http://{addr}");
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("bind broker");
    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
    .expect("serve broker");
}

use std::collections::VecDeque;
use std::net::TcpStream;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use super::protocol::BrokerControlMessage;
use super::tls::{http_client, websocket_connector};
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{client, client_tls_with_config, Message, WebSocket};
use url::Url;

#[derive(Debug)]
pub enum TransportCommand {
    Connect {
        websocket_url: String,
        tls_cert_pem: Option<String>,
    },
    SendText(String),
    SendBinary(Vec<u8>),
    Close,
}

#[derive(Debug)]
pub enum TransportEvent {
    Connected,
    Disconnected,
    Text(String),
    Binary(Vec<u8>),
    Error(String),
}

pub struct BackgroundTransport {
    command_tx: Sender<TransportCommand>,
    event_rx: Receiver<TransportEvent>,
}

impl BackgroundTransport {
    pub fn new() -> Self {
        let (command_tx, command_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        thread::spawn(move || transport_thread(command_rx, event_tx));
        Self {
            command_tx,
            event_rx,
        }
    }

    pub fn send(&self, command: TransportCommand) {
        let _ = self.command_tx.send(command);
    }

    pub fn drain_events(&self) -> Vec<TransportEvent> {
        let mut events = Vec::new();
        while let Ok(event) = self.event_rx.try_recv() {
            events.push(event);
        }
        events
    }
}

fn transport_thread(command_rx: Receiver<TransportCommand>, event_tx: Sender<TransportEvent>) {
    let mut socket: Option<WebSocket<MaybeTlsStream<std::net::TcpStream>>> = None;
    let mut outbound = VecDeque::new();
    let mut last_ping = Instant::now();
    let mut desired_websocket_url: Option<String> = None;
    let mut desired_tls_cert_pem: Option<String> = None;
    let mut reconnect_attempts: u32 = 0;
    let mut next_reconnect_at: Option<Instant> = None;

    loop {
        loop {
            match command_rx.try_recv() {
                Ok(TransportCommand::Connect {
                    websocket_url,
                    tls_cert_pem,
                }) => {
                    desired_websocket_url = Some(websocket_url);
                    desired_tls_cert_pem = tls_cert_pem;
                    reconnect_attempts = 0;
                    next_reconnect_at = Some(Instant::now());
                    if let Some(mut active_socket) = socket.take() {
                        let _ = active_socket.close(None);
                        let _ = event_tx.send(TransportEvent::Disconnected);
                    }
                }
                Ok(TransportCommand::SendText(message)) => {
                    outbound.push_back(Message::Text(message))
                }
                Ok(TransportCommand::SendBinary(message)) => {
                    outbound.push_back(Message::Binary(message))
                }
                Ok(TransportCommand::Close) => {
                    desired_websocket_url = None;
                    desired_tls_cert_pem = None;
                    reconnect_attempts = 0;
                    next_reconnect_at = None;
                    outbound.clear();
                    if let Some(mut active_socket) = socket.take() {
                        let _ = active_socket.close(None);
                    }
                    let _ = event_tx.send(TransportEvent::Disconnected);
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            }
        }

        if socket.is_none() {
            let should_reconnect = desired_websocket_url.is_some()
                && next_reconnect_at
                    .map(|deadline| Instant::now() >= deadline)
                    .unwrap_or(true);
            if should_reconnect {
                let websocket_url = desired_websocket_url
                    .as_ref()
                    .expect("checked desired websocket url")
                    .clone();
                match connect_websocket(websocket_url.as_str(), desired_tls_cert_pem.as_deref()) {
                    Ok((mut ws, _)) => {
                        if let Err(err) = set_websocket_nonblocking(&mut ws) {
                            reconnect_attempts = reconnect_attempts.saturating_add(1);
                            next_reconnect_at =
                                Some(Instant::now() + reconnect_delay(reconnect_attempts));
                            let _ = event_tx.send(TransportEvent::Error(err.to_string()));
                            thread::sleep(Duration::from_millis(16));
                            continue;
                        }
                        socket = Some(ws);
                        reconnect_attempts = 0;
                        next_reconnect_at = None;
                        last_ping = Instant::now();
                        let _ = event_tx.send(TransportEvent::Connected);
                    }
                    Err(err) => {
                        reconnect_attempts = reconnect_attempts.saturating_add(1);
                        next_reconnect_at =
                            Some(Instant::now() + reconnect_delay(reconnect_attempts));
                        let _ = event_tx.send(TransportEvent::Error(err.to_string()));
                    }
                }
            }
        }

        if socket.is_some() {
            let mut socket_failed = false;
            let mut disconnect_error = None::<String>;
            let mut disconnect_now = false;
            let socket_ref = socket.as_mut().expect("socket checked");
            while let Some(message) = outbound.pop_front() {
                if let Err(err) = socket_ref.send(message) {
                    disconnect_error = Some(err.to_string());
                    socket_failed = true;
                    break;
                }
            }
            if socket_failed {
                if let Some(message) = disconnect_error {
                    let _ = event_tx.send(TransportEvent::Error(message));
                }
                socket = None;
                let _ = event_tx.send(TransportEvent::Disconnected);
                thread::sleep(Duration::from_millis(16));
                continue;
            }

            if last_ping.elapsed() >= Duration::from_secs(10) {
                if let Err(err) = socket_ref.send(Message::Ping(Vec::new())) {
                    disconnect_error = Some(err.to_string());
                    disconnect_now = true;
                } else {
                    last_ping = Instant::now();
                }
            }

            if !disconnect_now {
                match socket_ref.read() {
                    Ok(Message::Text(text)) => {
                        let _ = event_tx.send(TransportEvent::Text(text));
                    }
                    Ok(Message::Binary(binary)) => {
                        let _ = event_tx.send(TransportEvent::Binary(binary));
                    }
                    Ok(Message::Close(_)) => {
                        disconnect_now = true;
                    }
                    Ok(Message::Ping(payload)) => {
                        let _ = socket_ref.send(Message::Pong(payload));
                    }
                    Ok(Message::Pong(_)) => {}
                    Ok(Message::Frame(_)) => {}
                    Err(tungstenite::Error::Io(err))
                        if err.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(err) => {
                        disconnect_error = Some(err.to_string());
                        disconnect_now = true;
                    }
                }
            }

            if disconnect_now {
                if let Some(message) = disconnect_error {
                    let _ = event_tx.send(TransportEvent::Error(message));
                }
                socket = None;
                let _ = event_tx.send(TransportEvent::Disconnected);
                if desired_websocket_url.is_some() {
                    reconnect_attempts = reconnect_attempts.saturating_add(1);
                    next_reconnect_at = Some(Instant::now() + reconnect_delay(reconnect_attempts));
                }
            }
        }

        thread::sleep(Duration::from_millis(16));
    }
}

fn reconnect_delay(attempt: u32) -> Duration {
    let capped_attempt = attempt.min(5);
    Duration::from_millis(250 * (1 << capped_attempt))
}

fn set_websocket_nonblocking(
    socket: &mut WebSocket<MaybeTlsStream<TcpStream>>,
) -> std::io::Result<()> {
    match socket.get_mut() {
        MaybeTlsStream::Plain(stream) => stream.set_nonblocking(true),
        MaybeTlsStream::Rustls(stream) => stream.sock.set_nonblocking(true),
        _ => Ok(()),
    }
}

fn connect_websocket(
    websocket_url: &str,
    tls_cert_pem: Option<&str>,
) -> anyhow::Result<(
    WebSocket<MaybeTlsStream<TcpStream>>,
    tungstenite::handshake::client::Response,
)> {
    let parsed = Url::parse(websocket_url)?;
    let host = parsed
        .host_str()
        .ok_or_else(|| anyhow::anyhow!("WebSocket URL missing host"))?;
    let port = parsed
        .port_or_known_default()
        .ok_or_else(|| anyhow::anyhow!("WebSocket URL missing port"))?;
    let stream = TcpStream::connect((host, port))?;
    let connector = websocket_connector(tls_cert_pem)?;

    if parsed.scheme() == "wss" {
        Ok(client_tls_with_config(
            websocket_url,
            stream,
            None,
            connector,
        )?)
    } else {
        Ok(client(websocket_url, MaybeTlsStream::Plain(stream))?)
    }
}

pub fn json_post<T: serde::Serialize, R: serde::de::DeserializeOwned>(
    url: &str,
    body: &T,
    tls_cert_pem: Option<&str>,
) -> anyhow::Result<R> {
    let response = http_client(tls_cert_pem)?.post(url).json(body).send()?;
    let response = response.error_for_status()?;
    Ok(response.json()?)
}

pub fn broker_message_from_text(text: &str) -> Option<BrokerControlMessage> {
    serde_json::from_str(text).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::{SinkExt, StreamExt};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;
    use tokio::net::TcpListener;
    use tokio::sync::oneshot;
    use tokio_tungstenite::accept_async;

    async fn spawn_test_server(
        close_first_connection: bool,
    ) -> (String, Arc<AtomicUsize>, oneshot::Sender<()>) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test server");
        let address = listener.local_addr().expect("local addr");
        let accepted = Arc::new(AtomicUsize::new(0));
        let closed_first = Arc::new(AtomicBool::new(false));
        let (shutdown_tx, mut shutdown_rx) = oneshot::channel::<()>();
        let accepted_ref = accepted.clone();
        let closed_first_ref = closed_first.clone();

        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut shutdown_rx => break,
                    incoming = listener.accept() => {
                        let Ok((stream, _)) = incoming else { break };
                        let accepted_now = accepted_ref.fetch_add(1, Ordering::SeqCst) + 1;
                        tokio::spawn({
                            let closed_first_ref = closed_first_ref.clone();
                            async move {
                                let mut websocket = accept_async(stream).await.expect("accept websocket");
                                if close_first_connection
                                    && accepted_now == 1
                                    && !closed_first_ref.swap(true, Ordering::SeqCst)
                                {
                                    return;
                                }

                                while let Some(message) = websocket.next().await {
                                    match message {
                                        Ok(tungstenite::Message::Close(_)) | Err(_) => break,
                                        Ok(tungstenite::Message::Ping(payload)) => {
                                            let _ = websocket.send(tungstenite::Message::Pong(payload)).await;
                                        }
                                        Ok(_) => {}
                                    }
                                }
                            }
                        });
                    }
                }
            }
        });

        (format!("ws://{}", address), accepted, shutdown_tx)
    }

    async fn wait_for_event(
        transport: &BackgroundTransport,
        predicate: impl Fn(&TransportEvent) -> bool,
        timeout: Duration,
    ) -> bool {
        let start = Instant::now();
        while start.elapsed() < timeout {
            for event in transport.drain_events() {
                if predicate(&event) {
                    return true;
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        false
    }

    async fn collect_events(
        transport: &BackgroundTransport,
        timeout: Duration,
    ) -> Vec<TransportEvent> {
        let start = Instant::now();
        let mut events = Vec::new();
        while start.elapsed() < timeout {
            let drained = transport.drain_events();
            if !drained.is_empty() {
                events.extend(drained);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        events
    }

    #[tokio::test]
    async fn close_does_not_kill_transport_thread_for_future_connections() {
        let (websocket_url, accepted, shutdown_tx) = spawn_test_server(false).await;
        let transport = BackgroundTransport::new();

        transport.send(TransportCommand::Connect {
            websocket_url: websocket_url.clone(),
            tls_cert_pem: None,
        });
        let connected = wait_for_event(
            &transport,
            |event| matches!(event, TransportEvent::Connected),
            Duration::from_secs(2),
        )
        .await;
        if !connected {
            panic!(
                "missing initial connect, events: {:?}",
                collect_events(&transport, Duration::from_millis(200)).await
            );
        }

        transport.send(TransportCommand::Close);
        assert!(
            wait_for_event(
                &transport,
                |event| matches!(event, TransportEvent::Disconnected),
                Duration::from_secs(2),
            )
            .await
        );

        transport.send(TransportCommand::Connect {
            websocket_url,
            tls_cert_pem: None,
        });
        let reconnected = wait_for_event(
            &transport,
            |event| matches!(event, TransportEvent::Connected),
            Duration::from_secs(2),
        )
        .await;
        if !reconnected {
            panic!(
                "missing reconnect, events: {:?}",
                collect_events(&transport, Duration::from_millis(200)).await
            );
        }
        assert!(accepted.load(Ordering::SeqCst) >= 2);

        let _ = shutdown_tx.send(());
    }

    #[tokio::test]
    async fn unexpected_disconnect_triggers_reconnect() {
        let (websocket_url, accepted, shutdown_tx) = spawn_test_server(true).await;
        let transport = BackgroundTransport::new();

        transport.send(TransportCommand::Connect {
            websocket_url,
            tls_cert_pem: None,
        });
        assert!(
            wait_for_event(
                &transport,
                |event| matches!(event, TransportEvent::Connected),
                Duration::from_secs(2),
            )
            .await
        );
        assert!(
            wait_for_event(
                &transport,
                |event| matches!(event, TransportEvent::Connected),
                Duration::from_secs(5),
            )
            .await
        );
        assert!(accepted.load(Ordering::SeqCst) >= 2);

        transport.send(TransportCommand::Close);
        let _ = shutdown_tx.send(());
    }
}

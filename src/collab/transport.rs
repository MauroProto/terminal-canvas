use std::collections::VecDeque;
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TryRecvError, TrySendError};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use super::protocol::BrokerControlMessage;
use super::tls::{http_client, websocket_connector};
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{client, client_tls_with_config, Message, WebSocket};
use url::Url;

const COMMAND_WAIT_SLICE: Duration = Duration::from_millis(50);
const SOCKET_IO_TIMEOUT: Duration = Duration::from_millis(50);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const COMMAND_QUEUE_CAPACITY: usize = 32;
const EVENT_QUEUE_CAPACITY: usize = 64;
const MAX_BINARY_MESSAGE_BYTES: usize = 16 * 1024 * 1024;
const MAX_QUEUED_BINARY_BYTES: usize = 32 * 1024 * 1024;

#[derive(Debug)]
pub enum TransportCommand {
    Connect {
        websocket_url: String,
        tls_cert_pem: Option<String>,
    },
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
    command_tx: SyncSender<(u64, TransportCommand)>,
    event_rx: Receiver<TransportEvent>,
    queued_command_bytes: Arc<AtomicUsize>,
    queued_event_bytes: Arc<AtomicUsize>,
    close_requested: Arc<AtomicBool>,
    event_overflowed: Arc<AtomicBool>,
    generation: Arc<AtomicU64>,
}

impl BackgroundTransport {
    pub fn new() -> Self {
        let (command_tx, command_rx) =
            mpsc::sync_channel::<(u64, TransportCommand)>(COMMAND_QUEUE_CAPACITY);
        let (event_tx, event_rx) = mpsc::sync_channel(EVENT_QUEUE_CAPACITY);
        let queued_command_bytes = Arc::new(AtomicUsize::new(0));
        let queued_event_bytes = Arc::new(AtomicUsize::new(0));
        let close_requested = Arc::new(AtomicBool::new(false));
        let event_overflowed = Arc::new(AtomicBool::new(false));
        let generation = Arc::new(AtomicU64::new(0));
        let worker_command_bytes = queued_command_bytes.clone();
        let worker_event_bytes = queued_event_bytes.clone();
        let worker_close_requested = close_requested.clone();
        let worker_event_overflowed = event_overflowed.clone();
        let worker_generation = generation.clone();
        thread::spawn(move || {
            transport_thread(
                command_rx,
                event_tx,
                worker_command_bytes,
                worker_event_bytes,
                worker_close_requested,
                worker_event_overflowed,
                worker_generation,
            )
        });
        Self {
            command_tx,
            event_rx,
            queued_command_bytes,
            queued_event_bytes,
            close_requested,
            event_overflowed,
            generation,
        }
    }

    /// Encola sin bloquear el hilo UI. Devuelve `false` cuando el transporte
    /// aplica backpressure y el caller debe conservar/reintentar estado.
    pub fn send(&self, command: TransportCommand) -> bool {
        if matches!(command, TransportCommand::Close) {
            self.generation.fetch_add(1, Ordering::AcqRel);
            self.close_requested.store(true, Ordering::Release);
            return true;
        }
        let generation = self.generation.load(Ordering::Acquire);
        let binary_bytes = match &command {
            TransportCommand::SendBinary(bytes) => bytes.len(),
            _ => 0,
        };
        if binary_bytes > MAX_BINARY_MESSAGE_BYTES
            || (binary_bytes > 0
                && !reserve_bytes(
                    &self.queued_command_bytes,
                    binary_bytes,
                    MAX_QUEUED_BINARY_BYTES,
                ))
        {
            return false;
        }
        match self.command_tx.try_send((generation, command)) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
                release_bytes(&self.queued_command_bytes, binary_bytes);
                false
            }
        }
    }

    pub fn drain_events(&self) -> Vec<TransportEvent> {
        let mut events = Vec::new();
        while let Ok(event) = self.event_rx.try_recv() {
            release_bytes(&self.queued_event_bytes, event_payload_bytes(&event));
            events.push(event);
        }
        if self.event_overflowed.swap(false, Ordering::AcqRel) {
            events.push(TransportEvent::Error(
                "Collaboration transport overflowed its bounded event queue".to_owned(),
            ));
            events.push(TransportEvent::Disconnected);
        }
        events
    }
}

fn transport_thread(
    command_rx: Receiver<(u64, TransportCommand)>,
    event_tx: SyncSender<TransportEvent>,
    queued_command_bytes: Arc<AtomicUsize>,
    queued_event_bytes: Arc<AtomicUsize>,
    close_requested: Arc<AtomicBool>,
    event_overflowed: Arc<AtomicBool>,
    generation: Arc<AtomicU64>,
) {
    let mut socket: Option<WebSocket<MaybeTlsStream<std::net::TcpStream>>> = None;
    let mut outbound = VecDeque::<Vec<u8>>::new();
    let mut last_ping = Instant::now();
    let mut desired_websocket_url: Option<String> = None;
    let mut desired_tls_cert_pem: Option<String> = None;
    let mut reconnect_attempts: u32 = 0;
    let mut next_reconnect_at: Option<Instant> = None;

    loop {
        if close_requested.swap(false, Ordering::AcqRel) {
            handle_command(
                TransportCommand::Close,
                &event_tx,
                &queued_event_bytes,
                &event_overflowed,
                &mut socket,
                &mut outbound,
                &mut desired_websocket_url,
                &mut desired_tls_cert_pem,
                &mut reconnect_attempts,
                &mut next_reconnect_at,
            );
        }
        if socket.is_none() {
            if !wait_for_commands_or_reconnect(
                &command_rx,
                &event_tx,
                &queued_command_bytes,
                &queued_event_bytes,
                &event_overflowed,
                &close_requested,
                &generation,
                &mut socket,
                &mut outbound,
                &mut desired_websocket_url,
                &mut desired_tls_cert_pem,
                &mut reconnect_attempts,
                &mut next_reconnect_at,
            ) {
                return;
            }
            if socket.is_none() && desired_websocket_url.is_none() {
                continue;
            }
        }

        loop {
            match command_rx.try_recv() {
                Ok((command_generation, command)) => {
                    release_bytes(&queued_command_bytes, command_payload_bytes(&command));
                    if command_generation != generation.load(Ordering::Acquire) {
                        continue;
                    }
                    handle_command(
                        command,
                        &event_tx,
                        &queued_event_bytes,
                        &event_overflowed,
                        &mut socket,
                        &mut outbound,
                        &mut desired_websocket_url,
                        &mut desired_tls_cert_pem,
                        &mut reconnect_attempts,
                        &mut next_reconnect_at,
                    )
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
                        if close_requested.load(Ordering::Acquire) {
                            let _ = ws.close(None);
                            continue;
                        }
                        if let Err(err) = set_websocket_timeouts(&mut ws) {
                            reconnect_attempts = reconnect_attempts.saturating_add(1);
                            next_reconnect_at =
                                Some(Instant::now() + reconnect_delay(reconnect_attempts));
                            emit_event(
                                &event_tx,
                                &queued_event_bytes,
                                &event_overflowed,
                                TransportEvent::Error(err.to_string()),
                            );
                            continue;
                        }
                        socket = Some(ws);
                        reconnect_attempts = 0;
                        next_reconnect_at = None;
                        last_ping = Instant::now();
                        emit_event(
                            &event_tx,
                            &queued_event_bytes,
                            &event_overflowed,
                            TransportEvent::Connected,
                        );
                    }
                    Err(err) => {
                        reconnect_attempts = reconnect_attempts.saturating_add(1);
                        next_reconnect_at =
                            Some(Instant::now() + reconnect_delay(reconnect_attempts));
                        emit_event(
                            &event_tx,
                            &queued_event_bytes,
                            &event_overflowed,
                            TransportEvent::Error(err.to_string()),
                        );
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
                if let Err(err) = socket_ref.send(Message::Binary(message)) {
                    disconnect_error = Some(err.to_string());
                    socket_failed = true;
                    break;
                }
            }
            if socket_failed {
                if let Some(message) = disconnect_error {
                    emit_event(
                        &event_tx,
                        &queued_event_bytes,
                        &event_overflowed,
                        TransportEvent::Error(message),
                    );
                }
                socket = None;
                emit_event(
                    &event_tx,
                    &queued_event_bytes,
                    &event_overflowed,
                    TransportEvent::Disconnected,
                );
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
                        if !emit_event(
                            &event_tx,
                            &queued_event_bytes,
                            &event_overflowed,
                            TransportEvent::Text(text),
                        ) {
                            disconnect_now = true;
                        }
                    }
                    Ok(Message::Binary(binary)) => {
                        if !emit_event(
                            &event_tx,
                            &queued_event_bytes,
                            &event_overflowed,
                            TransportEvent::Binary(binary),
                        ) {
                            disconnect_now = true;
                        }
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
                        if err.kind() == std::io::ErrorKind::WouldBlock
                            || err.kind() == std::io::ErrorKind::TimedOut => {}
                    Err(err) => {
                        disconnect_error = Some(err.to_string());
                        disconnect_now = true;
                    }
                }
            }

            if disconnect_now {
                if let Some(message) = disconnect_error {
                    emit_event(
                        &event_tx,
                        &queued_event_bytes,
                        &event_overflowed,
                        TransportEvent::Error(message),
                    );
                }
                socket = None;
                emit_event(
                    &event_tx,
                    &queued_event_bytes,
                    &event_overflowed,
                    TransportEvent::Disconnected,
                );
                if desired_websocket_url.is_some() {
                    reconnect_attempts = reconnect_attempts.saturating_add(1);
                    next_reconnect_at = Some(Instant::now() + reconnect_delay(reconnect_attempts));
                }
            }
        }
    }
}

fn reconnect_delay(attempt: u32) -> Duration {
    let capped_attempt = attempt.min(5);
    Duration::from_millis(250 * (1 << capped_attempt))
}

fn set_websocket_timeouts(
    socket: &mut WebSocket<MaybeTlsStream<TcpStream>>,
) -> std::io::Result<()> {
    match socket.get_mut() {
        MaybeTlsStream::Plain(stream) => {
            stream.set_read_timeout(Some(SOCKET_IO_TIMEOUT))?;
            stream.set_write_timeout(Some(SOCKET_IO_TIMEOUT))
        }
        MaybeTlsStream::Rustls(stream) => {
            stream.sock.set_read_timeout(Some(SOCKET_IO_TIMEOUT))?;
            stream.sock.set_write_timeout(Some(SOCKET_IO_TIMEOUT))
        }
        _ => Ok(()),
    }
}

fn wait_for_commands_or_reconnect(
    command_rx: &Receiver<(u64, TransportCommand)>,
    event_tx: &SyncSender<TransportEvent>,
    queued_command_bytes: &AtomicUsize,
    queued_event_bytes: &AtomicUsize,
    event_overflowed: &AtomicBool,
    close_requested: &AtomicBool,
    generation: &AtomicU64,
    socket: &mut Option<WebSocket<MaybeTlsStream<TcpStream>>>,
    outbound: &mut VecDeque<Vec<u8>>,
    desired_websocket_url: &mut Option<String>,
    desired_tls_cert_pem: &mut Option<String>,
    reconnect_attempts: &mut u32,
    next_reconnect_at: &mut Option<Instant>,
) -> bool {
    let wait_duration = if desired_websocket_url.is_some() {
        next_reconnect_at
            .map(|deadline| deadline.saturating_duration_since(Instant::now()))
            .unwrap_or(Duration::ZERO)
            .min(COMMAND_WAIT_SLICE)
    } else {
        COMMAND_WAIT_SLICE
    };

    let command = if desired_websocket_url.is_none() {
        match command_rx.recv() {
            Ok(command) => Some(command),
            Err(_) => return false,
        }
    } else {
        match command_rx.recv_timeout(wait_duration) {
            Ok(command) => Some(command),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => return false,
        }
    };

    if close_requested.swap(false, Ordering::AcqRel) {
        handle_command(
            TransportCommand::Close,
            event_tx,
            queued_event_bytes,
            event_overflowed,
            socket,
            outbound,
            desired_websocket_url,
            desired_tls_cert_pem,
            reconnect_attempts,
            next_reconnect_at,
        );
    }
    if let Some((command_generation, command)) = command {
        release_bytes(queued_command_bytes, command_payload_bytes(&command));
        if command_generation != generation.load(Ordering::Acquire) {
            return true;
        }
        handle_command(
            command,
            event_tx,
            queued_event_bytes,
            event_overflowed,
            socket,
            outbound,
            desired_websocket_url,
            desired_tls_cert_pem,
            reconnect_attempts,
            next_reconnect_at,
        );
    }
    true
}

fn handle_command(
    command: TransportCommand,
    event_tx: &SyncSender<TransportEvent>,
    queued_event_bytes: &AtomicUsize,
    event_overflowed: &AtomicBool,
    socket: &mut Option<WebSocket<MaybeTlsStream<TcpStream>>>,
    outbound: &mut VecDeque<Vec<u8>>,
    desired_websocket_url: &mut Option<String>,
    desired_tls_cert_pem: &mut Option<String>,
    reconnect_attempts: &mut u32,
    next_reconnect_at: &mut Option<Instant>,
) {
    match command {
        TransportCommand::Connect {
            websocket_url,
            tls_cert_pem,
        } => {
            *desired_websocket_url = Some(websocket_url);
            *desired_tls_cert_pem = tls_cert_pem;
            *reconnect_attempts = 0;
            *next_reconnect_at = Some(Instant::now());
            if let Some(mut active_socket) = socket.take() {
                let _ = active_socket.close(None);
                emit_event(
                    event_tx,
                    queued_event_bytes,
                    event_overflowed,
                    TransportEvent::Disconnected,
                );
            }
        }
        TransportCommand::SendBinary(message) => {
            let dropped = enqueue_outbound(outbound, message);
            if dropped {
                emit_event(
                    event_tx,
                    queued_event_bytes,
                    event_overflowed,
                    TransportEvent::Error(
                        "Collaboration transport dropped stale outbound data under backpressure"
                            .to_owned(),
                    ),
                );
            }
        }
        TransportCommand::Close => {
            *desired_websocket_url = None;
            *desired_tls_cert_pem = None;
            *reconnect_attempts = 0;
            *next_reconnect_at = None;
            outbound.clear();
            if let Some(mut active_socket) = socket.take() {
                let _ = active_socket.close(None);
            }
            emit_event(
                event_tx,
                queued_event_bytes,
                event_overflowed,
                TransportEvent::Disconnected,
            );
        }
    }
}

fn enqueue_outbound(outbound: &mut VecDeque<Vec<u8>>, message: Vec<u8>) -> bool {
    if message.len() > MAX_BINARY_MESSAGE_BYTES {
        return true;
    }
    let mut queued_bytes = outbound.iter().map(Vec::len).sum::<usize>();
    let mut dropped = false;
    while !outbound.is_empty()
        && queued_bytes.saturating_add(message.len()) > MAX_QUEUED_BINARY_BYTES
    {
        if let Some(stale) = outbound.pop_front() {
            queued_bytes = queued_bytes.saturating_sub(stale.len());
            dropped = true;
        }
    }
    outbound.push_back(message);
    dropped
}

fn command_payload_bytes(command: &TransportCommand) -> usize {
    match command {
        TransportCommand::SendBinary(bytes) => bytes.len(),
        _ => 0,
    }
}

fn event_payload_bytes(event: &TransportEvent) -> usize {
    match event {
        TransportEvent::Text(text) | TransportEvent::Error(text) => text.len(),
        TransportEvent::Binary(bytes) => bytes.len(),
        TransportEvent::Connected | TransportEvent::Disconnected => 0,
    }
}

fn reserve_bytes(counter: &AtomicUsize, bytes: usize, maximum: usize) -> bool {
    if bytes == 0 {
        return true;
    }
    counter
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
            current.checked_add(bytes).filter(|next| *next <= maximum)
        })
        .is_ok()
}

fn release_bytes(counter: &AtomicUsize, bytes: usize) {
    if bytes == 0 {
        return;
    }
    let _ = counter.fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
        Some(current.saturating_sub(bytes))
    });
}

fn emit_event(
    event_tx: &SyncSender<TransportEvent>,
    queued_event_bytes: &AtomicUsize,
    event_overflowed: &AtomicBool,
    event: TransportEvent,
) -> bool {
    let bytes = event_payload_bytes(&event);
    if bytes > MAX_BINARY_MESSAGE_BYTES
        || !reserve_bytes(queued_event_bytes, bytes, MAX_QUEUED_BINARY_BYTES)
    {
        event_overflowed.store(true, Ordering::Release);
        return false;
    }
    match event_tx.try_send(event) {
        Ok(()) => true,
        Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
            release_bytes(queued_event_bytes, bytes);
            event_overflowed.store(true, Ordering::Release);
            false
        }
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
    let addresses = (host, port).to_socket_addrs()?.collect::<Vec<_>>();
    let mut last_error = None;
    let mut stream = None;
    for address in addresses {
        match TcpStream::connect_timeout(&address, CONNECT_TIMEOUT) {
            Ok(connected) => {
                stream = Some(connected);
                break;
            }
            Err(error) => last_error = Some(error),
        }
    }
    let stream = stream.ok_or_else(|| {
        last_error.unwrap_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::AddrNotAvailable,
                "WebSocket host resolved to no addresses",
            )
        })
    })?;
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

    #[test]
    fn outbound_queue_sheds_oldest_messages_and_stays_within_its_byte_budget() {
        let mut outbound = VecDeque::new();
        let half_budget = MAX_QUEUED_BINARY_BYTES / 2;
        assert!(!enqueue_outbound(&mut outbound, vec![1; half_budget]));
        assert!(!enqueue_outbound(&mut outbound, vec![2; half_budget]));

        assert!(enqueue_outbound(&mut outbound, vec![3; 1]));
        assert!(outbound.iter().map(Vec::len).sum::<usize>() <= MAX_QUEUED_BINARY_BYTES);
        assert_eq!(outbound.back().and_then(|bytes| bytes.first()), Some(&3));
    }

    #[test]
    fn event_queue_reports_backpressure_without_blocking() {
        let (tx, _rx) = mpsc::sync_channel(1);
        let queued_bytes = AtomicUsize::new(0);
        let overflowed = AtomicBool::new(false);

        assert!(emit_event(
            &tx,
            &queued_bytes,
            &overflowed,
            TransportEvent::Connected,
        ));
        assert!(!emit_event(
            &tx,
            &queued_bytes,
            &overflowed,
            TransportEvent::Disconnected,
        ));
        assert!(overflowed.load(Ordering::Acquire));
    }

    #[test]
    fn transport_rejects_an_oversized_binary_before_it_reaches_the_worker() {
        let transport = BackgroundTransport::new();
        assert!(!transport.send(TransportCommand::SendBinary(vec![
            0;
            MAX_BINARY_MESSAGE_BYTES
                + 1
        ])));
        assert_eq!(transport.queued_command_bytes.load(Ordering::Acquire), 0);
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
    async fn an_immediate_close_does_not_discard_a_newer_connect_request() {
        let (websocket_url, accepted, shutdown_tx) = spawn_test_server(false).await;
        let transport = BackgroundTransport::new();

        assert!(transport.send(TransportCommand::Connect {
            websocket_url: websocket_url.clone(),
            tls_cert_pem: None,
        }));
        assert!(transport.send(TransportCommand::Close));
        assert!(transport.send(TransportCommand::Connect {
            websocket_url,
            tls_cert_pem: None,
        }));

        assert!(
            wait_for_event(
                &transport,
                |event| matches!(event, TransportEvent::Connected),
                Duration::from_secs(3),
            )
            .await
        );
        assert!(accepted.load(Ordering::SeqCst) >= 1);

        transport.send(TransportCommand::Close);
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

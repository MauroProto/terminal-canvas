//! Sesión de terminal hosteada por el daemon, vista desde la app
//! (P3.15, T3).
//!
//! La idea que hace esto barato: el grid se sigue parseando en la app, igual
//! que con un PTY local. Lo único que cambia es de dónde salen los bytes (del
//! socket en vez del fd) y a dónde van (`Write` en vez del writer del PTY).
//! Así el render, la selección, el scrollback y la búsqueda no se enteran.

use std::io::{BufReader, Write};
use std::os::unix::net::UnixStream;
use std::sync::Arc;

use uuid::Uuid;

use crate::daemon::protocol::{
    decode_line, encode_line, read_protocol_line, Request, Response, MAX_WIRE_INPUT_BYTES,
};

/// Enlace con la sesión del daemon: por acá salen resize y kill, que no pasan
/// por el stream de bytes.
#[derive(Clone)]
pub struct RemoteLink {
    session_id: Uuid,
    control: Result<super::pty::InputWriter, Arc<String>>,
    disconnect: Option<Arc<UnixStream>>,
    killing: Arc<std::sync::atomic::AtomicBool>,
}

struct SocketWriter(UnixStream);
impl Write for SocketWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.write(bytes)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}
impl Drop for SocketWriter {
    fn drop(&mut self) {
        let _ = self.0.shutdown(std::net::Shutdown::Both);
    }
}

impl RemoteLink {
    pub fn new(session_id: Uuid, control: UnixStream) -> Self {
        let _ = control.set_write_timeout(Some(std::time::Duration::from_secs(5)));
        Self {
            session_id,
            disconnect: control.try_clone().ok().map(Arc::new),
            killing: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            control: super::pty::InputWriter::new(Box::new(SocketWriter(control)))
                .map_err(|error| Arc::new(error.to_string())),
        }
    }

    pub fn session_id(&self) -> Uuid {
        self.session_id
    }

    fn send(&self, request: &Request) -> std::io::Result<()> {
        let control = self
            .control
            .as_ref()
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        control.enqueue(encode_line(request).as_bytes())
    }

    pub fn resize(&self, cols: u16, rows: u16) {
        let _ = self.send(&Request::Resize {
            id: self.session_id,
            cols,
            rows,
        });
    }

    pub fn kill(&self) {
        if let Ok(writer) = &self.control {
            if writer
                .enqueue_final(
                    encode_line(&Request::Kill {
                        id: self.session_id,
                    })
                    .as_bytes(),
                )
                .is_ok()
            {
                self.killing
                    .store(true, std::sync::atomic::Ordering::Release);
            }
        }
    }

    /// Disconnect this UI without terminating the daemon's PTY. This clone
    /// is independent of the writer mutex, so it also cancels a stuck write.
    pub fn disconnect(&self) {
        if self.killing.load(std::sync::atomic::Ordering::Acquire) {
            return;
        }
        if let Ok(writer) = &self.control {
            writer.close();
        }
        if let Some(stream) = &self.disconnect {
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
    }

    pub fn input_error(&self) -> Option<String> {
        match &self.control {
            Ok(writer) => writer.error(),
            Err(error) => Some(error.to_string()),
        }
    }
}

/// `Write` que enmarca lo que la app escribe como `Request::Write` NDJSON.
/// Se usa tal cual en el lugar del writer del PTY local.
pub struct RemoteWriter {
    link: RemoteLink,
}

impl RemoteWriter {
    pub fn new(link: RemoteLink) -> Self {
        Self { link }
    }
}

impl Write for RemoteWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        for chunk in buf.chunks(MAX_WIRE_INPUT_BYTES) {
            self.link.send(&Request::Write {
                id: self.link.session_id(),
                data: chunk.to_vec(),
            })?;
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Bytes de salida de **una** sesión, leídos del socket. Filtra los eventos de
/// la sesión adjunta y descarta los que ya venían en el snapshot del attach
/// (dedup por `seq`, T4).
pub struct RemoteReader {
    lines: BufReader<UnixStream>,
    session_id: Uuid,
    attached_seq: u64,
    exited: bool,
    input_error: Option<String>,
}

impl RemoteReader {
    pub fn new(stream: UnixStream, session_id: Uuid, attached_seq: u64) -> Self {
        Self::from_buffered(BufReader::new(stream), session_id, attached_seq)
    }

    /// Preserve bytes read ahead while receiving the attach response.
    pub fn from_buffered(
        lines: BufReader<UnixStream>,
        session_id: Uuid,
        attached_seq: u64,
    ) -> Self {
        Self {
            lines,
            session_id,
            attached_seq,
            exited: false,
            input_error: None,
        }
    }

    /// ¿La sesión terminó del otro lado?
    pub fn exited(&self) -> bool {
        self.exited
    }

    pub fn take_input_error(&mut self) -> Option<String> {
        self.input_error.take()
    }

    /// Próximo bloque de salida de esta sesión. `None` cuando el socket cierra
    /// o la sesión terminó.
    pub fn next_output(&mut self) -> Option<Vec<u8>> {
        loop {
            let line = match read_protocol_line(&mut self.lines) {
                Ok(Some(line)) => line,
                Ok(None) | Err(_) => return None,
            };
            match decode_line::<Response>(&line) {
                Some(Response::InputError { id, message }) if id == self.session_id => {
                    self.input_error = Some(message);
                    return Some(Vec::new());
                }
                Some(Response::Output { id, seq, data })
                    if id == self.session_id && seq > self.attached_seq =>
                {
                    return Some(data);
                }
                Some(Response::Exit { id } | Response::Killed { id }) if id == self.session_id => {
                    self.exited = true;
                    return None;
                }
                // Evento de otra sesión, respuesta a un pedido, o línea
                // corrupta: se ignora y se sigue leyendo.
                _ => continue,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{RemoteLink, RemoteReader, RemoteWriter};
    use crate::daemon::protocol::{
        decode_line, encode_line, Request, Response, MAX_WIRE_INPUT_BYTES,
    };
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;
    use uuid::Uuid;

    /// Par de sockets conectados, para probar sin daemon.
    fn socket_pair() -> (UnixStream, UnixStream) {
        UnixStream::pair().expect("socketpair")
    }

    #[test]
    fn the_writer_frames_bytes_as_a_write_request() {
        let (ours, theirs) = socket_pair();
        let id = Uuid::new_v4();
        let mut writer = RemoteWriter::new(RemoteLink::new(id, ours));

        writer.write_all(b"echo hola\n").expect("escribe");

        let mut reader = BufReader::new(theirs);
        let mut line = String::new();
        reader.read_line(&mut line).expect("lee");
        match decode_line::<Request>(&line) {
            Some(Request::Write { id: got, data }) => {
                assert_eq!(got, id);
                assert_eq!(data, b"echo hola\n");
            }
            other => panic!("esperaba Write, got {other:?} / {line}"),
        }
    }

    #[test]
    fn a_large_write_is_split_into_bounded_protocol_messages() {
        let (ours, theirs) = socket_pair();
        let id = Uuid::new_v4();
        let mut writer = RemoteWriter::new(RemoteLink::new(id, ours));
        let payload = vec![b'x'; MAX_WIRE_INPUT_BYTES + 17];
        let payload_for_writer = payload.clone();
        let writer_thread = std::thread::spawn(move || {
            writer.write_all(&payload_for_writer).expect("escribe todo");
        });

        let mut reader = BufReader::new(theirs);
        let mut rebuilt = Vec::new();
        for expected_len in [MAX_WIRE_INPUT_BYTES, 17] {
            let mut line = String::new();
            reader.read_line(&mut line).expect("lee chunk");
            match decode_line::<Request>(&line) {
                Some(Request::Write { id: got, data }) => {
                    assert_eq!(got, id);
                    assert_eq!(data.len(), expected_len);
                    rebuilt.extend_from_slice(&data);
                }
                other => panic!("esperaba Write, got {other:?}"),
            }
        }
        writer_thread
            .join()
            .expect("writer no debe entrar en pánico");
        assert_eq!(rebuilt, payload);
    }

    #[test]
    fn resize_and_kill_travel_through_the_control_channel() {
        let (ours, theirs) = socket_pair();
        let id = Uuid::new_v4();
        let link = RemoteLink::new(id, ours);
        link.resize(120, 40);
        link.kill();

        let mut reader = BufReader::new(theirs);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        assert!(
            matches!(
                decode_line::<Request>(&line),
                Some(Request::Resize {
                    cols: 120,
                    rows: 40,
                    ..
                })
            ),
            "got {line}"
        );
        line.clear();
        reader.read_line(&mut line).unwrap();
        assert!(
            matches!(decode_line::<Request>(&line), Some(Request::Kill { .. })),
            "got {line}"
        );
    }

    #[test]
    fn the_reader_only_yields_output_of_its_own_session() {
        let (ours, mut theirs) = socket_pair();
        let mine = Uuid::new_v4();
        let other = Uuid::new_v4();

        for event in [
            Response::Output {
                id: other,
                seq: 1,
                data: b"de otra".to_vec(),
            },
            Response::Sessions { ids: vec![mine] },
            Response::Output {
                id: mine,
                seq: 1,
                data: b"mia".to_vec(),
            },
        ] {
            theirs.write_all(encode_line(&event).as_bytes()).unwrap();
        }
        drop(theirs);

        let mut reader = RemoteReader::new(ours, mine, 0);
        assert_eq!(reader.next_output(), Some(b"mia".to_vec()));
        assert_eq!(reader.next_output(), None, "no hay más");
    }

    #[test]
    fn the_reader_drops_events_already_in_the_attach_snapshot() {
        let (ours, mut theirs) = socket_pair();
        let id = Uuid::new_v4();
        for seq in 1..=5 {
            theirs
                .write_all(
                    encode_line(&Response::Output {
                        id,
                        seq,
                        data: format!("linea{seq}").into_bytes(),
                    })
                    .as_bytes(),
                )
                .unwrap();
        }
        drop(theirs);

        // El attach devolvió hasta el seq 3: solo 4 y 5 son nuevos (T4).
        let mut reader = RemoteReader::new(ours, id, 3);
        assert_eq!(reader.next_output(), Some(b"linea4".to_vec()));
        assert_eq!(reader.next_output(), Some(b"linea5".to_vec()));
        assert_eq!(reader.next_output(), None);
    }

    #[test]
    fn an_exit_event_ends_the_stream() {
        let (ours, mut theirs) = socket_pair();
        let id = Uuid::new_v4();
        theirs
            .write_all(
                encode_line(&Response::Output {
                    id,
                    seq: 1,
                    data: b"x".to_vec(),
                })
                .as_bytes(),
            )
            .unwrap();
        theirs
            .write_all(encode_line(&Response::Exit { id }).as_bytes())
            .unwrap();
        // El socket queda abierto: lo que corta el stream es el Exit.
        let mut reader = RemoteReader::new(ours, id, 0);
        assert_eq!(reader.next_output(), Some(b"x".to_vec()));
        assert_eq!(reader.next_output(), None);
        assert!(reader.exited(), "tiene que quedar marcada como terminada");
    }

    #[test]
    fn a_corrupt_line_does_not_break_the_stream() {
        let (ours, mut theirs) = socket_pair();
        let id = Uuid::new_v4();
        theirs.write_all(b"no soy json\n").unwrap();
        theirs
            .write_all(
                encode_line(&Response::Output {
                    id,
                    seq: 1,
                    data: b"ok".to_vec(),
                })
                .as_bytes(),
            )
            .unwrap();
        drop(theirs);

        let mut reader = RemoteReader::new(ours, id, 0);
        assert_eq!(reader.next_output(), Some(b"ok".to_vec()));
    }

    #[test]
    fn handoff_keeps_output_buffered_after_the_attach_response() {
        let (ours, mut theirs) = socket_pair();
        let id = Uuid::new_v4();
        let messages = [
            encode_line(&Response::Attached {
                id,
                snapshot: Vec::new(),
                seq: 1,
                alive: true,
            }),
            encode_line(&Response::Output {
                id,
                seq: 2,
                data: b"after".to_vec(),
            }),
        ]
        .concat();
        theirs.write_all(messages.as_bytes()).unwrap();
        drop(theirs);
        let mut buffered = BufReader::new(ours);
        let first = crate::daemon::protocol::read_protocol_line(&mut buffered)
            .unwrap()
            .unwrap();
        assert!(matches!(
            decode_line::<Response>(&first),
            Some(Response::Attached { .. })
        ));
        let mut reader = RemoteReader::from_buffered(buffered, id, 1);
        assert_eq!(reader.next_output(), Some(b"after".to_vec()));
        assert_eq!(reader.next_output(), None);
    }

    #[test]
    fn disconnect_releases_the_reader_without_killing_a_session() {
        let (ours, _theirs) = socket_pair();
        let id = Uuid::new_v4();
        let link = RemoteLink::new(id, ours.try_clone().unwrap());
        let mut reader = RemoteReader::new(ours, id, 0);
        link.disconnect();
        assert_eq!(reader.next_output(), None);
        assert!(!reader.exited(), "local detach is not process exit");
    }
}

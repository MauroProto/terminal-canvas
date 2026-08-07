//! Sesión de terminal hosteada por el daemon, vista desde la app
//! (P3.15, T3).
//!
//! La idea que hace esto barato: el grid se sigue parseando en la app, igual
//! que con un PTY local. Lo único que cambia es de dónde salen los bytes (del
//! socket en vez del fd) y a dónde van (`Write` en vez del writer del PTY).
//! Así el render, la selección, el scrollback y la búsqueda no se enteran.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};

use uuid::Uuid;

use crate::daemon::protocol::{decode_line, encode_line, Request, Response};

/// Enlace con la sesión del daemon: por acá salen resize y kill, que no pasan
/// por el stream de bytes.
#[derive(Clone)]
pub struct RemoteLink {
    session_id: Uuid,
    control: Arc<Mutex<UnixStream>>,
}

impl RemoteLink {
    pub fn new(session_id: Uuid, control: UnixStream) -> Self {
        Self {
            session_id,
            control: Arc::new(Mutex::new(control)),
        }
    }

    pub fn session_id(&self) -> Uuid {
        self.session_id
    }

    fn send(&self, request: &Request) {
        if let Ok(mut control) = self.control.lock() {
            let _ = control.write_all(encode_line(request).as_bytes());
            let _ = control.flush();
        }
    }

    pub fn resize(&self, cols: u16, rows: u16) {
        self.send(&Request::Resize {
            id: self.session_id,
            cols,
            rows,
        });
    }

    pub fn kill(&self) {
        self.send(&Request::Kill {
            id: self.session_id,
        });
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
        // El protocolo es JSON, así que los bytes viajan como texto. Lo que no
        // es UTF-8 válido se reemplaza en vez de descartar la escritura: es lo
        // mismo que hace el terminal al mostrarlo.
        let data = String::from_utf8_lossy(buf).into_owned();
        self.link.send(&Request::Write {
            id: self.link.session_id(),
            data,
        });
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Bytes de salida de **una** sesión, leídos del socket. Filtra los eventos de
/// las otras sesiones (el daemon difunde todo a todos los clientes) y descarta
/// los que ya venían en el snapshot del attach (dedup por `seq`, T4).
pub struct RemoteReader {
    lines: BufReader<UnixStream>,
    session_id: Uuid,
    attached_seq: u64,
    exited: bool,
}

impl RemoteReader {
    pub fn new(stream: UnixStream, session_id: Uuid, attached_seq: u64) -> Self {
        Self {
            lines: BufReader::new(stream),
            session_id,
            attached_seq,
            exited: false,
        }
    }

    /// ¿La sesión terminó del otro lado?
    pub fn exited(&self) -> bool {
        self.exited
    }

    /// Próximo bloque de salida de esta sesión. `None` cuando el socket cierra
    /// o la sesión terminó.
    pub fn next_output(&mut self) -> Option<Vec<u8>> {
        loop {
            let mut line = String::new();
            match self.lines.read_line(&mut line) {
                Ok(0) | Err(_) => return None,
                Ok(_) => {}
            }
            match decode_line::<Response>(&line) {
                Some(Response::Output { id, seq, data })
                    if id == self.session_id && seq > self.attached_seq =>
                {
                    return Some(data.into_bytes());
                }
                Some(Response::Exit { id }) if id == self.session_id => {
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
    use crate::daemon::protocol::{decode_line, encode_line, Request, Response};
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
                assert_eq!(data, "echo hola\n");
            }
            other => panic!("esperaba Write, got {other:?} / {line}"),
        }
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
                data: "de otra".to_owned(),
            },
            Response::Sessions { ids: vec![mine] },
            Response::Output {
                id: mine,
                seq: 1,
                data: "mia".to_owned(),
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
                        data: format!("linea{seq}"),
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
                    data: "x".to_owned(),
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
                    data: "ok".to_owned(),
                })
                .as_bytes(),
            )
            .unwrap();
        drop(theirs);

        let mut reader = RemoteReader::new(ours, id, 0);
        assert_eq!(reader.next_output(), Some(b"ok".to_vec()));
    }
}

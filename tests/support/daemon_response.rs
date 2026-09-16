//! Test-only NDJSON reader. Retry transient socket errors without losing a prefix.
use std::io::{self, BufRead, BufReader};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use mi_terminal::daemon::protocol::{Response, MAX_PROTOCOL_LINE_BYTES};

pub fn read_before(reader: &mut BufReader<UnixStream>, deadline: Instant) -> io::Result<Response> {
    let mut line = Vec::new();
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "response deadline expired",
            ));
        }
        reader
            .get_ref()
            .set_read_timeout(Some(remaining.min(Duration::from_millis(100))))?;
        // read_until can keep reading indefinitely if bytes arrive without a
        // newline. Check the absolute deadline between individual buffers.
        let available = match reader.fill_buf() {
            Ok([]) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "daemon closed an incomplete response",
                ))
            }
            Ok(bytes) => bytes,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock
                        | io::ErrorKind::TimedOut
                        | io::ErrorKind::Interrupted
                ) =>
            {
                continue
            }
            Err(error) => return Err(error),
        };
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "response deadline expired",
            ));
        }
        let consumed = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |index| index + 1);
        if line.len().saturating_add(consumed) > MAX_PROTOCOL_LINE_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "response exceeds protocol limit",
            ));
        }
        line.extend_from_slice(&available[..consumed]);
        reader.consume(consumed);
        if line.last() == Some(&b'\n') {
            return serde_json::from_slice(&line)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mi_terminal::daemon::protocol::encode_line;
    use std::io::Write;

    #[test]
    fn retains_partial_utf8_and_the_next_buffered_response() {
        let (client, mut server) = UnixStream::pair().unwrap();
        let expected = Response::Error {
            message: "respuesta 🚀".to_owned(),
        };
        let encoded = encode_line(&expected).into_bytes();
        let split = encoded.iter().position(|byte| *byte == 0xf0).unwrap() + 1;
        let writer = std::thread::spawn(move || {
            server.write_all(&encoded[..split]).unwrap();
            // Exceed the per-read timeout, but not the absolute deadline.
            std::thread::sleep(Duration::from_millis(250));
            server.write_all(&encoded[split..]).unwrap();
            server
                .write_all(encode_line(&Response::ShuttingDown).as_bytes())
                .unwrap();
        });
        let mut reader = BufReader::new(client);
        let deadline = Instant::now() + Duration::from_secs(5);
        assert_eq!(read_before(&mut reader, deadline).unwrap(), expected);
        assert_eq!(
            read_before(&mut reader, deadline).unwrap(),
            Response::ShuttingDown
        );
        writer.join().unwrap();
    }

    #[test]
    fn rejects_empty_and_partial_eof() {
        for prefix in [b"".as_slice(), b"{\"type\":\"output\""] {
            let (client, mut server) = UnixStream::pair().unwrap();
            server.write_all(prefix).unwrap();
            drop(server);
            let error = read_before(
                &mut BufReader::new(client),
                Instant::now() + Duration::from_secs(1),
            )
            .unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
        }
    }

    #[test]
    fn silent_peer_cannot_extend_the_absolute_deadline() {
        let (client, _server) = UnixStream::pair().unwrap();
        let error = read_before(
            &mut BufReader::new(client),
            Instant::now() + Duration::from_millis(150),
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    }

    #[test]
    fn rejects_invalid_messages_instead_of_skipping_them() {
        let (client, mut server) = UnixStream::pair().unwrap();
        server.write_all(b"not json\n").unwrap();
        let error = read_before(
            &mut BufReader::new(client),
            Instant::now() + Duration::from_secs(1),
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }
}

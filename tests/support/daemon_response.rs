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

/// Measure real printed payload, excluding OSC title/cwd/status frames that
/// the daemon may insert between PTY chunks. Metadata cannot supply bytes.
pub fn longest_printed_run(bytes: &[u8], needle: char) -> usize {
    struct Runs {
        needle: char,
        current: usize,
        longest: usize,
    }
    impl alacritty_terminal::vte::Perform for Runs {
        fn print(&mut self, character: char) {
            if character == self.needle {
                self.current += 1;
                self.longest = self.longest.max(self.current);
            } else {
                self.current = 0;
            }
        }
        fn execute(&mut self, byte: u8) {
            if matches!(byte, b'\r' | b'\n') {
                self.current = 0;
            }
        }
    }
    let mut runs = Runs {
        needle,
        current: 0,
        longest: 0,
    };
    let mut parser = alacritty_terminal::vte::Parser::new();
    parser.advance(&mut runs, bytes);
    runs.longest
}

#[cfg(test)]
mod tests {
    use super::*;
    use mi_terminal::daemon::protocol::encode_line;
    use std::io::Write;

    #[test]
    fn osc_metadata_neither_breaks_nor_supplies_payload_bytes() {
        const BYTES: usize = 512 * 1024;
        let mut output = vec![b'x'; BYTES / 2];
        output.extend_from_slice(b"\x1b]2;xxxx\x07\x1b]7;file:///xxxx\x1b\\");
        output.extend(std::iter::repeat_n(b'x', BYTES / 2));
        assert_eq!(longest_printed_run(&output, 'x'), BYTES);
        output.pop();
        assert_eq!(longest_printed_run(&output, 'x'), BYTES - 1);
        assert_eq!(longest_printed_run(b"xxx\r\nxxx", 'x'), 3);
        assert_eq!(longest_printed_run(b"xxxYxxx", 'x'), 3);
    }

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

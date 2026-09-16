"""One-shot, exact-anchor repairs for the reviewed premium-quality revision."""
from pathlib import Path
import subprocess


def replace_once(path, old, new):
    p = Path(path)
    text = p.read_text()
    assert text.count(old) == 1, f'{path}: expected one matching anchor'
    p.write_text(text.replace(old, new))


def commit(message, paths):
    subprocess.run(['cargo', 'fmt', '--all'], check=True)
    changed = subprocess.check_output(['git', 'diff', '--name-only'], text=True).splitlines()
    assert set(changed).issubset(set(paths)), f'unexpected changes: {changed}'
    subprocess.run(['git', 'add', '--', *paths], check=True)
    subprocess.run(['git', 'commit', '-m', message], check=True)


replace_once('src/daemon/protocol.rs',
    '#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]\npub struct WireSpec',
    '#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]\npub struct WireSpec')
replace_once('src/daemon/protocol.rs', 'fn default_cols() -> u16 {', '''// Rust construction and deserialization must use the same terminal geometry.
// Deriving Default silently creates a 0x0 PTY, later clamped to 1x1.
impl Default for WireSpec {
    fn default() -> Self {
        Self {
            memory_task_id: None,
            title: String::new(),
            cwd: None,
            startup_command: None,
            panel_id: None,
            workspace_id: None,
            leaf_id: None,
            cols: default_cols(),
            rows: default_rows(),
        }
    }
}

fn default_cols() -> u16 {''')
replace_once('src/daemon/protocol.rs', '    fn temp_dir(tag: &str) -> std::path::PathBuf {', '''    #[test]
    fn default_spec_matches_missing_wire_fields() {
        let decoded: WireSpec = serde_json::from_str("{}").unwrap();
        assert_eq!(WireSpec::default(), decoded);
        assert_eq!((decoded.cols, decoded.rows), (80, 24));
    }

    fn temp_dir(tag: &str) -> std::path::PathBuf {''')
commit('fix(daemon): align default PTY geometry with wire defaults', ['src/daemon/protocol.rs'])

replace_once('tests/daemon_process.rs', 'struct DaemonProcess {', '''#[path = "support/daemon_response.rs"]
mod daemon_response;

struct DaemonProcess {''')
p = Path('tests/support/daemon_response.rs')
p.parent.mkdir(exist_ok=True)
p.write_text(r'''//! Test-only NDJSON reader. Retry transient socket errors without losing a prefix.
use std::io::{self, BufRead, BufReader};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use mi_terminal::daemon::protocol::{Response, MAX_PROTOCOL_LINE_BYTES};

pub fn read_before(reader: &mut BufReader<UnixStream>, deadline: Instant) -> io::Result<Response> {
    let mut line = Vec::new();
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "response deadline expired"));
        }
        reader.get_ref().set_read_timeout(Some(remaining.min(Duration::from_millis(100))))?;
        // read_until can keep reading indefinitely if bytes arrive without a
        // newline. Check the absolute deadline between individual buffers.
        let available = match reader.fill_buf() {
            Ok([]) => return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "daemon closed an incomplete response")),
            Ok(bytes) => bytes,
            Err(error) if matches!(error.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::Interrupted) => continue,
            Err(error) => return Err(error),
        };
        if Instant::now() >= deadline {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "response deadline expired"));
        }
        let consumed = available.iter().position(|byte| *byte == b'\n').map_or(available.len(), |index| index + 1);
        if line.len().saturating_add(consumed) > MAX_PROTOCOL_LINE_BYTES {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "response exceeds protocol limit"));
        }
        line.extend_from_slice(&available[..consumed]);
        reader.consume(consumed);
        if line.last() == Some(&b'\n') {
            return serde_json::from_slice(&line).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use mi_terminal::daemon::protocol::encode_line;

    #[test]
    fn retains_partial_utf8_and_the_next_buffered_response() {
        let (client, mut server) = UnixStream::pair().unwrap();
        let expected = Response::Error { message: "respuesta 🚀".to_owned() };
        let encoded = encode_line(&expected).into_bytes();
        let split = encoded.iter().position(|byte| *byte == 0xf0).unwrap() + 1;
        let writer = std::thread::spawn(move || {
            server.write_all(&encoded[..split]).unwrap();
            // Exceed the per-read timeout, but not the absolute deadline.
            std::thread::sleep(Duration::from_millis(250));
            server.write_all(&encoded[split..]).unwrap();
            server.write_all(encode_line(&Response::ShuttingDown).as_bytes()).unwrap();
        });
        let mut reader = BufReader::new(client);
        let deadline = Instant::now() + Duration::from_secs(5);
        assert_eq!(read_before(&mut reader, deadline).unwrap(), expected);
        assert_eq!(read_before(&mut reader, deadline).unwrap(), Response::ShuttingDown);
        writer.join().unwrap();
    }

    #[test]
    fn rejects_empty_and_partial_eof() {
        for prefix in [b"".as_slice(), b"{\"type\":\"output\""] {
            let (client, mut server) = UnixStream::pair().unwrap();
            server.write_all(prefix).unwrap();
            drop(server);
            let error = read_before(&mut BufReader::new(client), Instant::now() + Duration::from_secs(1)).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
        }
    }

    #[test]
    fn silent_peer_cannot_extend_the_absolute_deadline() {
        let (client, _server) = UnixStream::pair().unwrap();
        let error = read_before(&mut BufReader::new(client), Instant::now() + Duration::from_millis(150)).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    }

    #[test]
    fn rejects_invalid_messages_instead_of_skipping_them() {
        let (client, mut server) = UnixStream::pair().unwrap();
        server.write_all(b"not json\n").unwrap();
        let error = read_before(&mut BufReader::new(client), Instant::now() + Duration::from_secs(1)).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }
}
''')
p = Path('tests/daemon_process.rs')
s = p.read_text()
start = s.index('fn several_real_ptys_finish_parallel_output_bursts() {')
end = s.index('\n#[test]', start)
old = s[start:end]
new = old.replace('use mi_terminal::daemon::protocol::{decode_line, encode_line, Request, Response};', 'use mi_terminal::daemon::protocol::{encode_line, Request, Response};').replace('use std::io::{BufRead, BufReader, Write};', 'use std::io::{BufReader, Write};')
closure = '''                let next = |reader: &mut BufReader<UnixStream>| loop {
                    let mut line = String::new();
                    let read = reader.read_line(&mut line).expect("respuesta");
                    assert!(read > 0, "el daemon cerró el stream");
                    if let Some(response) = decode_line::<Response>(&line) {
                        break response;
                    }
                };'''
assert new.count(closure) == 1
new = new.replace(closure, '''                let next = |reader: &mut BufReader<UnixStream>, deadline: Instant| {
                    daemon_response::read_before(reader, deadline)
                        .unwrap_or_else(|error| panic!("respuesta de la sesión {index}: {error}"))
                };
                let handshake_deadline = Instant::now() + Duration::from_secs(30);''')
assert new.count('next(&mut reader)') == 4
new = new.replace('next(&mut reader)', 'next(&mut reader, handshake_deadline)', 3).replace('next(&mut reader)', 'next(&mut reader, deadline)')
# Keep both existing marker and size assertions. Also require the actual
# payload bytes, so command echo or metadata cannot conceal missing data.
anchor = '''                assert!(
                    output.len() >= OUTPUT_BYTES,'''
assert new.count(anchor) == 1
new = new.replace(anchor, '''                assert!(
                    output.split(|byte| *byte != b'x').any(|run| run.len() >= OUTPUT_BYTES),
                    "la sesión {index} perdió bytes del payload"
                );
                assert!(
                    output.len() >= OUTPUT_BYTES,''')
p.write_text(s[:start] + new + s[end:])
commit('test(daemon): retry partial UnixStream replies until a fixed deadline', ['tests/daemon_process.rs', 'tests/support/daemon_response.rs'])

replace_once('src/terminal/pty.rs', '    pub fn alive(&self) -> bool {', '''    /// The process can exit before its PTY reader finishes publishing bytes.
    /// This non-blocking boundary must be sampled before draining the final log.
    /// Windows UI liveness still uses its independent ConPTY process watcher.
    pub fn output_finished(&self) -> bool {
        self._reader_thread.is_finished()
    }

    pub fn alive(&self) -> bool {''')
replace_once('src/terminal/pty.rs', '''                        Err(_) => break,
                    }
                }
            }));''', '''                        Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(error) => {
                            log::error!("PTY reader for session {session_id} failed: {error}");
                            break;
                        }
                    }
                }
            }));''')
replace_once('src/daemon/server.rs', '''                    // Sample EOF before draining. If it arrives during this
                    // drain, defer Exit to the next pump, which drains all of
                    // the final backlog before ending the subscriber stream.
                    let alive = pty.alive.load(std::sync::atomic::Ordering::Acquire);''', '''                    // Process liveness (including terminal exit events) is not
                    // an EOF boundary. Sample reader completion before draining:
                    // a completion during this drain is handled by the next pump.
                    let alive = !pty.output_finished();''')
replace_once('src/daemon/server.rs', '''        while handle
            .lock()
            .unwrap()
            .alive
            .load(std::sync::atomic::Ordering::Acquire)
        {
            assert!(Instant::now() < deadline, "fixture process did not exit");''', '''        while !handle.lock().unwrap().output_finished() {
            assert!(Instant::now() < deadline, "fixture reader did not finish");''')
replace_once('src/daemon/server.rs', '    fn attach_reports_process_exit_and_kill_notifies_subscribers() {', '''    fn early_exit_signal_does_not_discard_later_pty_output() {
        use std::sync::{Arc, Mutex};
        let mut state = DaemonState::new();
        let scheduler = Arc::new(Mutex::new(crate::runtime::RuntimeScheduler::new()));
        let id = state.spawn_with_pty(WireSpec::default(), &scheduler, None);
        let handle = state.session(id).unwrap().handle.clone().expect("real PTY");
        state.priority_session = Some(id);
        // Model a process-exit notification while the reader is still active.
        handle.lock().unwrap().alive.store(false, std::sync::atomic::Ordering::Release);
        assert!(!handle.lock().unwrap().output_finished());
        assert!(!state.pump_output().iter().any(|event| matches!(event, Response::Exit { .. })));
        let marker = format!("TC_LATE_{}", Uuid::new_v4().simple());
        handle.lock().unwrap().write_all(format!("printf '\\\\n{marker}\\\\n'; exit\\r").as_bytes());
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut output = Vec::new();
        let mut exited = false;
        while !exited {
            for event in state.pump_output() {
                match event {
                    Response::Output { id: got, data, .. } if got == id => {
                        assert!(!exited, "output delivered after Exit");
                        output.extend_from_slice(&data);
                    }
                    Response::Exit { id: got } if got == id => exited = true,
                    _ => {}
                }
            }
            assert!(Instant::now() < deadline, "PTY reader did not finish");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(String::from_utf8_lossy(&output).lines().any(|line| line.trim() == marker));
        assert!(handle.lock().unwrap().pending_log_snapshot().is_empty());
        assert!(!state.pump_output().iter().any(|event| matches!(event, Response::Output { .. } | Response::Exit { .. })));
    }

    #[test]
    fn attach_reports_process_exit_and_kill_notifies_subscribers() {''')
commit('fix(daemon): await reader completion before final output and Exit', ['src/terminal/pty.rs', 'src/daemon/server.rs'])

replace_once('Cargo.toml', 'rustls = "0.23"', 'rustls = "0.23.45"')
subprocess.run(['cargo', 'update', '-p', 'rustls', '--precise', '0.23.45'], check=True)
commit('fix(deps): update rustls for RUSTSEC-2026-0285', ['Cargo.toml', 'Cargo.lock'])
subprocess.run(['git', 'diff', '--check'], check=True)
subprocess.run(['cargo', 'fmt', '--all', '--', '--check'], check=True)
print(subprocess.check_output(['git', 'log', '-4', '--oneline'], text=True))

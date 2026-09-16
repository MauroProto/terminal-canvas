//! Test de proceso real del daemon de PTYs (P3.15, T6).
//!
//! Levanta el binario del daemon de verdad en un directorio temporal, habla
//! por el socket, **mata al cliente** y se reengancha: es la única forma de
//! comprobar lo que el daemon promete, que las sesiones sobreviven a la app.

#![cfg(all(unix, feature = "daemon"))]

use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use mi_terminal::daemon::client::DaemonConn;
use mi_terminal::daemon::protocol::{self, WireSpec};

#[path = "support/daemon_response.rs"]
mod daemon_response;

struct DaemonProcess {
    child: Child,
    dir: PathBuf,
    _permit: DaemonTestPermit,
}

/// Estos tests levantan daemons, shells y procesos Python reales. Ejecutar los
/// 18 a la vez no representa el producto (una cuenta usa un solo daemon) y en
/// runners cargados puede privar a un PTY de CPU hasta agotar un timeout. Se
/// conserva paralelismo acotado para detectar carreras sin volver la suite
/// dependiente de la cantidad de cores disponible.
const MAX_CONCURRENT_TEST_DAEMONS: usize = 4;

struct DaemonTestPermit;

impl DaemonTestPermit {
    fn acquire() -> Self {
        let (active, available) = daemon_test_slots();
        let mut count = active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        while *count >= MAX_CONCURRENT_TEST_DAEMONS {
            count = available
                .wait(count)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
        *count += 1;
        Self
    }
}

impl Drop for DaemonTestPermit {
    fn drop(&mut self) {
        release_daemon_test_permit();
    }
}

fn daemon_test_slots() -> &'static (Mutex<usize>, Condvar) {
    static ACTIVE: OnceLock<(Mutex<usize>, Condvar)> = OnceLock::new();
    ACTIVE.get_or_init(|| (Mutex::new(0), Condvar::new()))
}

fn release_daemon_test_permit() {
    let (active, available) = daemon_test_slots();
    let mut count = active
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *count = count.saturating_sub(1);
    available.notify_one();
}

impl Drop for DaemonProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn daemon_binary() -> PathBuf {
    // El test corre desde target/debug/deps: el binario queda dos niveles
    // arriba (target/debug/mi-terminal-daemon).
    let mut path = std::env::current_exe().expect("current_exe");
    path.pop();
    if path.ends_with("deps") {
        path.pop();
    }
    path.join("mi-terminal-daemon")
}

/// Levanta el daemon y espera a que el socket esté listo.
fn start_daemon() -> (DaemonProcess, String) {
    let permit = DaemonTestPermit::acquire();
    // Dir corto a propósito: el path del socket tiene que entrar en los 104
    // bytes de `sun_path` (un uuid completo en /var/folders/... no entra).
    let short_id = uuid::Uuid::new_v4().simple().to_string()[..8].to_owned();
    let dir = std::path::PathBuf::from("/tmp").join(format!("tcd-{short_id}"));
    std::fs::create_dir_all(&dir).expect("mkdir");
    let binary = daemon_binary();
    assert!(
        binary.exists(),
        "falta el binario del daemon en {}: corré `cargo build`",
        binary.display()
    );

    let child = Command::new(&binary)
        .env("MI_TERMINAL_DAEMON_DIR", &dir)
        .env("MI_TERMINAL_SCROLLBACK_DIR", dir.join("scrollback"))
        .spawn()
        .expect("arranca el daemon");
    let process = DaemonProcess {
        child,
        dir: dir.clone(),
        _permit: permit,
    };

    let socket = protocol::socket_path(&dir);
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline && !socket.exists() {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(socket.exists(), "el daemon nunca abrió el socket");

    let token = protocol::ensure_token(&dir).expect("token");
    (process, token)
}

#[test]
fn sessions_survive_the_app_and_can_be_reattached() {
    let (daemon, token) = start_daemon();

    // "App" #1: crea una sesión y ve salida.
    let session_id = {
        let mut conn = DaemonConn::try_connect(&daemon.dir, &token).expect("conecta");
        let id = conn
            .spawn_session(WireSpec {
                title: "Terminal".to_owned(),
                cols: 80,
                rows: 24,
                ..WireSpec::default()
            })
            .expect("spawnea");
        assert_eq!(conn.list(), vec![id]);
        id
        // Acá se dropea la conexión: equivale a que la app se cierre o crashee.
    };

    // "App" #2: el daemon siguió vivo con la sesión adentro.
    let mut conn = DaemonConn::try_connect(&daemon.dir, &token).expect("reconecta");
    assert_eq!(
        conn.list(),
        vec![session_id],
        "la sesión tiene que sobrevivir al cierre de la app"
    );

    let (snapshot, seq) = conn.attach(session_id).expect("reattach");
    assert!(
        seq == 0 || !snapshot.is_empty(),
        "attach devuelve snapshot+seq"
    );
}

#[test]
fn attaching_inside_a_tui_recovers_primary_history_when_it_exits() {
    use alacritty_terminal::term::test::TermSize;
    use alacritty_terminal::term::{Config, Term};
    use alacritty_terminal::vte::ansi::{Processor, StdSyncHandler};
    use mi_terminal::daemon::protocol::{Request, Response};
    let (daemon, token) = start_daemon();
    let mut conn = DaemonConn::try_connect(&daemon.dir, &token).expect("connect");
    let id = conn.spawn_session(WireSpec {
        startup_command: Some("printf 'TC_PRIMARY_HISTORY\\r\\n\\033[?1049hTC_TUI_SCREEN'; read tc_done; printf '\\033[?1049lTC_RETURNED\\r\\n'".to_owned()),
        ..Default::default()
    }).expect("spawn");
    let deadline = Instant::now() + Duration::from_secs(10);
    let (snapshot, attached_seq) = loop {
        let attached = conn.attach(id).expect("attach");
        // El eco del comando en la pantalla primaria también contiene el
        // texto literal "TC_TUI_SCREEN". Sólo la secuencia real de alternate
        // screen, que el eco no puede producir, prueba que la TUI empezó.
        if attached
            .0
            .windows(b"\x1b[?1049h".len())
            .any(|bytes| bytes == b"\x1b[?1049h")
            && attached
                .0
                .windows(b"TC_TUI_SCREEN".len())
                .any(|bytes| bytes == b"TC_TUI_SCREEN")
        {
            break attached;
        }
        assert!(Instant::now() < deadline, "TUI did not start");
        std::thread::sleep(Duration::from_millis(20));
    };
    let (tx, _rx) = std::sync::mpsc::channel();
    let mut term = Term::new(
        Config::default(),
        &TermSize::new(80, 24),
        mi_terminal::terminal::pty::EventProxy::new(tx),
    );
    let mut parser = Processor::<StdSyncHandler>::new();
    parser.advance(&mut term, &snapshot);
    assert!(
        !mi_terminal::terminal::export::scrollback_to_text(&term).contains("TC_PRIMARY_HISTORY")
    );
    conn.request(&Request::Write {
        id,
        data: b"\n".to_vec(),
    })
    .expect("resume TUI");
    loop {
        conn.list();
        for event in conn.drain_events() {
            if let Response::Output { seq, data, .. } = event {
                if seq > attached_seq {
                    parser.advance(&mut term, &data);
                }
            }
        }
        let text = mi_terminal::terminal::export::scrollback_to_text(&term);
        if text.contains("TC_RETURNED") && text.contains("TC_PRIMARY_HISTORY") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "primary screen was lost: {text:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn reattach_snapshot_preserves_scrollback_beyond_the_raw_tail_cap() {
    use mi_terminal::daemon::protocol::Request;

    let (daemon, token) = start_daemon();
    let mut conn = DaemonConn::try_connect(&daemon.dir, &token).expect("conecta");
    let id = conn
        .spawn_session(WireSpec {
            cols: 80,
            rows: 24,
            ..WireSpec::default()
        })
        .expect("spawn");
    let start = format!("SNAP_START_{}", uuid::Uuid::new_v4().simple());
    let end = format!("SNAP_END_{}", uuid::Uuid::new_v4().simple());
    let command =
        format!("python3 -c \"import sys;sys.stdout.write('{start}'+'x'*400000+'{end}')\"\n");
    assert!(conn
        .request(&Request::Write {
            id,
            data: command.into_bytes(),
        })
        .is_some());

    let deadline = Instant::now() + Duration::from_secs(10);
    let snapshot = loop {
        let (snapshot, _) = conn.attach(id).expect("attach");
        let completed = snapshot
            .windows(end.len())
            .filter(|window| *window == end.as_bytes())
            .count()
            >= 2;
        if completed && snapshot.len() > mi_terminal::daemon::server::MAX_SNAPSHOT_BYTES {
            break snapshot;
        }
        assert!(Instant::now() < deadline, "la salida nunca terminó");
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(
        snapshot.len() > mi_terminal::daemon::server::MAX_SNAPSHOT_BYTES,
        "el reattach sigue limitado al tail crudo: {} bytes",
        snapshot.len()
    );
    assert!(
        snapshot
            .windows(start.len())
            .any(|window| window == start.as_bytes()),
        "se perdió el comienzo del scrollback"
    );
}

#[test]
fn attach_snapshot_and_seq_share_one_atomic_output_boundary() {
    use mi_terminal::daemon::protocol::{Request, Response};

    let (daemon, token) = start_daemon();
    let mut conn = DaemonConn::try_connect(&daemon.dir, &token).expect("conecta");
    for _ in 0..8 {
        let id = conn
            .spawn_session(WireSpec::default())
            .expect("spawn para boundary");
        let marker = format!("BOUNDARY_{}", uuid::Uuid::new_v4().simple());
        let codepoints = marker
            .bytes()
            .map(|byte| byte.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let command = format!("python3 -c \"print(''.join(map(chr,[{codepoints}])))\"\n");
        assert!(conn
            .request(&Request::Write {
                id,
                data: command.into_bytes(),
            })
            .is_some());
        // Da tiempo al reader PTY para alimentar el grid, pero normalmente no
        // al pump de 50 ms: reproduce la vieja ventana snapshot/seq.
        std::thread::sleep(Duration::from_millis(8));
        let (snapshot, _) = conn.attach(id).expect("attach");
        let mut combined = snapshot;
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let _ = conn.list();
            for event in conn.drain_events() {
                if let Response::Output {
                    id: event_id, data, ..
                } = event
                {
                    if event_id == id {
                        combined.extend_from_slice(&data);
                    }
                }
            }
            let count = combined
                .windows(marker.len())
                .filter(|window| *window == marker.as_bytes())
                .count();
            if count > 0 {
                assert_eq!(
                    count, 1,
                    "el mismo output apareció en snapshot y evento posterior"
                );
                break;
            }
            assert!(Instant::now() < deadline, "el marker no llegó");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(conn.kill(id));
    }
}

#[test]
fn a_wrong_token_is_rejected() {
    let (daemon, _token) = start_daemon();
    assert!(
        DaemonConn::try_connect(&daemon.dir, "token-equivocado").is_none(),
        "el daemon no puede aceptar un token cualquiera"
    );
}

#[test]
fn reconcile_kills_the_sessions_the_app_no_longer_knows() {
    let (daemon, token) = start_daemon();
    let mut conn = DaemonConn::try_connect(&daemon.dir, &token).expect("conecta");

    let keep = conn.spawn_session(WireSpec::default()).expect("spawn");
    let orphan = conn.spawn_session(WireSpec::default()).expect("spawn");

    let killed = conn.reconcile_live(vec![keep]);
    assert_eq!(killed, vec![orphan], "el huérfano se va");
    assert_eq!(conn.list(), vec![keep]);
}

#[test]
fn reconcile_preserves_sessions_owned_by_another_live_app() {
    let (daemon, token) = start_daemon();
    let mut first = DaemonConn::try_connect(&daemon.dir, &token).expect("primera app");
    let first_id = first
        .spawn_session(WireSpec::default())
        .expect("sesión primera app");
    let mut second = DaemonConn::try_connect(&daemon.dir, &token).expect("segunda app");
    let second_id = second
        .spawn_session(WireSpec::default())
        .expect("sesión segunda app");
    second
        .attach(first_id)
        .expect("la segunda app puede observar la sesión compartida");

    assert!(
        second.reconcile_live(vec![second_id]).is_empty(),
        "una app viva no puede matar sesiones de otra app viva"
    );
    let mut both = second.list();
    both.sort_by_key(uuid::Uuid::as_u128);
    let mut expected = vec![first_id, second_id];
    expected.sort_by_key(uuid::Uuid::as_u128);
    assert_eq!(both, expected);

    drop(first);
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(
        second.reconcile_live(vec![second_id]),
        vec![first_id],
        "cuando la dueña desaparece, otra app puede limpiar el huérfano"
    );
}

#[test]
fn a_second_daemon_cannot_take_over_an_active_socket() {
    let (mut daemon, _token) = start_daemon();
    let first_pid = daemon.child.id();
    let mut second = Command::new(daemon_binary())
        .env("MI_TERMINAL_DAEMON_DIR", &daemon.dir)
        .env("MI_TERMINAL_SCROLLBACK_DIR", daemon.dir.join("scrollback"))
        .spawn()
        .expect("intenta segundo daemon");
    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Some(status) = second.try_wait().expect("estado segundo daemon") {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "el segundo daemon quedó vivo compitiendo por el socket"
        );
        std::thread::sleep(Duration::from_millis(25));
    };
    assert!(
        !status.success(),
        "el segundo daemon debe rechazar el socket"
    );
    assert!(
        daemon.child.try_wait().expect("estado daemon").is_none(),
        "el daemon original debe seguir vivo"
    );
    assert_eq!(
        protocol::read_pid_file(&daemon.dir).expect("pid-file").pid,
        first_pid,
        "el segundo daemon no puede sobrescribir la identidad del primero"
    );
}

#[test]
fn killing_a_session_removes_it_for_every_client() {
    let (daemon, token) = start_daemon();
    let mut first = DaemonConn::try_connect(&daemon.dir, &token).expect("conecta");
    let id = first.spawn_session(WireSpec::default()).expect("spawn");

    let mut second = DaemonConn::try_connect(&daemon.dir, &token).expect("segunda app");
    assert_eq!(second.list(), vec![id], "las dos apps ven la misma sesión");

    assert!(second.kill(id));
    assert!(
        first.list().is_empty(),
        "matarla desde una app la saca para todas"
    );
}

#[test]
fn the_pid_file_identifies_the_running_daemon() {
    let (daemon, _token) = start_daemon();
    let pid_file = protocol::read_pid_file(&daemon.dir).expect("pid-file");
    assert_eq!(
        pid_file.pid,
        daemon.child.id(),
        "el pid tiene que ser el real"
    );
    assert!(!pid_file.nonce.is_empty());
    assert!(
        pid_file.start_ticks.is_some(),
        "el pid-file debe distinguir encarnaciones con el mismo PID"
    );
}

#[test]
fn a_real_pty_echoes_what_the_app_writes() {
    // El test que prueba que el daemon es dueño del PTY de verdad: se escribe
    // un comando y la salida vuelve por el socket como evento Output.
    use mi_terminal::daemon::protocol::{decode_line, encode_line, Request, Response};
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;

    let (daemon, token) = start_daemon();
    let stream = UnixStream::connect(protocol::socket_path(&daemon.dir)).expect("conecta");
    stream
        .set_read_timeout(Some(Duration::from_secs(15)))
        .expect("timeout");
    let mut writer = stream.try_clone().expect("clone");
    let mut reader = BufReader::new(stream);

    let send = |request: &Request, writer: &mut UnixStream| {
        writer
            .write_all(encode_line(request).as_bytes())
            .expect("escribe");
        writer.flush().expect("flush");
    };

    // Handshake.
    send(
        &Request::Hello {
            version: protocol::PROTOCOL_VERSION,
            token: token.clone(),
            client_id: uuid::Uuid::new_v4(),
        },
        &mut writer,
    );
    let mut line = String::new();
    reader.read_line(&mut line).expect("welcome");
    assert!(
        matches!(
            decode_line::<Response>(&line),
            Some(Response::Welcome { .. })
        ),
        "got {line}"
    );

    // Sesión con PTY real.
    send(
        &Request::Spawn {
            id: None,
            spec: WireSpec {
                title: "Terminal".to_owned(),
                cols: 80,
                rows: 24,
                ..WireSpec::default()
            },
        },
        &mut writer,
    );
    line.clear();
    reader.read_line(&mut line).expect("spawned");
    let id = match decode_line::<Response>(&line) {
        Some(Response::Spawned { id }) => id,
        other => panic!("esperaba Spawned, got {other:?} / {line}"),
    };
    send(&Request::Attach { id }, &mut writer);
    line.clear();
    assert!(
        reader.read_line(&mut line).is_ok()
            && matches!(
                decode_line::<Response>(&line),
                Some(Response::Attached { .. })
            ),
        "el attach no fue confirmado: {line}"
    );

    // Un marcador único, para no confundirlo con el prompt del shell.
    let marker = format!("tcmarker{}", uuid::Uuid::new_v4().simple());
    send(
        &Request::Write {
            id,
            data: format!("echo {marker}\n").into_bytes(),
        },
        &mut writer,
    );

    // Los eventos Output llegan por el mismo socket; se leen hasta encontrarlo.
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut seen = String::new();
    let mut last_seq = 0;
    while Instant::now() < deadline {
        line.clear();
        if reader.read_line(&mut line).is_err() || line.is_empty() {
            break;
        }
        if let Some(Response::Output { seq, data, .. }) = decode_line::<Response>(&line) {
            assert!(
                seq > last_seq,
                "el seq tiene que crecer: {seq} tras {last_seq}"
            );
            last_seq = seq;
            seen.push_str(&String::from_utf8_lossy(&data));
            if seen.matches(marker.as_str()).count() >= 2 {
                // Una vez el eco del comando y otra la salida del echo.
                break;
            }
        }
    }
    assert!(
        seen.contains(&marker),
        "el PTY del daemon nunca devolvió la salida. visto:\n{seen}"
    );
    assert!(last_seq > 0, "los eventos tienen que venir numerados");
}

#[test]
fn remote_spawn_delivers_startup_input_after_the_agent_renders() {
    use mi_terminal::daemon::sessions::{spawn_remote, DaemonEndpoint};
    use mi_terminal::runtime::{PtyManager, SessionSpec};

    let (daemon, token) = start_daemon();
    let endpoint = DaemonEndpoint::new(daemon.dir.clone(), token, uuid::Uuid::new_v4());
    let mut manager = PtyManager::new_for_tests();
    manager.set_remote_spawner(Box::new(move |spec, cols, rows, scheduler, desired_id| {
        spawn_remote(&endpoint, spec, cols, rows, scheduler, desired_id)
    }));
    let marker = format!("startup_{}", uuid::Uuid::new_v4().simple());
    let id = manager
        .spawn(
            SessionSpec {
                title: "startup input".to_owned(),
                startup_command: Some("cat".to_owned()),
                startup_input: Some(marker.clone()),
                ..SessionSpec::default()
            },
            None,
            80,
            24,
        )
        .expect("spawn remoto");

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut seen = String::new();
    while Instant::now() < deadline {
        manager.drain_ui_updates();
        if let Some(handle) = manager.handle(id) {
            seen = handle
                .lock()
                .ok()
                .and_then(|pty| {
                    pty.with_term(|term| mi_terminal::terminal::export::scrollback_to_text(term))
                })
                .unwrap_or_default();
            if seen.contains(&marker) {
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(
        seen.contains(&marker),
        "el daemon arrancó el agente pero perdió startup_input: {seen:?}"
    );
}

#[test]
fn several_real_ptys_finish_parallel_output_bursts() {
    use mi_terminal::daemon::protocol::{encode_line, Request, Response};
    use std::io::{BufReader, Write};
    use std::os::unix::net::UnixStream;
    use uuid::Uuid;

    const SESSIONS: usize = 8;
    const OUTPUT_BYTES: usize = 512 * 1024;

    let (daemon, token) = start_daemon();
    let socket = protocol::socket_path(&daemon.dir);
    let workers = (0..SESSIONS)
        .map(|index| {
            let socket = socket.clone();
            let token = token.clone();
            std::thread::spawn(move || {
                let stream = UnixStream::connect(socket).expect("conecta");
                stream
                    .set_read_timeout(Some(Duration::from_secs(30)))
                    .expect("timeout");
                let mut writer = stream.try_clone().expect("clone");
                let mut reader = BufReader::new(stream);
                let send = |request: &Request, writer: &mut UnixStream| {
                    writer
                        .write_all(encode_line(request).as_bytes())
                        .expect("escribe");
                    writer.flush().expect("flush");
                };
                let next = |reader: &mut BufReader<UnixStream>, deadline: Instant| {
                    daemon_response::read_before(reader, deadline)
                        .unwrap_or_else(|error| panic!("respuesta de la sesión {index}: {error}"))
                };
                let handshake_deadline = Instant::now() + Duration::from_secs(30);

                send(
                    &Request::Hello {
                        version: protocol::PROTOCOL_VERSION,
                        token,
                        client_id: Uuid::new_v4(),
                    },
                    &mut writer,
                );
                assert!(matches!(next(&mut reader, handshake_deadline), Response::Welcome { .. }));
                send(
                    &Request::Spawn {
                        id: None,
                        spec: WireSpec::default(),
                    },
                    &mut writer,
                );
                let id = match next(&mut reader, handshake_deadline) {
                    Response::Spawned { id } => id,
                    other => panic!("spawn inesperado: {other:?}"),
                };
                send(&Request::Attach { id }, &mut writer);
                assert!(matches!(next(&mut reader, handshake_deadline), Response::Attached { .. }));

                let marker = format!("load_done_{index}_{}", Uuid::new_v4().simple());
                // Neither marker is present in the command echo. Both must be
                // emitted by Python after the entire payload has been written.
                let (prefix, suffix) = marker.split_at(marker.len() / 2);
                let command = format!(
                    "python3 -c \"import sys;m='{prefix}'+'{suffix}';sys.stdout.write('x'*{OUTPUT_BYTES});print(m);print(m)\"\n"
                );
                assert!(!command.contains(&marker), "marker must not come from echo");
                send(
                    &Request::Write {
                        id,
                        data: command.into_bytes(),
                    },
                    &mut writer,
                );

                let deadline = Instant::now() + Duration::from_secs(30);
                let mut output = Vec::new();
                while Instant::now() < deadline {
                    match daemon_response::read_before(&mut reader, deadline).unwrap_or_else(|error| {
                        panic!(
                            "session {index}: {error}; bytes={}, markers={}, payload={}, head={:?}, tail={:?}",
                            output.len(),
                            output.windows(marker.len()).filter(|window| *window == marker.as_bytes()).count(),
                            daemon_response::longest_printed_run(&output, 'x'),
                            String::from_utf8_lossy(&output[..output.len().min(128)]),
                            String::from_utf8_lossy(&output[output.len().saturating_sub(128)..]),
                        )
                    }) {
                        Response::Output { id: got, data, .. } if got == id => {
                            output.extend_from_slice(&data);
                            if output
                                .windows(marker.len())
                                .filter(|window| *window == marker.as_bytes())
                                .count()
                                >= 2
                                && output.len() >= OUTPUT_BYTES
                                && daemon_response::longest_printed_run(&output, 'x') >= OUTPUT_BYTES
                            {
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                assert!(
                    output
                        .windows(marker.len())
                        .filter(|window| *window == marker.as_bytes())
                        .count()
                        >= 2,
                    "la sesión {index} no terminó su burst ({} bytes)",
                    output.len()
                );
                assert!(
                    daemon_response::longest_printed_run(&output, 'x') >= OUTPUT_BYTES,
                    "la sesión {index} perdió bytes del payload"
                );
                assert!(
                    output.len() >= OUTPUT_BYTES,
                    "la sesión {index} perdió salida: {} bytes",
                    output.len()
                );
            })
        })
        .collect::<Vec<_>>();

    // Join every worker before dropping the daemon, even if one failed.
    let results: Vec<_> = workers
        .into_iter()
        .map(std::thread::JoinHandle::join)
        .collect();
    assert!(results.iter().all(Result::is_ok), "worker de carga");
}

#[test]
fn split_sessions_persist_to_distinct_leaf_checkpoints() {
    use mi_terminal::daemon::protocol::Request;
    use mi_terminal::state::scrollback_store::scrollback_leaf_file_name;

    let (daemon, token) = start_daemon();
    let panel_id = uuid::Uuid::new_v4();
    let first_leaf = uuid::Uuid::new_v4();
    let second_leaf = uuid::Uuid::new_v4();
    let mut first = DaemonConn::try_connect(&daemon.dir, &token).expect("primera conexión");
    let mut second = DaemonConn::try_connect(&daemon.dir, &token).expect("segunda conexión");
    let spec = |leaf_id| WireSpec {
        title: "Terminal".to_owned(),
        panel_id: Some(panel_id),
        leaf_id: Some(leaf_id),
        cols: 80,
        rows: 24,
        ..WireSpec::default()
    };
    let first_id = first
        .spawn_session(spec(first_leaf))
        .expect("primera sesión");
    let second_id = second
        .spawn_session(spec(second_leaf))
        .expect("segunda sesión");
    first.attach(first_id).expect("attach primera");
    second.attach(second_id).expect("attach segunda");

    let first_marker = format!("leafone{}", uuid::Uuid::new_v4().simple());
    let second_marker = format!("leaftwo{}", uuid::Uuid::new_v4().simple());
    assert!(first
        .request(&Request::Write {
            id: first_id,
            data: format!("echo {first_marker}\n").into_bytes(),
        })
        .is_some());
    assert!(second
        .request(&Request::Write {
            id: second_id,
            data: format!("echo {second_marker}\n").into_bytes(),
        })
        .is_some());

    let scrollback_dir = daemon.dir.join("scrollback");
    let first_path = scrollback_dir.join(scrollback_leaf_file_name(panel_id, Some(first_leaf)));
    let second_path = scrollback_dir.join(scrollback_leaf_file_name(panel_id, Some(second_leaf)));
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline && (!first_path.exists() || !second_path.exists()) {
        std::thread::sleep(Duration::from_millis(100));
    }
    let first_text = std::fs::read_to_string(&first_path).expect("checkpoint primera hoja");
    let second_text = std::fs::read_to_string(&second_path).expect("checkpoint segunda hoja");
    let _ = std::fs::remove_file(&first_path);
    let _ = std::fs::remove_file(&second_path);

    assert!(first_text.contains(&first_marker), "{first_text:?}");
    assert!(!first_text.contains(&second_marker), "{first_text:?}");
    assert!(second_text.contains(&second_marker), "{second_text:?}");
    assert!(!second_text.contains(&first_marker), "{second_text:?}");
}

#[test]
fn shutdown_if_idle_stops_a_daemon_with_no_sessions() {
    use mi_terminal::daemon::protocol::{decode_line, encode_line, Request, Response};
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;

    let (mut daemon, token) = start_daemon();
    let stream = UnixStream::connect(protocol::socket_path(&daemon.dir)).expect("conecta");
    let mut writer = stream.try_clone().expect("clone");
    let mut reader = BufReader::new(stream);

    writer
        .write_all(
            encode_line(&Request::Hello {
                version: protocol::PROTOCOL_VERSION,
                token,
                client_id: uuid::Uuid::new_v4(),
            })
            .as_bytes(),
        )
        .expect("hello");
    let mut line = String::new();
    reader.read_line(&mut line).expect("welcome");

    // Sin sesiones adentro, pedirle el apagado tiene que terminarlo.
    writer
        .write_all(encode_line(&Request::ShutdownIfIdle).as_bytes())
        .expect("shutdown");
    line.clear();
    reader.read_line(&mut line).expect("respuesta");
    assert!(
        matches!(decode_line::<Response>(&line), Some(Response::ShuttingDown)),
        "got {line}"
    );

    // El watchdog lo mata dentro de su ciclo de 5 s.
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut exited = false;
    while Instant::now() < deadline {
        if matches!(daemon.child.try_wait(), Ok(Some(_))) {
            exited = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    assert!(
        exited,
        "el daemon no se apagó cuando se lo pidió estando vacío"
    );
}

#[test]
fn shutdown_if_idle_is_refused_while_a_session_lives() {
    let (mut daemon, token) = start_daemon();
    let mut conn = DaemonConn::try_connect(&daemon.dir, &token).expect("conecta");
    let id = conn.spawn_session(WireSpec::default()).expect("spawn");

    // Con una sesión adentro, el daemon NO se apaga: para eso existe.
    let _ = conn.request(&mi_terminal::daemon::protocol::Request::ShutdownIfIdle);
    std::thread::sleep(Duration::from_secs(7));
    assert!(
        matches!(daemon.child.try_wait(), Ok(None)),
        "el daemon se apagó con una sesión viva adentro"
    );
    assert_eq!(conn.list(), vec![id], "y la sesión sigue ahí");
}

#[cfg(feature = "daemon")]
#[test]
fn a_desired_id_reattaches_instead_of_creating_a_second_session() {
    // Éste es el corazón de la supervivencia (T4): al reabrir la app, el panel
    // pide **su** sesión de la corrida anterior. Si en vez de engancharse
    // creara otra, el usuario perdería su shell y quedaría un PTY huérfano.
    use mi_terminal::daemon::sessions::{spawn_remote, DaemonEndpoint};
    use mi_terminal::runtime::{RuntimeScheduler, SessionSpec};
    use std::sync::{Arc, Mutex};

    let (daemon, token) = start_daemon();
    let endpoint = DaemonEndpoint::new(daemon.dir.clone(), token.clone(), uuid::Uuid::new_v4());
    let scheduler = Arc::new(Mutex::new(RuntimeScheduler::new()));
    let spec = SessionSpec::default();

    // Primera corrida: se crea la sesión.
    let (first_id, first_handle) =
        spawn_remote(&endpoint, &spec, 80, 24, Arc::clone(&scheduler), None)
            .expect("crea la sesión");

    // "Se cierra la app": el handle local se va, la sesión del daemon queda.
    drop(first_handle);
    let mut conn = DaemonConn::try_connect(&daemon.dir, &token).expect("conecta");
    assert_eq!(conn.list(), vec![first_id], "la sesión sobrevive al handle");

    // Segunda corrida: pidiendo el mismo id tiene que reengancharse.
    let (second_id, _second_handle) =
        spawn_remote(&endpoint, &spec, 80, 24, scheduler, Some(first_id)).expect("se reengancha");

    assert_eq!(second_id, first_id, "tiene que ser la MISMA sesión");
    assert_eq!(
        conn.list(),
        vec![first_id],
        "y no puede haber quedado una segunda"
    );
}

#[cfg(feature = "daemon")]
#[test]
fn an_unknown_desired_id_creates_the_session_with_that_id() {
    // Primera corrida de un panel restaurado: el daemon no tiene su sesión
    // (por ejemplo porque se reinició la máquina), así que la crea con el id
    // que pidió la app para que el mapeo panel ↔ sesión siga valiendo.
    use mi_terminal::daemon::sessions::{spawn_remote, DaemonEndpoint};
    use mi_terminal::runtime::{RuntimeScheduler, SessionSpec};
    use std::sync::{Arc, Mutex};

    let (daemon, token) = start_daemon();
    let endpoint = DaemonEndpoint::new(daemon.dir.clone(), token.clone(), uuid::Uuid::new_v4());
    let wanted = uuid::Uuid::new_v4();

    let (id, _handle) = spawn_remote(
        &endpoint,
        &SessionSpec::default(),
        80,
        24,
        Arc::new(Mutex::new(RuntimeScheduler::new())),
        Some(wanted),
    )
    .expect("crea la sesión");

    assert_eq!(id, wanted, "el daemon tiene que respetar el id pedido");
    let mut conn = DaemonConn::try_connect(&daemon.dir, &token).expect("conecta");
    assert_eq!(conn.list(), vec![wanted]);
}

#[cfg(feature = "daemon")]
#[test]
fn closing_the_app_keeps_the_session_but_closing_the_panel_kills_it() {
    // La distinción que importa: soltar la sesión (lo que pasa al cerrar la
    // app, porque los paneles se dropean) NO puede matar el PTY del daemon;
    // cerrar el panel a mano SÍ. Antes de este arreglo, un Cmd+Q mataba las
    // cuatro sesiones y el daemon no servía para nada.
    use mi_terminal::daemon::sessions::DaemonEndpoint;
    use mi_terminal::runtime::{PtyManager, SessionSpec};

    let (daemon, token) = start_daemon();
    let endpoint = DaemonEndpoint::new(daemon.dir.clone(), token.clone(), uuid::Uuid::new_v4());

    let mut manager = PtyManager::new_for_tests();
    let spawner_endpoint = endpoint.clone();
    manager.set_remote_spawner(Box::new(move |spec, cols, rows, scheduler, desired| {
        mi_terminal::daemon::sessions::spawn_remote(
            &spawner_endpoint,
            spec,
            cols,
            rows,
            scheduler,
            desired,
        )
    }));
    assert!(manager.hosts_out_of_process());

    let id = manager
        .spawn(SessionSpec::default(), None, 80, 24)
        .expect("crea la sesión en el daemon");

    let mut conn = DaemonConn::try_connect(&daemon.dir, &token).expect("conecta");
    assert_eq!(conn.list(), vec![id], "la sesión vive en el daemon");

    // Cierre de la app: se suelta la sesión, no se mata.
    assert!(manager.close(id));
    assert_eq!(
        conn.list(),
        vec![id],
        "cerrar la app NO puede matar la sesión del daemon"
    );

    // Cierre de panel a mano: ahora sí se va.
    let id2 = manager
        .spawn(SessionSpec::default(), None, 80, 24)
        .expect("otra sesión");
    assert!(manager.close_and_kill_remote(id2));
    // El kill viaja por la conexión del manager y el list por esta otra, así
    // que no hay orden garantizado entre las dos: bajo carga el list llegaba
    // primero y el test fallaba sin que hubiera nada roto. Se espera al efecto.
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut remaining = conn.list();
    while remaining.contains(&id2) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(25));
        remaining = conn.list();
    }
    assert!(
        !remaining.contains(&id2),
        "cerrar el panel tiene que matar su sesión; quedan {remaining:?}"
    );
    assert!(
        remaining.contains(&id),
        "y no puede llevarse puesta la sesión que sólo se soltó"
    );
}

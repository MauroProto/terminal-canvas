//! Test de proceso real del daemon de PTYs (P3.15, T6).
//!
//! Levanta el binario del daemon de verdad en un directorio temporal, habla
//! por el socket, **mata al cliente** y se reengancha: es la única forma de
//! comprobar lo que el daemon promete, que las sesiones sobreviven a la app.

#![cfg(unix)]

use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use mi_terminal::daemon::client::DaemonConn;
use mi_terminal::daemon::protocol::{self, WireSpec};

struct DaemonProcess {
    child: Child,
    dir: PathBuf,
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
        .spawn()
        .expect("arranca el daemon");
    let process = DaemonProcess {
        child,
        dir: dir.clone(),
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
}

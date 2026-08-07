//! Daemon de PTYs (P3.15): proceso separado que tiene las sesiones para que
//! cerrar o crashear la app no mate los agentes.
//!
//! Se levanta solo desde la app (`fork+setsid`); a mano se corre con
//! `MI_TERMINAL_DAEMON_DIR=/tmp/x mi-terminal-daemon`.

#[cfg(unix)]
use mi_terminal::daemon::{protocol, server};

#[cfg(not(unix))]
fn main() {
    eprintln!("el daemon de PTYs solo existe en unix");
    std::process::exit(1);
}

#[cfg(unix)]
fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let Some(dir) = server::resolve_dir() else {
        eprintln!("no se pudo resolver el directorio del daemon");
        std::process::exit(1);
    };
    if let Err(err) = std::fs::create_dir_all(&dir) {
        eprintln!("no se pudo crear {}: {err}", dir.display());
        std::process::exit(1);
    }

    let token = match protocol::ensure_token(&dir) {
        Ok(token) => token,
        Err(err) => {
            eprintln!("no se pudo preparar el token: {err}");
            std::process::exit(1);
        }
    };

    // Pid-file con nonce: distingue este daemon de un proceso que reusó el pid.
    let pid_file = protocol::PidFile::new(std::process::id());
    if let Err(err) = protocol::write_pid_file(&dir, &pid_file) {
        log::warn!("no se pudo escribir el pid-file: {err}");
    }

    log::info!(
        "daemon escuchando en {}",
        protocol::socket_path(&dir).display()
    );
    if let Err(err) = server::serve(&dir, token) {
        eprintln!("el daemon terminó con error: {err}");
        std::process::exit(1);
    }
}

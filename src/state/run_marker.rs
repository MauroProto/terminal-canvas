//! Marcador de corrida: distingue un cierre limpio de una muerte súbita.
//!
//! Al arrancar se escribe un marcador en disco y al salir limpio se borra. Si
//! al arrancar el marcador ya existe, la corrida anterior murió sin pasar por
//! `on_exit` (kill, crash nativo, OOM, corte de luz). Eso se le informa al
//! usuario —su estado se restaura igual— y queda asentado en un log de
//! diagnóstico con hora de inicio y PID, para poder correlacionar con lo que
//! haya pasado en el sistema.
//!
//! No detecta el *motivo* de la muerte (eso no se puede desde el propio
//! proceso), pero elimina la ambigüedad de "¿se cerró sola o la cerré yo?".

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Tope del log de diagnóstico; al superarlo se recorta conservando el final.
const MAX_LOG_BYTES: u64 = 128 * 1024;

/// Evidencia de la corrida anterior, si murió sucia.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirtyRun {
    /// Contenido del marcador que dejó: `<pid> <timestamp rfc3339>`.
    pub marker: String,
}

#[derive(Debug)]
struct RunClaim {
    marker_path: PathBuf,
    token: String,
}

/// Identidad de esta instancia. El archivo se vuelve una lease de escritor:
/// la instancia más nueva toma ownership y las anteriores dejan de persistir.
/// Así dos binarios abiertos (por ejemplo release + bundle) no alternan
/// snapshots distintos y hacen que un proyecto parezca desaparecer.
static RUN_CLAIM: OnceLock<RunClaim> = OnceLock::new();

/// Keeps ownership stable for the entire durable write, not just its preflight.
pub struct RunWriteGuard {
    _file: Option<std::fs::File>,
}

fn lock_run_dir(dir: &Path) -> std::io::Result<RunWriteGuard> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join("run.lock");
    let mut options = std::fs::OpenOptions::new();
    options.create(true).read(true).write(true).truncate(false);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(0);
    }
    let started = std::time::Instant::now();
    loop {
        match options.open(&path) {
            Ok(file) => {
                #[cfg(unix)]
                {
                    use std::os::fd::AsRawFd;
                    // SAFETY: the descriptor stays owned by the guard. Closing
                    // it releases the advisory process lock on every exit path.
                    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0
                    {
                        let error = std::io::Error::last_os_error();
                        if error.kind() != std::io::ErrorKind::WouldBlock {
                            return Err(error);
                        }
                        drop(file);
                    } else {
                        return Ok(RunWriteGuard { _file: Some(file) });
                    }
                }
                #[cfg(not(unix))]
                return Ok(RunWriteGuard { _file: Some(file) });
            }
            #[cfg(windows)]
            Err(error) if matches!(error.raw_os_error(), Some(32 | 33)) => {}
            Err(error) => return Err(error),
        }
        if started.elapsed() >= std::time::Duration::from_secs(5) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "persistence ownership lock timed out",
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

pub fn acquire_write_guard() -> std::io::Result<Option<RunWriteGuard>> {
    let Some(claim) = RUN_CLAIM.get() else {
        return Ok(Some(RunWriteGuard { _file: None }));
    };
    let dir = claim
        .marker_path
        .parent()
        .ok_or_else(|| std::io::Error::other("run marker has no parent"))?;
    let guard = lock_run_dir(dir)?;
    Ok(marker_has_token(&claim.marker_path, &claim.token).then_some(guard))
}

fn data_dir() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "terminal-app").map(|dirs| dirs.data_dir().to_path_buf())
}

fn marker_path_in(dir: &Path) -> PathBuf {
    dir.join("run.marker")
}

fn log_path_in(dir: &Path) -> PathBuf {
    dir.join("runs.log")
}

/// Marca el comienzo de una corrida. Devuelve la evidencia de la anterior si
/// no cerró limpia.
pub fn begin_run() -> Option<DirtyRun> {
    let dir = data_dir()?;
    let pid = std::process::id();
    let timestamp = chrono::Utc::now().to_rfc3339();
    let token = uuid::Uuid::new_v4().to_string();
    let dirty = claim_run_in(&dir, pid, &token, &timestamp);
    let _ = RUN_CLAIM.set(RunClaim {
        marker_path: marker_path_in(&dir),
        token,
    });
    dirty
}

/// Marca el final limpio de la corrida actual.
pub fn end_run_clean() {
    let Some(claim) = RUN_CLAIM.get() else {
        return;
    };
    let Ok(Some(_guard)) = acquire_write_guard() else {
        return;
    };
    // Una instancia anterior nunca debe borrar el marker de la nueva.
    if marker_has_token(&claim.marker_path, &claim.token) {
        let _ = std::fs::remove_file(&claim.marker_path);
    }
}

/// Guard central para todo write durable del proceso. Los tests y utilidades
/// sin una corrida real no tienen claim y conservan su comportamiento normal.
pub fn current_process_may_write() -> bool {
    RUN_CLAIM
        .get()
        .map(|claim| marker_has_token(&claim.marker_path, &claim.token))
        .unwrap_or(true)
}

pub fn begin_run_in(dir: &Path, pid: u32, timestamp: &str) -> Option<DirtyRun> {
    let _ = std::fs::create_dir_all(dir);
    let marker = marker_path_in(dir);

    let previous = std::fs::read_to_string(&marker)
        .ok()
        .map(|contents| contents.trim().to_owned())
        .filter(|contents| !contents.is_empty());

    if let Some(previous) = &previous {
        append_log(
            dir,
            &format!("{timestamp} corrida anterior murió sin cierre limpio (marker: {previous})\n"),
        );
    }

    // El marcador nuevo reemplaza al viejo: sólo interesa la última corrida.
    let _ = std::fs::write(&marker, format!("{pid} {timestamp}\n"));

    previous.map(|marker| DirtyRun { marker })
}

fn claim_run_in(dir: &Path, pid: u32, token: &str, timestamp: &str) -> Option<DirtyRun> {
    let _guard = match lock_run_dir(dir) {
        Ok(guard) => guard,
        Err(error) => {
            log::error!("failed to claim persistence ownership: {error}");
            return None;
        }
    };
    let marker = marker_path_in(dir);
    let previous = std::fs::read_to_string(&marker)
        .ok()
        .map(|contents| contents.trim().to_owned())
        .filter(|contents| !contents.is_empty());

    let previous_is_live = previous
        .as_deref()
        .and_then(marker_pid)
        .is_some_and(process_is_alive);
    let dirty = if let Some(previous) = previous {
        if previous_is_live {
            append_log(
                dir,
                &format!(
                    "{timestamp} otra instancia tomó la lease de persistencia (marker anterior: {previous})\n"
                ),
            );
            None
        } else {
            append_log(
                dir,
                &format!(
                    "{timestamp} corrida anterior murió sin cierre limpio (marker: {previous})\n"
                ),
            );
            Some(DirtyRun { marker: previous })
        }
    } else {
        None
    };

    let _ = std::fs::write(&marker, format!("{pid} {token} {timestamp}\n"));
    dirty
}

fn marker_pid(marker: &str) -> Option<u32> {
    marker.split_whitespace().next()?.parse().ok()
}

fn marker_has_token(path: &Path, token: &str) -> bool {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|marker| marker.split_whitespace().nth(1).map(str::to_owned))
        .as_deref()
        == Some(token)
}

#[cfg(unix)]
fn process_is_alive(pid: u32) -> bool {
    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };
    // SAFETY: `kill(pid, 0)` no envía una señal; sólo consulta existencia y
    // permisos para un PID entero validado.
    let result = unsafe { libc::kill(pid, 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(windows)]
fn process_is_alive(pid: u32) -> bool {
    type Handle = *mut std::ffi::c_void;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> Handle;
        fn GetExitCodeProcess(process: Handle, code: *mut u32) -> i32;
        fn CloseHandle(handle: Handle) -> i32;
    }
    // SAFETY: query-only access; every successful handle is closed exactly once.
    unsafe {
        let handle = OpenProcess(0x1000, 0, pid);
        if handle.is_null() {
            return std::io::Error::last_os_error().raw_os_error() == Some(5);
        }
        let mut code = 0;
        let queried = GetExitCodeProcess(handle, &mut code) != 0;
        CloseHandle(handle);
        queried && code == 259
    }
}

#[cfg(not(any(unix, windows)))]
fn process_is_alive(_pid: u32) -> bool {
    false
}

pub fn end_run_clean_in(dir: &Path) {
    let _ = std::fs::remove_file(marker_path_in(dir));
}

fn append_log(dir: &Path, line: &str) {
    let path = log_path_in(dir);
    trim_log_if_needed(&path);
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        use std::io::Write;
        let _ = file.write_all(line.as_bytes());
    }
}

/// Mantiene el log acotado conservando la mitad final (las corridas más
/// recientes son las que sirven para diagnosticar).
fn trim_log_if_needed(path: &Path) {
    let Ok(meta) = std::fs::metadata(path) else {
        return;
    };
    if meta.len() <= MAX_LOG_BYTES {
        return;
    }
    let Ok(contents) = std::fs::read_to_string(path) else {
        return;
    };
    let mut keep_from = contents.len() / 2;
    while !contents.is_char_boundary(keep_from) {
        keep_from += 1;
    }
    // Alinear a un borde de línea para no dejar una entrada partida.
    let aligned = contents[keep_from..]
        .find('\n')
        .map(|offset| keep_from + offset + 1)
        .unwrap_or(keep_from);
    let _ = std::fs::write(path, &contents[aligned..]);
}

#[cfg(test)]
mod tests {
    use super::{
        begin_run_in, claim_run_in, end_run_clean_in, log_path_in, marker_has_token, marker_path_in,
    };

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("run-marker-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    #[test]
    fn first_run_reports_nothing() {
        let dir = temp_dir("first");
        let dirty = begin_run_in(&dir, 100, "2026-08-05T00:00:00Z");
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(dirty, None);
    }

    #[test]
    fn a_clean_exit_leaves_no_evidence() {
        let dir = temp_dir("clean");
        begin_run_in(&dir, 100, "2026-08-05T00:00:00Z");
        end_run_clean_in(&dir);
        let dirty = begin_run_in(&dir, 200, "2026-08-05T00:05:00Z");
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(dirty, None, "clean exit must not be flagged");
    }

    #[test]
    fn dying_without_cleanup_is_detected_on_the_next_start() {
        let dir = temp_dir("dirty");
        begin_run_in(&dir, 100, "2026-08-05T00:00:00Z");
        // Sin end_run_clean_in: simulamos kill -9 / crash / OOM.
        let dirty = begin_run_in(&dir, 200, "2026-08-05T00:05:00Z");
        let logged = std::fs::read_to_string(log_path_in(&dir)).unwrap_or_default();
        let _ = std::fs::remove_dir_all(&dir);

        let dirty = dirty.expect("the dirty death must be reported");
        assert!(dirty.marker.contains("100"), "got {:?}", dirty.marker);
        assert!(
            logged.contains("murió sin cierre limpio"),
            "diagnostic log missing: {logged:?}"
        );
    }

    #[test]
    fn the_marker_always_holds_the_current_run() {
        let dir = temp_dir("current");
        begin_run_in(&dir, 100, "2026-08-05T00:00:00Z");
        begin_run_in(&dir, 200, "2026-08-05T00:05:00Z");
        let marker = std::fs::read_to_string(marker_path_in(&dir)).expect("marker");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(marker.starts_with("200 "), "got {marker:?}");
    }

    #[test]
    fn an_empty_stale_marker_is_not_reported() {
        let dir = temp_dir("empty");
        std::fs::write(marker_path_in(&dir), "").expect("write");
        let dirty = begin_run_in(&dir, 100, "2026-08-05T00:00:00Z");
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(dirty, None);
    }

    #[test]
    fn ownership_is_scoped_to_the_exact_instance_token() {
        let dir = temp_dir("ownership-token");
        let marker = marker_path_in(&dir);
        std::fs::write(&marker, "100 token-new 2026-08-05T00:00:00Z\n").expect("write");
        assert!(marker_has_token(&marker, "token-new"));
        assert!(!marker_has_token(&marker, "token-old"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn a_live_instance_is_a_takeover_not_a_false_crash() {
        let dir = temp_dir("live-takeover");
        let marker = marker_path_in(&dir);
        std::fs::write(
            &marker,
            format!("{} token-old 2026-08-05T00:00:00Z\n", std::process::id()),
        )
        .expect("write");
        let dirty = claim_run_in(
            &dir,
            std::process::id(),
            "token-new",
            "2026-08-05T00:05:00Z",
        );
        assert_eq!(dirty, None);
        assert!(marker_has_token(&marker, "token-new"));
        let log = std::fs::read_to_string(log_path_in(&dir)).expect("takeover log");
        assert!(log.contains("lease de persistencia"), "got {log:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_diagnostic_log_stays_bounded() {
        let dir = temp_dir("bounded");
        // Forzar el recorte con un log gigante preexistente.
        let big = "x".repeat(300 * 1024) + "\nlinea final\n";
        std::fs::write(log_path_in(&dir), &big).expect("write");
        begin_run_in(&dir, 100, "2026-08-05T00:00:00Z");
        begin_run_in(&dir, 200, "2026-08-05T00:05:00Z");
        let size = std::fs::metadata(log_path_in(&dir)).expect("meta").len();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(size < 200 * 1024, "log was not trimmed: {size} bytes");
    }

    #[test]
    fn takeover_waits_for_the_previous_durable_write() {
        let dir = temp_dir("writer-lock");
        claim_run_in(&dir, std::process::id(), "old", "before");
        let guard = super::lock_run_dir(&dir).expect("writer lock");
        let takeover_dir = dir.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            claim_run_in(&takeover_dir, std::process::id(), "new", "after");
            tx.send(()).unwrap();
        });
        assert!(rx
            .recv_timeout(std::time::Duration::from_millis(80))
            .is_err());
        assert!(marker_has_token(&marker_path_in(&dir), "old"));
        drop(guard);
        rx.recv_timeout(std::time::Duration::from_secs(2))
            .expect("takeover completes");
        thread.join().unwrap();
        assert!(marker_has_token(&marker_path_in(&dir), "new"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn trimming_a_multibyte_log_never_slices_inside_a_character() {
        let dir = temp_dir("utf8-log");
        let path = log_path_in(&dir);
        std::fs::write(&path, "é".repeat(100_001)).unwrap();
        super::trim_log_if_needed(&path);
        assert_eq!(std::fs::read_to_string(path).unwrap(), "é".repeat(50_000));
        let _ = std::fs::remove_dir_all(dir);
    }
}

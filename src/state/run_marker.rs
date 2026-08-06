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

/// Tope del log de diagnóstico; al superarlo se recorta conservando el final.
const MAX_LOG_BYTES: u64 = 128 * 1024;

/// Evidencia de la corrida anterior, si murió sucia.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirtyRun {
    /// Contenido del marcador que dejó: `<pid> <timestamp rfc3339>`.
    pub marker: String,
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
    begin_run_in(&dir, std::process::id(), &chrono::Utc::now().to_rfc3339())
}

/// Marca el final limpio de la corrida actual.
pub fn end_run_clean() {
    if let Some(dir) = data_dir() {
        end_run_clean_in(&dir);
    }
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
    let keep_from = contents.len() / 2;
    // Alinear a un borde de línea para no dejar una entrada partida.
    let aligned = contents[keep_from..]
        .find('\n')
        .map(|offset| keep_from + offset + 1)
        .unwrap_or(keep_from);
    let _ = std::fs::write(path, &contents[aligned..]);
}

#[cfg(test)]
mod tests {
    use super::{begin_run_in, end_run_clean_in, log_path_in, marker_path_in};

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
}

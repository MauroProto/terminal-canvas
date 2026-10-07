//! Notas por línea sobre el diff, para mandarlas al agente como feedback
//! localizado (equivalente a los diff comments de Orca,
//! `src/shared/diff-comments-format.ts`).
//!
//! Dos reglas de Orca que se preservan porque son el corazón del flujo:
//! - **`sent_at` marca "ya entregada"**; editar el cuerpo borra `sent_at`,
//!   así la nota se re-encola sola sin intervención del usuario.
//! - El **formato del prompt es un contrato** byte-exacto con el agente, no
//!   un detalle de presentación; el test lo fija.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// The same serialized limit applies to reads, imports and saves.
const MAX_NOTES_FILE_BYTES: usize = 4 * 1024 * 1024;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Una nota sobre el diff. `line == 0` significa "comentario al archivo
/// entero" (convención de Orca); si no, es el número de línea en el archivo
/// **nuevo**. `start_line` convierte la nota en un rango (`start_line..=line`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiffNote {
    pub id: Uuid,
    pub file_path: String,
    #[serde(default)]
    pub start_line: Option<u32>,
    pub line: u32,
    #[serde(default)]
    pub old_side: bool,
    #[serde(default)]
    pub review_identity: Option<String>,
    pub body: String,
    pub created_at: DateTime<Utc>,
    /// `Some` = ya se mandó al agente. Editar el cuerpo la devuelve a `None`.
    #[serde(default)]
    pub sent_at: Option<DateTime<Utc>>,
}

/// Colección de notas de un repo, persistida en JSON junto al resto del
/// estado de la app.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DiffNotes {
    #[serde(default)]
    pub notes: Vec<DiffNote>,
}

impl DiffNotes {
    pub fn add(&mut self, file_path: &str, start_line: Option<u32>, line: u32, body: &str) -> Uuid {
        let id = Uuid::new_v4();
        self.notes.push(DiffNote {
            id,
            file_path: file_path.to_owned(),
            start_line,
            line,
            old_side: false,
            review_identity: None,
            body: body.trim().to_owned(),
            created_at: Utc::now(),
            sent_at: None,
        });
        id
    }

    pub fn add_on_side(&mut self, file_path: &str, line: u32, body: &str, old_side: bool) -> Uuid {
        let id = self.add(file_path, None, line, body);
        if let Some(note) = self.notes.iter_mut().find(|note| note.id == id) {
            note.old_side = old_side;
        }
        id
    }

    /// Edita el cuerpo y **borra `sent_at`**: una nota editada después de
    /// enviada es una nota nueva y se re-encola sola.
    pub fn edit(&mut self, id: Uuid, body: &str) {
        if let Some(note) = self.notes.iter_mut().find(|note| note.id == id) {
            note.body = body.trim().to_owned();
            note.sent_at = None;
        }
    }

    pub fn remove(&mut self, id: Uuid) {
        self.notes.retain(|note| note.id != id);
    }

    /// Las notas que todavía no llegaron al agente.
    pub fn pending(&self) -> Vec<&DiffNote> {
        self.notes
            .iter()
            .filter(|note| note.sent_at.is_none())
            .collect()
    }

    /// Marca como enviadas las notas elegidas (las demás no se tocan).
    pub fn mark_sent(&mut self, ids: &[Uuid], now: DateTime<Utc>) {
        for note in self.notes.iter_mut() {
            if ids.contains(&note.id) {
                note.sent_at = Some(now);
            }
        }
    }

    /// Saca las notas que apuntan a un archivo que ya no está en el diff:
    /// el feedback localizado sobre código que ya no cambió no tiene destino.
    pub fn prune_missing_files(&mut self, current_files: &[String]) {
        self.notes
            .retain(|note| current_files.iter().any(|file| file == &note.file_path));
    }
}

/// Formato determinístico con que una nota viaja al agente. Es el contrato
/// (copiado de Orca), no prosa: cambiarlo rompe lo que el agente ya aprendió
/// a interpretar.
pub fn format_note(note: &DiffNote) -> String {
    let mut out = format!("File: {}", note.file_path);
    if note.line > 0 {
        match note.start_line {
            Some(start) if start != note.line => {
                out.push_str(&format!("\nLines: {start}-{}", note.line));
            }
            _ => {
                out.push_str(&format!("\nLines: {}", note.line));
            }
        }
    }
    if note.old_side {
        out.push_str("\nSide: old (removed code)");
    }
    out.push_str(&format!("\nUser comment: \"{}\"", escape_body(&note.body)));
    out
}

/// El cuerpo va entre comillas dobles: `"` se escapa y los saltos de línea se
/// colapsan a `\n` literal para que la nota sea siempre una sola unidad.
fn escape_body(body: &str) -> String {
    body.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
}

fn notes_dir() -> Option<PathBuf> {
    Some(crate::utils::app_paths::data_dir()?.join("diff-notes"))
}

/// Identity of the canonical repository path, without separator collisions.
fn repo_slug(repo_root: &Path) -> String {
    use sha2::{Digest, Sha256};
    let identity = std::fs::canonicalize(repo_root).unwrap_or_else(|_| repo_root.to_path_buf());
    format!(
        "{:x}",
        Sha256::digest(identity.as_os_str().as_encoded_bytes())
    )
}

fn notes_file(repo_root: &Path) -> Option<PathBuf> {
    Some(notes_dir()?.join(format!("{}.json", repo_slug(repo_root))))
}

fn legacy_notes_file(repo_root: &Path) -> Option<PathBuf> {
    let slug = repo_root.to_string_lossy().replace(['/', '\\', ':'], "-");
    Some(notes_dir()?.join(format!("{slug}.json")))
}

/// Legacy files have no reliable repository identity. Only load after the
/// user explicitly chooses to import into the currently reviewed repository.
pub fn load_legacy_notes(repo_root: &Path) -> anyhow::Result<DiffNotes> {
    let path = legacy_notes_file(repo_root)
        .ok_or_else(|| anyhow::anyhow!("No se pudo resolver el directorio de notas"))?;
    load_existing_notes_from_path(&path, MAX_NOTES_FILE_BYTES)
}

fn load_existing_notes_from_path(path: &Path, limit: usize) -> anyhow::Result<DiffNotes> {
    let bytes = read_notes_bytes(path, limit).map_err(|error| notes_read_error(path, error))?;
    parse_notes_bytes(path, &bytes)
}

pub fn legacy_notes_available(repo_root: &Path) -> bool {
    legacy_notes_file(repo_root).is_some_and(|path| path.is_file())
}

/// Persiste las notas del repo (escritura durable: tmp+fsync+rename+ring).
pub fn save_notes(repo_root: &Path, notes: &DiffNotes) -> anyhow::Result<()> {
    let Some(path) = notes_file(repo_root) else {
        anyhow::bail!("No se pudo resolver el directorio de notas");
    };
    save_notes_to_path(&path, notes)
}

/// An explicit destination keeps storage tests independent of the profile.
pub fn save_notes_to_path(path: &Path, notes: &DiffNotes) -> anyhow::Result<()> {
    save_notes_to_path_with_limit(path, notes, MAX_NOTES_FILE_BYTES)
}

fn save_notes_to_path_with_limit(
    path: &Path,
    notes: &DiffNotes,
    limit: usize,
) -> anyhow::Result<()> {
    use anyhow::Context;
    if notes.notes.is_empty() {
        // Sin notas no queda archivo: un repo limpio no arrastra notas viejas.
        match std::fs::remove_file(path) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(err.into()),
        }
        return Ok(());
    }
    let mut buffer = LimitedNotesBuffer {
        bytes: Vec::new(),
        limit,
    };
    serde_json::to_writer_pretty(&mut buffer, notes).with_context(|| {
        format!(
            "No se pudo guardar {}: el JSON de notas debe ocupar como máximo {limit} bytes",
            path.display()
        )
    })?;
    crate::state::durable_write::write_durable(path, &buffer.bytes)?;
    Ok(())
}

/// An absent file is a new repository. Unreadable or malformed notes must
/// remain an error: exposing an empty editable collection could overwrite them.
pub fn load_notes(repo_root: &Path) -> anyhow::Result<DiffNotes> {
    let path = notes_file(repo_root).ok_or_else(|| {
        anyhow::anyhow!(
            "No se pudo resolver el archivo de notas de {}",
            repo_root.display()
        )
    })?;
    load_notes_from_path(&path)
}

/// Read an explicit destination without changing either its bytes or profile.
pub fn load_notes_from_path(path: &Path) -> anyhow::Result<DiffNotes> {
    load_notes_from_path_with_limit(path, MAX_NOTES_FILE_BYTES)
}

fn load_notes_from_path_with_limit(path: &Path, limit: usize) -> anyhow::Result<DiffNotes> {
    let bytes = match read_notes_bytes(path, limit) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(DiffNotes::default())
        }
        Err(error) => return Err(notes_read_error(path, error)),
    };
    parse_notes_bytes(path, &bytes)
}

fn parse_notes_bytes(path: &Path, bytes: &[u8]) -> anyhow::Result<DiffNotes> {
    use anyhow::Context;
    serde_json::from_slice(bytes).with_context(|| {
        format!(
            "El archivo de notas {} no contiene JSON válido",
            path.display()
        )
    })
}

fn notes_read_error(path: &Path, error: std::io::Error) -> anyhow::Error {
    let message = format!("No se pudo leer {}: {error}", path.display());
    anyhow::Error::new(error).context(message)
}

fn notes_size_error(limit: usize) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        format!("El archivo de notas supera el límite de {limit} bytes"),
    )
}

fn read_notes_bytes(path: &Path, limit: usize) -> std::io::Result<Vec<u8>> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "Las notas deben estar en un archivo regular",
        ));
    }
    read_notes_bytes_with_limit(file, metadata.len(), limit)
}

fn read_notes_bytes_with_limit(
    reader: impl Read,
    declared_len: u64,
    limit: usize,
) -> std::io::Result<Vec<u8>> {
    if declared_len > limit as u64 {
        return Err(notes_size_error(limit));
    }
    let sentinel_limit = limit.checked_add(1).ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "Límite de notas inválido")
    })?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(declared_len as usize)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::OutOfMemory, error))?;
    reader.take(sentinel_limit as u64).read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(notes_size_error(limit));
    }
    Ok(bytes)
}

struct LimitedNotesBuffer {
    bytes: Vec<u8>,
    limit: usize,
}

impl Write for LimitedNotesBuffer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
            return Err(notes_size_error(self.limit));
        }
        self.bytes
            .try_reserve(bytes.len())
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::OutOfMemory, error))?;
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

// Explicit test paths and injected limits exercise the production I/O paths
// without resolving or modifying the user's notes profile.
#[cfg(test)]
pub(crate) fn test_load_notes_from_path_with_limit(
    path: &Path,
    limit: usize,
    require_existing: bool,
) -> anyhow::Result<DiffNotes> {
    if require_existing {
        load_existing_notes_from_path(path, limit)
    } else {
        load_notes_from_path_with_limit(path, limit)
    }
}

#[cfg(test)]
pub(crate) fn test_save_notes_to_path_with_limit(
    path: &Path,
    notes: &DiffNotes,
    limit: usize,
) -> anyhow::Result<()> {
    save_notes_to_path_with_limit(path, notes, limit)
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::{format_note, load_notes, save_notes, DiffNotes};

    #[test]
    fn note_storage_keys_do_not_collapse_separators_into_dashes() {
        assert_ne!(
            super::repo_slug(std::path::Path::new("/repos/a-b")),
            super::repo_slug(std::path::Path::new("/repos-a/b"))
        );
        assert_eq!(super::repo_slug(std::path::Path::new("a")).len(), 64);
    }

    #[test]
    fn removed_line_feedback_identifies_its_side() {
        let mut notes = DiffNotes::default();
        notes.add_on_side("a.rs", 7, "keep this behavior", true);
        let text = format_note(&notes.notes[0]);
        assert!(text.contains("Side: old (removed code)"));
        assert!(text.contains("Lines: 7"));
    }

    #[test]
    fn editing_a_sent_note_requeues_it() {
        // Regla central de Orca: editar borra sent_at → la nota vuelve a
        // pendientes y se reenvía.
        let mut notes = DiffNotes::default();
        let id = notes.add("src/foo.rs", None, 7, "revisar esto");
        notes.mark_sent(&[id], Utc::now());
        assert!(notes.pending().is_empty());

        notes.edit(id, "ahora con más contexto");
        let pending = notes.pending();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].body, "ahora con más contexto");
        assert_eq!(pending[0].sent_at, None);
    }

    #[test]
    fn pending_filters_out_sent_notes() {
        let mut notes = DiffNotes::default();
        let sent = notes.add("a.rs", None, 1, "ya fue");
        notes.add("b.rs", None, 2, "todavía no");
        notes.mark_sent(&[sent], Utc::now());

        let pending = notes.pending();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].file_path, "b.rs");
    }

    #[test]
    fn format_is_the_exact_contract() {
        // Byte-exacto: esto es lo que lee el agente.
        let mut notes = DiffNotes::default();
        let id = notes.add("src/foo.ts", Some(7), 12, "el texto, \"escapado\"");
        let note = notes.notes.iter().find(|note| note.id == id).unwrap();
        assert_eq!(
            format_note(note),
            "File: src/foo.ts\nLines: 7-12\nUser comment: \"el texto, \\\"escapado\\\"\""
        );
    }

    #[test]
    fn format_collapses_newlines_in_the_body() {
        let mut notes = DiffNotes::default();
        let id = notes.add("a.rs", None, 3, "línea uno\nlínea dos");
        let formatted = format_note(&notes.notes[0]);
        let _ = id;
        assert_eq!(
            formatted,
            "File: a.rs\nLines: 3\nUser comment: \"línea uno\\nlínea dos\""
        );
    }

    #[test]
    fn a_file_level_note_omits_the_lines_row() {
        // line == 0 es la convención de Orca para "comentario al archivo".
        let mut notes = DiffNotes::default();
        notes.add("src/whole.rs", None, 0, "revisar el enfoque general");
        assert_eq!(
            format_note(&notes.notes[0]),
            "File: src/whole.rs\nUser comment: \"revisar el enfoque general\""
        );
    }

    #[test]
    fn notes_round_trip_through_disk() {
        let dir = unique_dir();
        let mut notes = DiffNotes::default();
        notes.add("src/a.rs", Some(3), 9, "una nota");
        notes.add("src/b.rs", None, 1, "otra");

        save_notes(&dir, &notes).unwrap();
        let loaded = load_notes(&dir).unwrap();
        assert_eq!(loaded, notes);
        let _ = std::fs::remove_dir_all(dir_parent(&dir));
    }

    #[test]
    fn a_corrupt_notes_file_returns_an_error_without_modifying_it() {
        let dir = unique_dir();
        save_notes(&dir, &{
            let mut notes = DiffNotes::default();
            notes.add("a.rs", None, 1, "x");
            notes
        })
        .unwrap();
        // Corromper el principal.
        let file = notes_file_for(&dir);
        std::fs::write(&file, "{no es json").unwrap();

        let error = load_notes(&dir).unwrap_err();
        assert!(format!("{error:#}").contains(&file.display().to_string()));
        assert_eq!(std::fs::read(&file).unwrap(), b"{no es json");
        let _ = std::fs::remove_dir_all(dir_parent(&dir));
    }

    #[test]
    fn absent_notes_file_is_an_empty_collection_without_creating_a_file() {
        let directory = TemporaryNotesDirectory::new();
        let path = directory.0.join("missing.json");
        assert_eq!(
            super::load_notes_from_path(&path).unwrap(),
            DiffNotes::default()
        );
        assert!(!path.exists());
    }

    #[test]
    fn unreadable_notes_path_is_an_error_without_removing_its_contents() {
        let directory = TemporaryNotesDirectory::new();
        let path = directory.0.join("notes.json");
        std::fs::create_dir(&path).unwrap();
        let preserved = path.join("preserved");
        std::fs::write(&preserved, b"keep these bytes").unwrap();
        let error = super::load_notes_from_path(&path).unwrap_err();
        assert!(format!("{error:#}").contains(&path.display().to_string()));
        assert_eq!(std::fs::read(preserved).unwrap(), b"keep these bytes");
        assert!(path.is_dir());
    }

    #[test]
    fn malformed_notes_stay_untouched_and_can_be_read_after_explicit_repair() {
        let directory = TemporaryNotesDirectory::new();
        let path = directory.0.join("notes.json");
        std::fs::write(&path, b"{invalid JSON").unwrap();
        assert!(super::load_notes_from_path(&path).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"{invalid JSON");
        let mut repaired = DiffNotes::default();
        repaired.add("a.rs", None, 9, "Recovered note");
        super::save_notes_to_path(&path, &repaired).unwrap();
        assert_eq!(super::load_notes_from_path(&path).unwrap(), repaired);
    }

    #[test]
    fn saving_an_empty_collection_removes_the_file() {
        let dir = unique_dir();
        let mut notes = DiffNotes::default();
        notes.add("a.rs", None, 1, "x");
        save_notes(&dir, &notes).unwrap();
        assert!(notes_file_for(&dir).exists());

        save_notes(&dir, &DiffNotes::default()).unwrap();
        assert!(!notes_file_for(&dir).exists());
    }

    #[test]
    fn prune_drops_notes_for_files_no_longer_in_the_diff() {
        let mut notes = DiffNotes::default();
        notes.add("src/viejo.rs", None, 1, "ya no existe");
        notes.add("src/vivo.rs", None, 2, "sigue");
        notes.prune_missing_files(&["src/vivo.rs".to_owned()]);
        assert_eq!(notes.notes.len(), 1);
        assert_eq!(notes.notes[0].file_path, "src/vivo.rs");
    }

    #[test]
    fn bounded_note_reads_share_current_and_legacy_limits_without_changing_bytes() {
        let directory = TemporaryNotesDirectory::new();
        let path = directory.0.join("bounded.json");
        let backup = crate::state::durable_write::backup_path(&path, 0);
        std::fs::write(&backup, b"preserved backup").unwrap();
        let raw = br#"{"notes":[]}"#;
        std::fs::write(&path, raw).unwrap();
        let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
        for limit in [raw.len(), raw.len() + 1] {
            assert_eq!(
                super::load_notes_from_path_with_limit(&path, limit).unwrap(),
                DiffNotes::default()
            );
            assert_eq!(
                super::load_existing_notes_from_path(&path, limit).unwrap(),
                DiffNotes::default()
            );
        }
        assert_eq!(
            std::fs::metadata(&path).unwrap().modified().unwrap(),
            modified
        );
        for bytes in [raw.as_slice(), b"{malformed notes".as_slice()] {
            std::fs::write(&path, bytes).unwrap();
            let limit = bytes.len() - 1;
            for result in [
                super::load_notes_from_path_with_limit(&path, limit),
                super::load_existing_notes_from_path(&path, limit),
            ] {
                let error = result.unwrap_err().to_string();
                assert!(error.contains(&path.display().to_string()));
                assert!(error.contains(&format!("límite de {limit} bytes")));
                assert!(!error.contains("no contiene JSON válido"));
            }
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
        }
        std::fs::write(&path, raw).unwrap();
        let before = std::fs::metadata(&path).unwrap().modified().unwrap();
        assert!(super::load_notes_from_path_with_limit(&path, raw.len() - 1).is_err());
        assert_eq!(
            std::fs::metadata(&path).unwrap().modified().unwrap(),
            before
        );
        assert_eq!(std::fs::read(&backup).unwrap(), b"preserved backup");
    }

    #[test]
    fn bounded_missing_current_notes_are_empty_but_missing_legacy_notes_are_errors() {
        let directory = TemporaryNotesDirectory::new();
        let path = directory.0.join("absent.json");
        assert_eq!(
            super::load_notes_from_path_with_limit(&path, 16).unwrap(),
            DiffNotes::default()
        );
        let error = super::load_existing_notes_from_path(&path, 16).unwrap_err();
        assert!(format!("{error:#}").contains(&path.display().to_string()));
        assert!(!path.exists());
    }

    #[test]
    fn bounded_notes_reject_an_advertised_huge_file_before_reading() {
        struct NeverRead;
        impl std::io::Read for NeverRead {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                panic!("an excessive advertised size must not read or allocate its payload");
            }
        }
        let error = super::read_notes_bytes_with_limit(NeverRead, u64::MAX, 32).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("32 bytes"));
    }

    #[test]
    fn bounded_notes_reject_growth_after_metadata_without_accepting_a_json_prefix() {
        let raw = br#"{"notes":[]}"#;
        let mut grown = raw.to_vec();
        grown.extend_from_slice(b" more data");
        let consumed = std::cell::Cell::new(0);
        let reader = CountedNotesReader {
            inner: std::io::Cursor::new(grown),
            consumed: &consumed,
        };
        let error =
            super::read_notes_bytes_with_limit(reader, raw.len() as u64, raw.len()).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(consumed.get(), raw.len() + 1);
    }

    #[test]
    fn bounded_notes_retry_interrupted_reads_and_preserve_other_io_errors() {
        struct InterruptOnce {
            interrupted: bool,
            inner: std::io::Cursor<Vec<u8>>,
        }
        impl std::io::Read for InterruptOnce {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                if !self.interrupted {
                    self.interrupted = true;
                    return Err(std::io::ErrorKind::Interrupted.into());
                }
                std::io::Read::read(&mut self.inner, buffer)
            }
        }
        struct Denied;
        impl std::io::Read for Denied {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::ErrorKind::PermissionDenied.into())
            }
        }
        let raw = br#"{"notes":[]}"#;
        let reader = InterruptOnce {
            interrupted: false,
            inner: std::io::Cursor::new(raw.to_vec()),
        };
        assert_eq!(
            super::read_notes_bytes_with_limit(reader, raw.len() as u64, raw.len()).unwrap(),
            raw
        );
        let error = super::read_notes_bytes_with_limit(Denied, 2, 16).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn bounded_note_serialization_preserves_pretty_json_and_rejects_before_writing() {
        let directory = TemporaryNotesDirectory::new();
        let path = directory.0.join("notes.json");
        let backup = crate::state::durable_write::backup_path(&path, 0);
        std::fs::write(&path, b"preserve current bytes").unwrap();
        std::fs::write(&backup, b"preserve backup bytes").unwrap();
        let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
        let names_before = std::fs::read_dir(&directory.0).unwrap().count();
        let mut notes = DiffNotes::default();
        notes.add(
            "unicode.rs",
            Some(2),
            4,
            "Español: ñ, \"quotes\", newline\n and \\",
        );
        let golden = serde_json::to_vec_pretty(&notes).unwrap();
        let error =
            super::save_notes_to_path_with_limit(&path, &notes, golden.len() - 1).unwrap_err();
        assert!(format!("{error:#}").contains(&path.display().to_string()));
        assert_eq!(std::fs::read(&path).unwrap(), b"preserve current bytes");
        assert_eq!(std::fs::read(&backup).unwrap(), b"preserve backup bytes");
        assert_eq!(
            std::fs::metadata(&path).unwrap().modified().unwrap(),
            modified
        );
        assert_eq!(
            std::fs::read_dir(&directory.0).unwrap().count(),
            names_before
        );
        super::save_notes_to_path_with_limit(&path, &notes, golden.len()).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), golden);
        assert_eq!(
            super::load_notes_from_path_with_limit(&path, golden.len()).unwrap(),
            notes
        );
    }

    #[test]
    fn bounded_empty_notes_keep_the_existing_explicit_deletion_contract() {
        let directory = TemporaryNotesDirectory::new();
        let path = directory.0.join("notes.json");
        std::fs::write(&path, b"previous collection").unwrap();
        super::save_notes_to_path_with_limit(&path, &DiffNotes::default(), 0).unwrap();
        assert!(!path.exists());
    }

    struct CountedNotesReader<'a> {
        inner: std::io::Cursor<Vec<u8>>,
        consumed: &'a std::cell::Cell<usize>,
    }

    impl std::io::Read for CountedNotesReader<'_> {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            let size = std::io::Read::read(&mut self.inner, buffer)?;
            self.consumed.set(self.consumed.get() + size);
            Ok(size)
        }
    }

    struct TemporaryNotesDirectory(std::path::PathBuf);

    impl TemporaryNotesDirectory {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("tc-notes-read-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TemporaryNotesDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    // Los helpers de abajo reproducen el slug interno para localizar el
    // archivo desde los tests sin exportarlo.
    fn unique_dir() -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("diff-notes-test-{}", uuid::Uuid::new_v4()));
        // Simular un repo: cualquier path sirve como repo_root de prueba.
        root.join("repo").join("sub").join("proyecto")
    }

    fn dir_parent(repo: &std::path::Path) -> std::path::PathBuf {
        // unique_dir anida tres niveles; el dir temporal es el tatarabuelo.
        repo.parent()
            .and_then(|p| p.parent())
            .and_then(|p| p.parent())
            .unwrap()
            .to_path_buf()
    }

    fn notes_file_for(repo_root: &std::path::Path) -> std::path::PathBuf {
        super::notes_file(repo_root).unwrap()
    }
}

//! Notas por línea sobre el diff, para mandarlas al agente como feedback
//! localizado (equivalente a los diff comments de Orca,
//! `src/shared/diff-comments-format.ts`).
//!
//! Dos reglas de Orca que se preservan porque son el corazón del flujo:
//! - **`sent_at` marca "ya entregada"**; editar el cuerpo borra `sent_at`,
//!   así la nota se re-encola sola sin intervención del usuario.
//! - El **formato del prompt es un contrato** byte-exacto con el agente, no
//!   un detalle de presentación; el test lo fija.

use std::path::{Path, PathBuf};

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
    let dirs = directories::ProjectDirs::from("", "", "terminal-app")?;
    Some(dirs.data_dir().join("diff-notes"))
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
    Ok(serde_json::from_slice(&std::fs::read(path)?)?)
}

pub fn legacy_notes_available(repo_root: &Path) -> bool {
    legacy_notes_file(repo_root).is_some_and(|path| path.is_file())
}

/// Persiste las notas del repo (escritura durable: tmp+fsync+rename+ring).
pub fn save_notes(repo_root: &Path, notes: &DiffNotes) -> anyhow::Result<()> {
    let Some(path) = notes_file(repo_root) else {
        anyhow::bail!("No se pudo resolver el directorio de notas");
    };
    if notes.notes.is_empty() {
        // Sin notas no queda archivo: un repo limpio no arrastra notas viejas.
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(err.into()),
        }
        return Ok(());
    }
    let bytes = serde_json::to_vec_pretty(notes)?;
    crate::state::durable_write::write_durable(&path, &bytes)?;
    Ok(())
}

/// Carga las notas del repo; archivo ausente o corrupto → lista vacía (nunca
/// un panic: las notas son feedback, no estado crítico).
pub fn load_notes(repo_root: &Path) -> DiffNotes {
    let Some(path) = notes_file(repo_root) else {
        return DiffNotes::default();
    };
    let Ok(bytes) = std::fs::read(&path) else {
        return DiffNotes::default();
    };
    serde_json::from_slice(&bytes).unwrap_or_default()
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
        let loaded = load_notes(&dir);
        assert_eq!(loaded, notes);
        let _ = std::fs::remove_dir_all(dir_parent(&dir));
    }

    #[test]
    fn a_corrupt_notes_file_yields_an_empty_list_without_panicking() {
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

        assert_eq!(load_notes(&dir), DiffNotes::default());
        let _ = std::fs::remove_dir_all(dir_parent(&dir));
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

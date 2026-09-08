//! Persistencia del scrollback por panel: al cerrar la app se guarda el
//! historial de cada terminal y al volver a abrirla se reinyecta en el grid,
//! así el panel restaurado muestra lo que había en vez de un rectángulo negro
//! (equivalente al "scrollback that survives restarts" de Orca).
//!
//! El archivo se guarda por `panel_id`, no por título ni cwd, para que dos
//! paneles del mismo proyecto no se pisen entre sí.

use std::path::{Path, PathBuf};

use uuid::Uuid;

/// Tope por panel. El scrollback se recorta desde el principio (se conservan
/// las últimas líneas, que son las que el usuario quiere ver).
pub const MAX_PERSISTED_BYTES: usize = 256 * 1024;
const CHECKPOINT_MAGIC: &[u8] = b"TC-SCROLLBACK\x00\x02";

pub fn scrollback_dir() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("MI_TERMINAL_SCROLLBACK_DIR") {
        if !path.trim().is_empty() {
            return Some(PathBuf::from(path));
        }
    }
    let dirs = directories::ProjectDirs::from("", "", "terminal-app")?;
    Some(dirs.data_dir().join("scrollback"))
}

/// Nombre de archivo de un panel. Usa el UUID en hexadecimal, que nunca
/// contiene separadores de path.
pub fn scrollback_file_name(panel_id: Uuid) -> String {
    format!("{}.txt", panel_id.simple())
}

/// Nombre de archivo de una **hoja** de un panel con splits (P2.11, T4).
///
/// `Some(leaf)` es siempre el formato canónico, incluso para la raíz. `None`
/// conserva exclusivamente el nombre histórico como fallback de lectura, de
/// modo que la migración no destruye archivos de versiones anteriores.
pub fn scrollback_leaf_file_name(panel_id: Uuid, leaf_id: Option<Uuid>) -> String {
    match leaf_id {
        Some(leaf) => format!("{}-{}.txt", panel_id.simple(), leaf.simple()),
        None => scrollback_file_name(panel_id),
    }
}

/// Nombre del log incremental de un panel (P1.7).
pub fn scrollback_log_file_name(panel_id: Uuid) -> String {
    format!("{}.mtlg", panel_id.simple())
}

pub fn scrollback_leaf_log_file_name(panel_id: Uuid, leaf_id: Option<Uuid>) -> String {
    match leaf_id {
        Some(leaf) => format!("{}-{}.mtlg", panel_id.simple(), leaf.simple()),
        None => scrollback_log_file_name(panel_id),
    }
}

/// Nombre del archivo de generation de un panel (P1.7).
pub fn scrollback_gen_file_name(panel_id: Uuid) -> String {
    format!("{}.gen", panel_id.simple())
}

pub fn scrollback_leaf_gen_file_name(panel_id: Uuid, leaf_id: Option<Uuid>) -> String {
    match leaf_id {
        Some(leaf) => format!("{}-{}.gen", panel_id.simple(), leaf.simple()),
        None => scrollback_gen_file_name(panel_id),
    }
}

/// Recorta el historial al tope conservando el **final**, alineado a un borde
/// de línea para no restaurar una línea cortada al medio, y sin partir nunca
/// una secuencia SGR (`\x1b[...m`): si el corte cae adentro de una, avanza
/// hasta su `m` de cierre antes de alinear a la línea siguiente.
pub fn clamp_scrollback(text: &str, max_bytes: usize) -> &str {
    if text.len() <= max_bytes {
        return text;
    }
    let cut = text.len() - max_bytes;
    // `cut` puede caer en medio de un carácter multibyte: avanzamos al próximo
    // borde de carácter válido.
    let mut start = cut;
    while start < text.len() && !text.is_char_boundary(start) {
        start += 1;
    }
    // Si el corte cayó dentro de una secuencia SGR, el `\x1b` que la abre
    // quedó en el prefijo descartado: buscamos hacia atrás el escape más
    // cercano y avanzamos hasta su `m` de cierre para no emitir un `;204;0;0m`
    // huérfano que el parser mostraría como texto.
    if let Some(escape_start) = text[..start].rfind('\x1b') {
        if let Some(offset) = text[escape_start..].find('m') {
            let escape_end = escape_start + offset + 1;
            if escape_end > start {
                start = escape_end;
            }
        }
    }
    let tail = &text[start..];
    // Descartamos la primera línea parcial.
    match tail.find('\n') {
        Some(index) => &tail[index + 1..],
        None => tail,
    }
}

/// Guarda el scrollback de una hoja concreta (P2.11, T4). `Some(leaf)` es el
/// formato canónico; `None` sólo se usa para compatibilidad histórica.
pub fn save_leaf_scrollback(
    dir: &Path,
    panel_id: Uuid,
    leaf_id: Option<Uuid>,
    text: &str,
) -> anyhow::Result<()> {
    let name = scrollback_leaf_file_name(panel_id, leaf_id);
    if text.trim().is_empty() {
        let _ = std::fs::remove_file(dir.join(&name));
        return Ok(());
    }
    std::fs::create_dir_all(dir)?;
    let clamped = clamp_scrollback(text, MAX_PERSISTED_BYTES);
    crate::state::durable_write::write_atomic(&dir.join(&name), clamped.as_bytes())?;
    Ok(())
}

/// Guarda checkpoint y generation en una sola escritura atómica. Esto evita
/// que un crash entre dos sidecars haga reaplicar un log que ya está incluido
/// en el checkpoint.
pub fn save_leaf_scrollback_versioned(
    dir: &Path,
    panel_id: Uuid,
    leaf_id: Option<Uuid>,
    generation: u32,
    text: &str,
) -> anyhow::Result<()> {
    std::fs::create_dir_all(dir)?;
    let clamped = clamp_scrollback(text, MAX_PERSISTED_BYTES);
    let mut bytes = Vec::with_capacity(CHECKPOINT_MAGIC.len() + 4 + clamped.len());
    bytes.extend_from_slice(CHECKPOINT_MAGIC);
    bytes.extend_from_slice(&generation.to_le_bytes());
    bytes.extend_from_slice(clamped.as_bytes());
    crate::state::durable_write::write_atomic(
        &dir.join(scrollback_leaf_file_name(panel_id, leaf_id)),
        &bytes,
    )?;
    Ok(())
}

/// Lee tanto checkpoints versionados como los `.txt` históricos.
pub fn load_leaf_scrollback_checkpoint(
    dir: &Path,
    panel_id: Uuid,
    leaf_id: Option<Uuid>,
) -> Option<(Option<u32>, String)> {
    let bytes = std::fs::read(dir.join(scrollback_leaf_file_name(panel_id, leaf_id))).ok()?;
    if bytes.starts_with(CHECKPOINT_MAGIC) && bytes.len() >= CHECKPOINT_MAGIC.len() + 4 {
        let offset = CHECKPOINT_MAGIC.len();
        let generation = u32::from_le_bytes(bytes[offset..offset + 4].try_into().ok()?);
        let text = String::from_utf8_lossy(&bytes[offset + 4..]).into_owned();
        Some((Some(generation), text))
    } else {
        Some((None, String::from_utf8_lossy(&bytes).into_owned()))
    }
}

/// Carga el scrollback de una hoja concreta.
pub fn load_leaf_scrollback(dir: &Path, panel_id: Uuid, leaf_id: Option<Uuid>) -> Option<String> {
    let (_, text) = load_leaf_scrollback_checkpoint(dir, panel_id, leaf_id)?;
    if text.trim().is_empty() {
        None
    } else {
        Some(text)
    }
}

/// Load one coherent checkpoint/log generation before starting a new shell.
pub fn load_leaf_session(
    dir: &Path,
    panel_id: Uuid,
    leaf_id: Option<Uuid>,
) -> (Vec<u8>, Vec<crate::state::scrollback_log::Frame>) {
    let checkpoint = load_leaf_scrollback_checkpoint(dir, panel_id, leaf_id);
    let generation = checkpoint
        .as_ref()
        .and_then(|(generation, _)| *generation)
        .or_else(|| {
            std::fs::read_to_string(dir.join(scrollback_leaf_gen_file_name(panel_id, leaf_id)))
                .ok()?
                .trim()
                .parse()
                .ok()
        })
        .unwrap_or(0);
    let frames = std::fs::read(dir.join(scrollback_leaf_log_file_name(panel_id, leaf_id)))
        .ok()
        .and_then(|bytes| crate::state::scrollback_log::read_frames(&bytes))
        .filter(|(log_generation, _)| *log_generation == generation)
        .map(|(_, frames)| frames)
        .unwrap_or_default();
    let body = checkpoint
        .map(|(_, text)| replay_body(&text))
        .unwrap_or_default();
    (body, frames)
}

pub fn save_scrollback(dir: &Path, panel_id: Uuid, text: &str) -> anyhow::Result<()> {
    if text.trim().is_empty() {
        // Nada que guardar: si había un archivo viejo, lo sacamos para no
        // restaurar historial ajeno al estado actual.
        let _ = std::fs::remove_file(dir.join(scrollback_file_name(panel_id)));
        return Ok(());
    }
    std::fs::create_dir_all(dir)?;
    let clamped = clamp_scrollback(text, MAX_PERSISTED_BYTES);
    crate::state::durable_write::write_atomic(
        &dir.join(scrollback_file_name(panel_id)),
        clamped.as_bytes(),
    )?;
    Ok(())
}

pub fn load_scrollback(dir: &Path, panel_id: Uuid) -> Option<String> {
    let path = dir.join(scrollback_file_name(panel_id));
    let bytes = std::fs::read(path).ok()?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    if text.trim().is_empty() {
        None
    } else {
        Some(text)
    }
}

/// Borra artefactos de paneles que esta instancia conoce y que ya no existen.
///
/// El alcance `known_panel_ids` es deliberadamente obligatorio: varias
/// instancias de TerminalCanvas pueden compartir el directorio durable. Una
/// lista local de paneles vivos no demuestra que un UUID desconocido esté
/// muerto; podarlo causaría pérdida de historial en la otra instancia.
pub fn prune_scrollback(dir: &Path, live_panel_ids: &[Uuid], known_panel_ids: &[Uuid]) -> usize {
    let live_stems: std::collections::HashSet<String> = live_panel_ids
        .iter()
        .map(|id| id.simple().to_string())
        .collect();
    let known_stems: std::collections::HashSet<String> = known_panel_ids
        .iter()
        .map(|id| id.simple().to_string())
        .collect();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut removed = 0usize;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        // Sólo tocamos nuestros propios archivos (checkpoint, log, generation).
        let ours = name.ends_with(".txt") || name.ends_with(".mtlg") || name.ends_with(".gen");
        let panel_stem = panel_stem_of(&name);
        if !ours || !known_stems.contains(panel_stem) || live_stems.contains(panel_stem) {
            continue;
        }
        if std::fs::remove_file(entry.path()).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// Elimina artefactos de hojas que ya no pertenecen al árbol activo de un
/// panel. Sin esta poda, abrir/cerrar splits acumula checkpoints y logs para
/// siempre aunque el panel continúe vivo.
pub fn prune_panel_leaf_scrollback(
    dir: &Path,
    panel_id: Uuid,
    live_leaf_ids: &[Uuid],
    known_leaf_ids: &[Uuid],
) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let prefix = format!("{}-", panel_id.simple());
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let Some(leaf_text) = name.strip_prefix(&prefix).and_then(|rest| {
            rest.strip_suffix(".txt")
                .or_else(|| rest.strip_suffix(".mtlg"))
                .or_else(|| rest.strip_suffix(".gen"))
        }) else {
            continue;
        };
        let Ok(leaf_id) = Uuid::parse_str(leaf_text) else {
            continue;
        };
        if known_leaf_ids.contains(&leaf_id)
            && !live_leaf_ids.contains(&leaf_id)
            && std::fs::remove_file(entry.path()).is_ok()
        {
            removed += 1;
        }
    }
    removed
}

/// Stem de un archivo de scrollback sin la extensión, para comparar paneles
/// vivos contra sus tipos de archivo (.txt/.mtlg/.gen).
fn stem_of(name: &str) -> &str {
    if let Some(stem) = name.strip_suffix(".txt") {
        stem
    } else if let Some(stem) = name.strip_suffix(".mtlg") {
        stem
    } else if let Some(stem) = name.strip_suffix(".gen") {
        stem
    } else {
        name
    }
}

/// Parte del stem que identifica el **panel**: los archivos de hoja son
/// `{panel}-{leaf}`, así que el panel es lo que va antes del primer guión.
/// Sin esto, el prune borraba el historial de las hojas de paneles vivos.
fn panel_stem_of(name: &str) -> &str {
    let stem = stem_of(name);
    match stem.split_once('-') {
        Some((panel, _leaf)) => panel,
        None => stem,
    }
}

/// Convierte el texto guardado en bytes listos para reinyectar en el grid del
/// terminal: los saltos de línea pasan a CRLF porque el parser ANSI necesita el
/// retorno de carro explícito para volver a la columna 0.
///
/// Además se marca el final con un separador atenuado, para que quede claro que
/// eso es historial de una sesión anterior y no salida en vivo.
/// Cuerpo de replay: los saltos de línea pasan a CRLF porque el parser ANSI
/// necesita el retorno de carro explícito para volver a la columna 0.
pub fn replay_body(text: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len() + 8);
    for line in text.lines() {
        out.extend_from_slice(line.as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    out
}

pub fn replay_bytes(text: &str) -> Vec<u8> {
    let mut out = replay_body(text);
    // SGR 90 = gris; se resetea con SGR 0 para no teñir la salida siguiente.
    out.extend_from_slice("\x1b[90m── sesión anterior ──\x1b[0m\r\n".as_bytes());
    out
}

/// Separador atenuado que marca dónde terminó la sesión anterior.
pub fn replay_marker() -> Vec<u8> {
    "\x1b[90m── sesión anterior ──\x1b[0m\r\n"
        .as_bytes()
        .to_vec()
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::{
        clamp_scrollback, load_scrollback, prune_panel_leaf_scrollback, prune_scrollback,
        replay_bytes, save_scrollback, scrollback_file_name, MAX_PERSISTED_BYTES,
    };

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("scrollback-{tag}-{}", Uuid::new_v4()))
    }

    #[test]
    fn file_name_has_no_path_separators() {
        let name = scrollback_file_name(Uuid::new_v4());
        assert!(!name.contains('/'), "got {name}");
        assert!(!name.contains('\\'), "got {name}");
        assert!(name.ends_with(".txt"));
    }

    #[test]
    fn cold_session_ignores_a_log_from_an_older_checkpoint_generation() {
        use crate::state::scrollback_log::{append_frames, encode_frame, reset_log, FrameKind};
        let dir = temp_dir("cold-generation");
        let panel = Uuid::new_v4();
        super::save_leaf_scrollback_versioned(&dir, panel, None, 2, "saved\n").unwrap();
        let log = dir.join(super::scrollback_leaf_log_file_name(panel, None));
        reset_log(&log, 1).unwrap();
        append_frames(&log, &encode_frame(1, FrameKind::Output, b"duplicate")).unwrap();
        let (checkpoint, frames) = super::load_leaf_session(&dir, panel, None);
        assert_eq!(checkpoint, b"saved\r\n");
        assert!(frames.is_empty());
        reset_log(&log, 2).unwrap();
        append_frames(&log, &encode_frame(1, FrameKind::Output, b"tail")).unwrap();
        assert_eq!(
            super::load_leaf_session(&dir, panel, None).1[0].payload,
            b"tail"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn distinct_panels_get_distinct_files() {
        assert_ne!(
            scrollback_file_name(Uuid::new_v4()),
            scrollback_file_name(Uuid::new_v4())
        );
    }

    #[test]
    fn short_text_is_not_clamped() {
        assert_eq!(clamp_scrollback("hola\nchau\n", 1024), "hola\nchau\n");
    }

    #[test]
    fn clamping_keeps_the_end_not_the_beginning() {
        let text = "vieja\nmedia\nreciente\n";
        let clamped = clamp_scrollback(text, 12);
        assert!(clamped.ends_with("reciente\n"), "got {clamped:?}");
        assert!(!clamped.contains("vieja"), "got {clamped:?}");
    }

    #[test]
    fn clamping_drops_the_partial_first_line() {
        let text = "aaaaaaaaaa\nbbbb\ncccc\n";
        let clamped = clamp_scrollback(text, 12);
        // No puede empezar en medio de una línea.
        for line in clamped.lines() {
            assert!(
                ["bbbb", "cccc"].contains(&line),
                "partial line leaked: {line:?}"
            );
        }
    }

    #[test]
    fn clamping_never_splits_an_sgr_sequence() {
        // Líneas con escapes SGR intercalados: para todo tope, el resultado
        // no puede contener un `\x1b` sin su `m` de cierre.
        let mut text = String::new();
        for index in 0..40 {
            text.push_str(&format!("\x1b[38;2;204;0;0mroja {index}\x1b[0m\n"));
        }
        for max in 1..text.len() {
            let clamped = clamp_scrollback(&text, max);
            assert!(text.contains(clamped), "clamped must be a real slice");
            let mut rest = clamped;
            while let Some(pos) = rest.find('\x1b') {
                let closes = rest[pos..].find('m');
                assert!(
                    closes.is_some(),
                    "dangling escape at max={max}: {clamped:?}"
                );
                rest = &rest[pos + closes.unwrap() + 1..];
            }
        }
    }

    #[test]
    fn clamping_never_splits_a_multibyte_character() {
        // "ñ" ocupa 2 bytes: cortar en el medio invalidaría el &str.
        let text = "ñññññññññ\nfinal\n";
        for max in 1..text.len() {
            let clamped = clamp_scrollback(text, max);
            assert!(text.contains(clamped), "clamped must be a real slice");
        }
    }

    #[test]
    fn save_and_load_round_trip() {
        let dir = temp_dir("roundtrip");
        let panel = Uuid::new_v4();
        save_scrollback(&dir, panel, "linea uno\nlinea dos\n").expect("save");
        let loaded = load_scrollback(&dir, panel);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(loaded.as_deref(), Some("linea uno\nlinea dos\n"));
    }

    #[test]
    fn saving_blank_text_removes_a_previous_file() {
        let dir = temp_dir("blank");
        let panel = Uuid::new_v4();
        save_scrollback(&dir, panel, "algo\n").expect("save");
        assert!(load_scrollback(&dir, panel).is_some());

        save_scrollback(&dir, panel, "   \n\t").expect("save blank");
        let after = load_scrollback(&dir, panel);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(after, None, "stale history must not survive");
    }

    #[test]
    fn loading_an_unknown_panel_yields_none() {
        let dir = temp_dir("unknown");
        assert_eq!(load_scrollback(&dir, Uuid::new_v4()), None);
    }

    #[test]
    fn saved_file_never_exceeds_the_cap() {
        let dir = temp_dir("cap");
        let panel = Uuid::new_v4();
        let big = "x".repeat(MAX_PERSISTED_BYTES * 2);
        save_scrollback(&dir, panel, &big).expect("save");
        let size = std::fs::metadata(dir.join(scrollback_file_name(panel)))
            .expect("metadata")
            .len() as usize;
        let _ = std::fs::remove_dir_all(&dir);
        assert!(size <= MAX_PERSISTED_BYTES, "wrote {size} bytes");
    }

    #[test]
    fn pruning_removes_dead_panels_and_keeps_live_ones() {
        let dir = temp_dir("prune");
        let live = Uuid::new_v4();
        let dead = Uuid::new_v4();
        save_scrollback(&dir, live, "vivo\n").expect("save");
        save_scrollback(&dir, dead, "muerto\n").expect("save");

        let removed = prune_scrollback(&dir, &[live], &[live, dead]);
        let live_after = load_scrollback(&dir, live);
        let dead_after = load_scrollback(&dir, dead);
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(removed, 1);
        assert_eq!(live_after.as_deref(), Some("vivo\n"));
        assert_eq!(dead_after, None);
    }

    #[test]
    fn pruning_a_live_panel_removes_only_closed_leaf_artifacts() {
        let dir = temp_dir("prune-leaves");
        let panel = Uuid::new_v4();
        let live = Uuid::new_v4();
        let closed = Uuid::new_v4();
        super::save_leaf_scrollback(&dir, panel, Some(live), "viva\n").unwrap();
        super::save_leaf_scrollback(&dir, panel, Some(closed), "cerrada\n").unwrap();
        std::fs::write(
            dir.join(super::scrollback_leaf_log_file_name(panel, Some(closed))),
            b"old log",
        )
        .unwrap();
        std::fs::write(
            dir.join(super::scrollback_leaf_gen_file_name(panel, Some(closed))),
            b"old gen",
        )
        .unwrap();

        assert_eq!(
            prune_panel_leaf_scrollback(&dir, panel, &[live], &[live, closed]),
            3
        );
        assert!(super::load_leaf_scrollback(&dir, panel, Some(live)).is_some());
        assert!(super::load_leaf_scrollback(&dir, panel, Some(closed)).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pruning_a_panel_keeps_leaves_created_by_another_instance() {
        let dir = temp_dir("foreign-leaf");
        let panel = Uuid::new_v4();
        let local_leaf = Uuid::new_v4();
        let foreign_leaf = Uuid::new_v4();
        super::save_leaf_scrollback(&dir, panel, Some(local_leaf), "local\n").unwrap();
        super::save_leaf_scrollback(&dir, panel, Some(foreign_leaf), "foreign\n").unwrap();

        assert_eq!(
            prune_panel_leaf_scrollback(&dir, panel, &[local_leaf], &[local_leaf]),
            0
        );
        assert_eq!(
            super::load_leaf_scrollback(&dir, panel, Some(foreign_leaf)).as_deref(),
            Some("foreign\n")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pruning_ignores_foreign_files() {
        let dir = temp_dir("foreign");
        std::fs::create_dir_all(&dir).expect("mkdir");
        let foreign = dir.join("no-nuestro.json");
        std::fs::write(&foreign, b"{}").expect("write");

        let removed = prune_scrollback(&dir, &[], &[]);
        let survived = foreign.exists();
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(removed, 0);
        assert!(survived, "we must only delete our own .txt files");
    }

    #[test]
    fn pruning_a_missing_directory_is_a_noop() {
        assert_eq!(prune_scrollback(&temp_dir("missing"), &[], &[]), 0);
    }

    #[test]
    fn replay_uses_crlf_so_the_parser_returns_to_column_zero() {
        let bytes = replay_bytes("uno\ndos\n");
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.starts_with("uno\r\ndos\r\n"), "got {text:?}");
        assert!(!text.contains("uno\ndos"), "bare LF would stair-step");
    }

    #[test]
    fn replay_marks_where_the_previous_session_ended() {
        let text = String::from_utf8_lossy(&replay_bytes("uno\n")).into_owned();
        assert!(text.contains("sesión anterior"), "got {text:?}");
        // El color se resetea, si no tiñe la salida del shell nuevo.
        assert!(text.ends_with("\x1b[0m\r\n"), "got {text:?}");
    }

    #[test]
    fn replaying_empty_text_still_only_emits_the_marker() {
        let text = String::from_utf8_lossy(&replay_bytes("")).into_owned();
        assert!(text.contains("sesión anterior"));
        assert!(!text.contains("\r\n\r\n"), "no blank padding: {text:?}");
    }

    #[test]
    fn the_legacy_root_alias_keeps_the_historic_file_name() {
        // El alias de lectura conserva los archivos previos sin moverlos.
        let panel = Uuid::new_v4();
        assert_eq!(
            super::scrollback_leaf_file_name(panel, None),
            scrollback_file_name(panel)
        );
    }

    #[test]
    fn each_leaf_gets_its_own_file() {
        let panel = Uuid::new_v4();
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let name_a = super::scrollback_leaf_file_name(panel, Some(a));
        let name_b = super::scrollback_leaf_file_name(panel, Some(b));
        assert_ne!(name_a, name_b);
        assert!(name_a.starts_with(&panel.simple().to_string()));
        assert!(name_a.ends_with(".txt"));
        assert!(!name_a.contains('/'), "got {name_a}");
    }

    #[test]
    fn leaf_scrollbacks_round_trip_independently() {
        let dir = temp_dir("leaves");
        let panel = Uuid::new_v4();
        let leaf = Uuid::new_v4();
        super::save_leaf_scrollback(&dir, panel, None, "raiz\n").expect("save raíz");
        super::save_leaf_scrollback(&dir, panel, Some(leaf), "hoja\n").expect("save hoja");

        assert_eq!(
            super::load_leaf_scrollback(&dir, panel, None).as_deref(),
            Some("raiz\n")
        );
        assert_eq!(
            super::load_leaf_scrollback(&dir, panel, Some(leaf)).as_deref(),
            Some("hoja\n")
        );
        // La hoja raíz se lee igual por el camino histórico.
        assert_eq!(load_scrollback(&dir, panel).as_deref(), Some("raiz\n"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pruning_keeps_the_leaf_files_of_live_panels() {
        // La regresión concreta: el prune comparaba el stem completo, así que
        // `{panel}-{leaf}.txt` no matcheaba con el panel vivo y se borraba.
        let dir = temp_dir("prune-leaves");
        let live = Uuid::new_v4();
        let dead = Uuid::new_v4();
        let leaf = Uuid::new_v4();
        super::save_leaf_scrollback(&dir, live, None, "raiz viva\n").unwrap();
        super::save_leaf_scrollback(&dir, live, Some(leaf), "hoja viva\n").unwrap();
        super::save_leaf_scrollback(&dir, dead, Some(leaf), "hoja muerta\n").unwrap();

        let removed = prune_scrollback(&dir, &[live], &[live, dead]);
        let leaf_alive = super::load_leaf_scrollback(&dir, live, Some(leaf));
        let root_alive = super::load_leaf_scrollback(&dir, live, None);
        let leaf_dead = super::load_leaf_scrollback(&dir, dead, Some(leaf));
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(removed, 1, "solo el archivo del panel muerto");
        assert_eq!(leaf_alive.as_deref(), Some("hoja viva\n"));
        assert_eq!(root_alive.as_deref(), Some("raiz viva\n"));
        assert_eq!(leaf_dead, None);
    }

    #[test]
    fn saving_blank_leaf_text_removes_its_file() {
        let dir = temp_dir("blank-leaf");
        let panel = Uuid::new_v4();
        let leaf = Uuid::new_v4();
        super::save_leaf_scrollback(&dir, panel, Some(leaf), "algo\n").unwrap();
        super::save_leaf_scrollback(&dir, panel, Some(leaf), "  \n").unwrap();
        let after = super::load_leaf_scrollback(&dir, panel, Some(leaf));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(after, None, "historial viejo no puede sobrevivir");
    }
}

//! Descubrimiento de conversaciones anteriores de los CLI de agentes.
//!
//! Los CLI guardan su propio historial en disco; nosotros no lo duplicamos, lo
//! leemos. Claude Code escribe un JSONL por sesión en
//! `~/.claude/projects/<cwd-slug>/<session-id>.jsonl`, con registros tipados:
//! `ai-title` trae el título que el propio Claude le puso a la conversación y
//! los registros `user` traen `cwd`, `timestamp` y el mensaje.
//!
//! Con eso se puede listar "las conversaciones de este proyecto" y reanudar una
//! concreta con `--resume <id>`, que es exactamente lo que hace `/resume` por
//! dentro.

use std::path::{Path, PathBuf};

/// Tope de bytes a leer por archivo de sesión. Una conversación larga puede
/// pasar los 6 MB y sólo necesitamos el título y la primera consigna.
const MAX_SCAN_BYTES: u64 = 4 * 1024 * 1024;
/// Tope de sesiones a listar por proyecto.
const MAX_SESSIONS: usize = 100;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentSessionEntry {
    /// Id de sesión, que es también el nombre del archivo sin extensión. Es lo
    /// que se le pasa a `--resume`.
    pub id: String,
    /// Título legible: el que generó el agente, o la primera consigna.
    pub title: String,
    /// Última modificación del archivo, para ordenar de más reciente a más
    /// vieja.
    pub modified: Option<std::time::SystemTime>,
}

/// Variantes del nombre de carpeta que Claude puede haber usado para un `cwd`.
///
/// La regla es reemplazar los separadores por `-`, pero según la versión el
/// punto de un directorio oculto se conserva (`-Users-mauro-.cursor-...`) o
/// también se reemplaza (`/Users/mauro/.pencil/...` →
/// `-Users-mauro--pencil-...`). Probamos ambas en vez de adivinar.
pub fn slug_variants(cwd: &Path) -> Vec<String> {
    let raw = cwd.to_string_lossy();
    let keep_dots = raw.replace(['/', '\\'], "-");
    let replace_dots = keep_dots.replace('.', "-");
    if keep_dots == replace_dots {
        vec![keep_dots]
    } else {
        vec![keep_dots, replace_dots]
    }
}

fn claude_projects_root() -> Option<PathBuf> {
    crate::utils::platform::home_dir().map(|home| home.join(".claude").join("projects"))
}

/// Conversaciones anteriores de Claude Code para ese directorio de trabajo,
/// de la más reciente a la más vieja.
pub fn list_claude_sessions(cwd: &Path) -> Vec<AgentSessionEntry> {
    let Some(root) = claude_projects_root() else {
        return Vec::new();
    };
    for slug in slug_variants(cwd) {
        let dir = root.join(&slug);
        if dir.is_dir() {
            let sessions = list_sessions_in(&dir);
            if !sessions.is_empty() {
                return sessions;
            }
        }
    }
    Vec::new()
}

fn list_sessions_in(dir: &Path) -> Vec<AgentSessionEntry> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
            continue;
        }
        let Some(id) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        let modified = entry.metadata().and_then(|meta| meta.modified()).ok();
        let title = read_session_title(&path).unwrap_or_else(|| short_id(id));
        out.push(AgentSessionEntry {
            id: id.to_owned(),
            title,
            modified,
        });
    }
    // Más recientes primero: es el orden en que uno busca "la de recién".
    out.sort_by(|a, b| b.modified.cmp(&a.modified).then_with(|| a.id.cmp(&b.id)));
    out.truncate(MAX_SESSIONS);
    out
}

fn short_id(id: &str) -> String {
    format!("sesión {}", id.chars().take(8).collect::<String>())
}

fn read_session_title(path: &Path) -> Option<String> {
    use std::io::{BufRead, BufReader, Read};

    let file = std::fs::File::open(path).ok()?;
    let reader = BufReader::new(file.take(MAX_SCAN_BYTES));
    let lines = reader.lines().map_while(Result::ok);
    session_title_from_lines(lines)
}

/// Título de una conversación a partir de sus registros JSONL.
///
/// Se prefiere el `ai-title` que genera el propio agente; si no hay, cae a la
/// primera consigna del usuario. Sólo se parsea la línea si antes pasó un
/// chequeo de subcadena, porque parsear JSON de un archivo de megabytes línea
/// por línea sería mucho más caro que buscar un literal.
pub fn session_title_from_lines<I>(lines: I) -> Option<String>
where
    I: IntoIterator<Item = String>,
{
    let mut first_prompt: Option<String> = None;
    for line in lines {
        if line.contains("\"ai-title\"") {
            if let Some(title) = json_string_field(&line, "aiTitle") {
                let title = clean_title(&title);
                if !title.is_empty() {
                    return Some(title);
                }
            }
        }
        if first_prompt.is_none() && line.contains("\"type\":\"user\"") {
            if let Some(text) = json_string_field(&line, "content") {
                let text = clean_title(&text);
                if !text.is_empty() {
                    first_prompt = Some(text);
                }
            }
        }
    }
    first_prompt
}

/// Extrae `"<field>":"<valor>"` de una línea JSON sin construir el documento
/// entero. Devuelve `None` si el campo no está o el valor no es una cadena.
fn json_string_field(line: &str, field: &str) -> Option<String> {
    let needle = format!("\"{field}\":");
    let start = line.find(&needle)? + needle.len();
    let rest = line[start..].trim_start();
    let mut chars = rest.char_indices();
    if chars.next()?.1 != '"' {
        return None;
    }
    let mut value = String::new();
    let mut escaped = false;
    for (_, ch) in chars {
        if escaped {
            // Sólo los escapes que aparecen en texto plano; el resto se copia.
            value.push(match ch {
                'n' => '\n',
                't' => '\t',
                other => other,
            });
            escaped = false;
            continue;
        }
        match ch {
            '\\' => escaped = true,
            '"' => return Some(value),
            other => value.push(other),
        }
    }
    None
}

/// Deja el título en una línea y acotado, para que entre en una lista.
fn clean_title(raw: &str) -> String {
    let collapsed = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = collapsed.trim();
    if trimmed.chars().count() <= 72 {
        return trimmed.to_owned();
    }
    let cut: String = trimmed.chars().take(71).collect();
    format!("{}…", cut.trim_end())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{clean_title, json_string_field, session_title_from_lines, slug_variants};

    #[test]
    fn slug_replaces_separators_with_dashes() {
        assert_eq!(
            slug_variants(Path::new("/Users/mauro/Desktop/proyectos/terminalcanvas")),
            vec!["-Users-mauro-Desktop-proyectos-terminalcanvas".to_owned()]
        );
    }

    #[test]
    fn a_hidden_directory_yields_both_slug_variants() {
        // Según la versión, Claude conserva el punto o también lo reemplaza:
        // se prueban las dos antes que adivinar una.
        let variants = slug_variants(Path::new("/Users/mauro/.pencil/documents"));
        assert!(variants.contains(&"-Users-mauro-.pencil-documents".to_owned()));
        assert!(variants.contains(&"-Users-mauro--pencil-documents".to_owned()));
    }

    #[test]
    fn a_path_without_dots_yields_a_single_variant() {
        assert_eq!(slug_variants(Path::new("/tmp/plain")).len(), 1);
    }

    #[test]
    fn prefers_the_title_the_agent_generated() {
        let lines = vec![
            r#"{"type":"user","message":{"content":"arreglame el bug"}}"#.to_owned(),
            r#"{"type":"ai-title","aiTitle":"Explorar contenido del proyecto","sessionId":"x"}"#
                .to_owned(),
        ];
        assert_eq!(
            session_title_from_lines(lines).as_deref(),
            Some("Explorar contenido del proyecto")
        );
    }

    #[test]
    fn falls_back_to_the_first_prompt_when_there_is_no_title() {
        let lines = vec![
            r#"{"type":"system","content":"ruido"}"#.to_owned(),
            r#"{"type":"user","message":{"content":"arreglame el bug"}}"#.to_owned(),
        ];
        assert_eq!(
            session_title_from_lines(lines).as_deref(),
            Some("arreglame el bug")
        );
    }

    #[test]
    fn a_session_without_usable_records_has_no_title() {
        let lines = vec![r#"{"type":"mode","mode":"default"}"#.to_owned()];
        assert_eq!(session_title_from_lines(lines), None);
    }

    #[test]
    fn malformed_lines_do_not_abort_the_scan() {
        let lines = vec![
            "esto no es json".to_owned(),
            String::new(),
            r#"{"type":"ai-title","aiTitle":"Sobrevivió"}"#.to_owned(),
        ];
        assert_eq!(
            session_title_from_lines(lines).as_deref(),
            Some("Sobrevivió")
        );
    }

    #[test]
    fn field_extraction_handles_escaped_quotes() {
        let line = r#"{"aiTitle":"dijo \"hola\" y se fue"}"#;
        assert_eq!(
            json_string_field(line, "aiTitle").as_deref(),
            Some("dijo \"hola\" y se fue")
        );
    }

    #[test]
    fn field_extraction_returns_none_for_a_missing_field() {
        assert_eq!(json_string_field(r#"{"otro":"x"}"#, "aiTitle"), None);
    }

    #[test]
    fn field_extraction_returns_none_when_the_value_is_not_a_string() {
        assert_eq!(json_string_field(r#"{"aiTitle":42}"#, "aiTitle"), None);
    }

    #[test]
    fn titles_are_collapsed_to_one_line_and_capped() {
        let messy = "  varias\n   lineas\ty espacios  ";
        assert_eq!(clean_title(messy), "varias lineas y espacios");

        let long = "a".repeat(200);
        let capped = clean_title(&long);
        assert!(
            capped.chars().count() <= 72,
            "got {}",
            capped.chars().count()
        );
        assert!(capped.ends_with('…'));
    }

    #[test]
    fn a_multibyte_title_is_not_cut_mid_character() {
        // Cortar por bytes partiría la "ñ" y rompería el String.
        let long = "ñ".repeat(200);
        let capped = clean_title(&long);
        assert!(capped.chars().count() <= 72);
    }
    /// Diagnóstico contra el disco real de quien corre los tests. Se ignora por
    /// defecto porque depende del entorno; se corre con
    /// `cargo test -- --ignored real_claude_sessions`.
    #[test]
    #[ignore]
    fn real_claude_sessions_are_listed_with_titles() {
        let cwd = std::env::var("SESSIONS_CWD").unwrap_or_else(|_| "/Users/mauro".to_owned());
        let sessions = super::list_claude_sessions(Path::new(&cwd));
        println!("{} sesiones en {cwd}", sessions.len());
        for entry in sessions.iter().take(8) {
            println!("  {} :: {}", &entry.id[..8], entry.title);
        }
        assert!(!sessions.is_empty(), "no se encontró ninguna sesión");
    }
}

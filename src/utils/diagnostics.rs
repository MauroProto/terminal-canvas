//! Diagnóstico exportable (Ship-it 7.5).
//!
//! Arma un zip con lo mínimo para debuggear un problema reportado — panic.log,
//! runs.log, config.toml, versión y layout.json — **sin filtrar secretos ni
//! nombres de proyectos**: las claves que parecen token se redactan y los
//! títulos de panel se reemplazan por un hash corto y estable.

use std::io::Write;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// Claves de config cuyo valor nunca sale del equipo del usuario.
const SECRET_KEY_HINTS: &[&str] = &[
    "token",
    "secret",
    "password",
    "key",
    "apikey",
    "credential",
    "credentials",
];

/// ¿La línea de un TOML asigna una clave sensible?
///
/// El match es por **segmento** del nombre (separado por `_`, `-` o `.`), no
/// por substring: así `linear_token` y `api_key` se redactan pero `tokenizer`
/// o `keybindings` no. Ante la duda se prefiere redactar de más: un falso
/// positivo cuesta un poco de debuggabilidad, uno negativo filtra un secreto.
fn is_secret_line(line: &str) -> bool {
    let Some((key, _)) = line.split_once('=') else {
        return false;
    };
    let key = key.trim().trim_matches('"').to_ascii_lowercase();
    key.split(['_', '-', '.'])
        .any(|segment| SECRET_KEY_HINTS.contains(&segment))
}

/// Redacta los valores sensibles de un TOML, conservando la estructura para
/// que el archivo siga siendo legible y parseable.
pub fn redact_config(toml: &str) -> String {
    toml.lines()
        .map(|line| {
            if is_secret_line(line) {
                let key = line.split_once('=').map(|(key, _)| key).unwrap_or(line);
                format!("{key}= \"<redacted>\"")
            } else {
                line.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Hash corto y estable de un texto, para reemplazar nombres sin perder la
/// capacidad de correlacionar (el mismo título da siempre el mismo hash).
pub fn stable_hash(text: &str) -> String {
    let digest = Sha256::digest(text.as_bytes());
    digest[..4]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Reemplaza títulos, custom_titles, cwd y comandos de agente del layout por
/// hashes: la estructura (cuántos paneles, dónde, qué placement) es lo que
/// importa para debuggear, los nombres del proyecto del usuario no.
pub fn anonymize_layout(json: &str) -> String {
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(json) else {
        return String::new();
    };
    anonymize_value(&mut value);
    serde_json::to_string_pretty(&value).unwrap_or_default()
}

fn anonymize_value(value: &mut serde_json::Value) {
    const SENSITIVE: &[&str] = &[
        "title",
        "custom_title",
        "cwd",
        "agent_command",
        "name",
        "label",
        "brief",
        "task_title",
        "branch",
        "path",
        "repo_root",
        "worktree_path",
    ];
    match value {
        serde_json::Value::Object(map) => {
            for (key, entry) in map.iter_mut() {
                if SENSITIVE.contains(&key.as_str()) {
                    if let Some(text) = entry.as_str() {
                        if !text.is_empty() {
                            *entry = serde_json::Value::String(format!("#{}", stable_hash(text)));
                        }
                    }
                    continue;
                }
                anonymize_value(entry);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items.iter_mut() {
                anonymize_value(item);
            }
        }
        _ => {}
    }
}

/// Nombre del archivo de diagnóstico, ordenable por fecha.
pub fn diagnostics_file_name(now: chrono::DateTime<chrono::Local>) -> String {
    format!(
        "terminalcanvas-diagnostics-{}.zip",
        now.format("%Y%m%d-%H%M%S")
    )
}

/// Entrada que va al zip: nombre y contenido ya saneado.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticEntry {
    pub name: String,
    pub contents: String,
}

/// Junta las entradas del diagnóstico desde el data dir. Puro respecto del
/// zip: se puede testear sin escribir el archivo.
pub fn collect_entries(data_dir: &Path, version: &str) -> Vec<DiagnosticEntry> {
    let mut entries = vec![DiagnosticEntry {
        name: "version.txt".to_owned(),
        contents: format!(
            "terminalcanvas {version}\nos: {}\narch: {}\n",
            std::env::consts::OS,
            std::env::consts::ARCH
        ),
    }];

    let read = |name: &str| std::fs::read_to_string(data_dir.join(name)).ok();

    if let Some(panic_log) = read("panic.log") {
        entries.push(DiagnosticEntry {
            name: "panic.log".to_owned(),
            contents: panic_log,
        });
    }
    if let Some(runs) = read("runs.log") {
        entries.push(DiagnosticEntry {
            name: "runs.log".to_owned(),
            contents: runs,
        });
    }
    if let Some(config) = read("config.toml") {
        entries.push(DiagnosticEntry {
            name: "config.toml".to_owned(),
            contents: redact_config(&config),
        });
    }
    if let Some(layout) = read("layout.json") {
        entries.push(DiagnosticEntry {
            name: "layout.json".to_owned(),
            contents: anonymize_layout(&layout),
        });
    }
    entries
}

/// Escribe el zip con las entradas dadas.
pub fn write_zip(path: &Path, entries: &[DiagnosticEntry]) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = std::fs::File::create(path)?;
    let mut zip = zip::ZipWriter::new(file);
    let options: zip::write::FileOptions<'_, ()> =
        zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for entry in entries {
        zip.start_file(entry.name.as_str(), options)?;
        zip.write_all(entry.contents.as_bytes())?;
    }
    zip.finish()?;
    Ok(())
}

/// Exporta el diagnóstico a Descargas y devuelve el path escrito.
pub fn export(version: &str) -> anyhow::Result<PathBuf> {
    let dirs = directories::ProjectDirs::from("", "", "terminal-app")
        .ok_or_else(|| anyhow::anyhow!("no se pudo resolver el data dir"))?;
    let entries = collect_entries(dirs.data_dir(), version);
    let downloads = directories::UserDirs::new()
        .and_then(|dirs| dirs.download_dir().map(Path::to_path_buf))
        .unwrap_or_else(std::env::temp_dir);
    let path = downloads.join(diagnostics_file_name(chrono::Local::now()));
    write_zip(&path, &entries)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::{
        anonymize_layout, collect_entries, diagnostics_file_name, redact_config, stable_hash,
        write_zip,
    };

    #[test]
    fn secrets_are_redacted_but_the_structure_survives() {
        let config =
            "[terminal]\nfont_size = 15.0\n\n[integrations]\nlinear_token = \"lin_api_secreto\"\n";
        let redacted = redact_config(config);
        assert!(!redacted.contains("lin_api_secreto"), "got {redacted}");
        assert!(redacted.contains("<redacted>"));
        // Lo que no es secreto queda igual.
        assert!(redacted.contains("font_size = 15.0"));
        assert!(redacted.contains("[integrations]"));
        // Y sigue parseando como TOML.
        assert!(
            toml::from_str::<toml::Value>(&redacted).is_ok(),
            "got {redacted}"
        );
    }

    #[test]
    fn every_secret_flavour_is_caught() {
        for key in [
            "token",
            "api_key",
            "apiKey",
            "password",
            "client_secret",
            "linear_token",
            "auth-token",
        ] {
            let line = format!("{key} = \"valor\"");
            assert!(
                redact_config(&line).contains("<redacted>"),
                "no redactó {key}"
            );
        }
        // Nombres que solo se parecen no se tocan: el match es por segmento.
        assert!(!redact_config("tokenizer = 5").contains("<redacted>"));
        assert!(!redact_config("keybindings = 3").contains("<redacted>"));
        assert!(!redact_config("monkey = 1").contains("<redacted>"));
    }

    #[test]
    fn layout_titles_and_paths_become_stable_hashes() {
        let layout = r#"{"workspaces":[{"name":"Proyecto Secreto","cwd":"/Users/mauro/repo",
            "panels":[{"title":"claude — feature X","z_index":3,"minimized":false}]}]}"#;
        let anonymized = anonymize_layout(layout);
        assert!(!anonymized.contains("Proyecto Secreto"), "got {anonymized}");
        assert!(!anonymized.contains("/Users/mauro"), "got {anonymized}");
        assert!(!anonymized.contains("feature X"), "got {anonymized}");
        // La estructura sí se conserva: es lo que sirve para debuggear.
        assert!(anonymized.contains("\"z_index\": 3"), "got {anonymized}");
        assert!(anonymized.contains("\"minimized\": false"));
    }

    #[test]
    fn the_same_title_always_hashes_the_same() {
        assert_eq!(stable_hash("claude"), stable_hash("claude"));
        assert_ne!(stable_hash("claude"), stable_hash("codex"));
        assert_eq!(stable_hash("claude").len(), 8);
    }

    #[test]
    fn a_broken_layout_yields_empty_instead_of_leaking_raw_json() {
        assert_eq!(anonymize_layout("{no soy json"), "");
    }

    #[test]
    fn the_file_name_is_sortable() {
        use chrono::TimeZone;
        let when = chrono::Local
            .with_ymd_and_hms(2026, 8, 6, 15, 4, 9)
            .single()
            .expect("hora válida");
        assert_eq!(
            diagnostics_file_name(when),
            "terminalcanvas-diagnostics-20260806-150409.zip"
        );
    }

    #[test]
    fn collect_reads_what_exists_and_skips_what_does_not() {
        let dir = std::env::temp_dir().join(format!("diag-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("runs.log"), "run 1\n").unwrap();
        std::fs::write(dir.join("config.toml"), "linear_token = \"x\"\n").unwrap();

        let entries = collect_entries(&dir, "1.2.3");
        let names: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
        assert!(names.contains(&"version.txt"));
        assert!(names.contains(&"runs.log"));
        assert!(names.contains(&"config.toml"));
        assert!(!names.contains(&"panic.log"), "no existía: no se inventa");
        // El token no viaja ni siquiera acá.
        let config = entries.iter().find(|e| e.name == "config.toml").unwrap();
        assert!(config.contents.contains("<redacted>"));
        assert!(entries[0].contents.contains("1.2.3"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_zip_is_written_and_contains_every_entry() {
        let dir = std::env::temp_dir().join(format!("diag-zip-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("diag.zip");
        let entries = vec![
            super::DiagnosticEntry {
                name: "version.txt".to_owned(),
                contents: "v1\n".to_owned(),
            },
            super::DiagnosticEntry {
                name: "runs.log".to_owned(),
                contents: "run\n".to_owned(),
            },
        ];
        write_zip(&path, &entries).expect("escribe el zip");

        let file = std::fs::File::open(&path).unwrap();
        let mut archive = zip::ZipArchive::new(file).expect("zip válido");
        assert_eq!(archive.len(), 2);
        let names: Vec<String> = (0..archive.len())
            .map(|index| archive.by_index(index).unwrap().name().to_owned())
            .collect();
        assert!(names.contains(&"version.txt".to_owned()));
        assert!(names.contains(&"runs.log".to_owned()));

        let _ = std::fs::remove_dir_all(&dir);
    }
}

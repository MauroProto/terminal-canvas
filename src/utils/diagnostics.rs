//! Diagnóstico exportable (Ship-it 7.5).
//!
//! Arma un zip con lo mínimo para debuggear un problema reportado — panic.log,
//! runs.log, config.toml, versión y layout.json. Config y layout se anonimizan;
//! los logs pueden contener texto de errores y deben revisarse antes de compartir.

use std::io::{Read, Seek, SeekFrom, Write};
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

/// ¿Una clave TOML contiene un segmento sensible?
///
/// El match es por **segmento** del nombre (separado por `_`, `-` o `.`), no
/// por substring: así `linear_token` y `api_key` se redactan pero `tokenizer`
/// o `keybindings` no. Ante la duda se prefiere redactar de más: un falso
/// positivo cuesta un poco de debuggabilidad, uno negativo filtra un secreto.
fn is_secret_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    key.split(['_', '-', '.'])
        .any(|segment| SECRET_KEY_HINTS.contains(&segment))
}

/// Redacta los valores sensibles de un TOML, conservando la estructura para
/// que el archivo siga siendo legible y parseable.
pub fn redact_config(toml: &str) -> String {
    redact_config_checked(toml)
        .unwrap_or_else(|| "# Configuration omitted: invalid TOML.\n".to_owned())
}

fn redact_config_checked(toml: &str) -> Option<String> {
    fn redact(value: &mut toml::Value) {
        match value {
            toml::Value::Table(table) => {
                for (key, value) in table {
                    if is_secret_key(key) {
                        *value = toml::Value::String("<redacted>".to_owned());
                    } else {
                        redact(value);
                    }
                }
            }
            toml::Value::Array(values) => values.iter_mut().for_each(redact),
            _ => {}
        }
    }
    // A malformed file cannot be safely redacted line by line: quoted keys,
    // inline tables and multiline strings can carry secrets across lines.
    let mut value = toml::from_str::<toml::Value>(toml).ok()?;
    redact(&mut value);
    toml::to_string_pretty(&value).ok()
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

fn anonymize_layout_checked(json: &str) -> Option<String> {
    let mut value = serde_json::from_str::<serde_json::Value>(json).ok()?;
    if !value.is_object() {
        return None;
    }
    anonymize_value(&mut value);
    serde_json::to_string_pretty(&value).ok()
}

fn anonymize_value(value: &mut serde_json::Value) {
    const SENSITIVE: &[&str] = &[
        "title",
        "custom_title",
        "cwd",
        "agent_command",
        "leaf_agent_commands",
        "agent_session_id",
        "leaf_agent_session_ids",
        "startup_command",
        "startup_input",
        "summary",
        "excerpt",
        "command_excerpt",
        "last_error",
        "last_success",
        "changed_files",
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
                    anonymize_strings(entry);
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

fn anonymize_strings(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::String(text) if !text.is_empty() => {
            *text = format!("#{}", stable_hash(text));
        }
        serde_json::Value::Object(map) => map.values_mut().for_each(anonymize_strings),
        serde_json::Value::Array(items) => items.iter_mut().for_each(anonymize_strings),
        _ => {}
    }
}

/// Nombre ordenable por fecha y único incluso para exports simultáneos.
pub fn diagnostics_file_name(now: chrono::DateTime<chrono::Local>) -> String {
    format!(
        "terminalcanvas-diagnostics-{}-{}.zip",
        now.format("%Y%m%d-%H%M%S"),
        uuid::Uuid::new_v4()
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
pub fn collect_entries(
    data_dir: &Path,
    config_path: &Path,
    panic_path: &Path,
    version: &str,
) -> Vec<DiagnosticEntry> {
    collect_entries_with_limit(
        data_dir,
        config_path,
        panic_path,
        version,
        MAX_DIAGNOSTIC_BYTES,
    )
}

fn collect_entries_with_limit(
    data_dir: &Path,
    config_path: &Path,
    panic_path: &Path,
    version: &str,
    limit: u64,
) -> Vec<DiagnosticEntry> {
    let mut entries = vec![DiagnosticEntry {
        name: "version.txt".to_owned(),
        contents: format!(
            "terminalcanvas {version}\nos: {}\narch: {}\n",
            std::env::consts::OS,
            std::env::consts::ARCH
        ),
    }];
    let mut sources = Vec::new();
    for (name, path, kind) in [
        ("panic.log", panic_path.to_path_buf(), SourceKind::Log),
        ("runs.log", data_dir.join("runs.log"), SourceKind::Log),
        ("config.toml", config_path.to_path_buf(), SourceKind::Config),
        (
            "layout.json",
            data_dir.join("layout.json"),
            SourceKind::Layout,
        ),
    ] {
        let outcome = read_diagnostic(&path, matches!(kind, SourceKind::Log), limit);
        match outcome {
            DiagnosticRead::Included { bytes, truncated } => {
                let prepared = match kind {
                    SourceKind::Log => {
                        let (text, lossy_utf8) = log_text(&bytes, truncated);
                        Some((text, lossy_utf8))
                    }
                    SourceKind::Config | SourceKind::Layout => {
                        std::str::from_utf8(&bytes).ok().and_then(|text| {
                            let sanitized = match kind {
                                SourceKind::Config => redact_config_checked(text),
                                SourceKind::Layout => anonymize_layout_checked(text),
                                SourceKind::Log => unreachable!(),
                            };
                            sanitized.map(|text| (text, false))
                        })
                    }
                };
                if let Some((contents, lossy_utf8)) = prepared {
                    sources.push(serde_json::json!({
                        "file": name, "status": "included", "truncated": truncated,
                        "lossy_utf8": lossy_utf8,
                    }));
                    entries.push(DiagnosticEntry {
                        name: name.to_owned(),
                        contents,
                    });
                } else {
                    sources.push(omission_report(name, DiagnosticIssue::InvalidFormat));
                }
            }
            DiagnosticRead::Omitted(issue) => sources.push(omission_report(name, issue)),
        }
    }
    // Missing optional files are normal, but remain visible to the recipient.
    // Only logical filenames and safe codes appear here, never paths/error text.
    let report = serde_json::json!({
        "schema_version": 1,
        "source_byte_limit": limit,
        "optional_missing_is_an_error": false,
        "sources": sources,
        "note": "Omitted sources are absent from the ZIP. Included logs can contain private error text; review them before sharing.",
    });
    entries.push(DiagnosticEntry {
        name: "collection-report.json".to_owned(),
        contents: format!("{report:#}\n"),
    });
    entries
}

#[derive(Clone, Copy)]
enum SourceKind {
    Log,
    Config,
    Layout,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DiagnosticIssue {
    Missing,
    ReadError {
        operation: &'static str,
        kind: std::io::ErrorKind,
    },
    Oversize,
    InvalidFormat,
}

#[derive(Debug, PartialEq, Eq)]
enum DiagnosticRead {
    Included { bytes: Vec<u8>, truncated: bool },
    Omitted(DiagnosticIssue),
}

fn omission_report(name: &str, issue: DiagnosticIssue) -> serde_json::Value {
    let (reason, operation, io_kind) = match issue {
        DiagnosticIssue::Missing => ("missing", None, None),
        DiagnosticIssue::ReadError { operation, kind } => {
            ("read-error", Some(operation), Some(format!("{kind:?}")))
        }
        DiagnosticIssue::Oversize => ("oversize", None, None),
        DiagnosticIssue::InvalidFormat => ("invalid-format", None, None),
    };
    serde_json::json!({
        "file": name, "status": "omitted", "reason": reason,
        "operation": operation, "io_kind": io_kind,
    })
}

const MAX_DIAGNOSTIC_BYTES: u64 = 4 * 1024 * 1024;

fn open_regular_source(path: &Path) -> Result<Option<std::fs::File>, DiagnosticIssue> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // A FIFO with no writer must never block the single export worker.
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return match std::fs::symlink_metadata(path) {
                Err(status) if status.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(status) => Err(DiagnosticIssue::ReadError {
                    operation: "path-status",
                    kind: status.kind(),
                }),
                Ok(_) => Err(DiagnosticIssue::ReadError {
                    operation: "open",
                    kind: error.kind(),
                }),
            };
        }
        Err(error) => {
            return Err(DiagnosticIssue::ReadError {
                operation: "open",
                kind: error.kind(),
            })
        }
    };
    let metadata = file
        .metadata()
        .map_err(|error| DiagnosticIssue::ReadError {
            operation: "metadata",
            kind: error.kind(),
        })?;
    if !metadata.is_file() {
        return Err(DiagnosticIssue::ReadError {
            operation: "file-type",
            kind: std::io::ErrorKind::InvalidInput,
        });
    }
    Ok(Some(file))
}

fn read_diagnostic(path: &Path, tail: bool, limit: u64) -> DiagnosticRead {
    let mut file = match open_regular_source(path) {
        Ok(Some(file)) => file,
        Ok(None) => return DiagnosticRead::Omitted(DiagnosticIssue::Missing),
        Err(issue) => return DiagnosticRead::Omitted(issue),
    };
    let metadata = match file.metadata() {
        Ok(metadata) => metadata,
        Err(error) => {
            return DiagnosticRead::Omitted(DiagnosticIssue::ReadError {
                operation: "metadata",
                kind: error.kind(),
            })
        }
    };
    read_from_regular(&mut file, metadata.len(), tail, limit)
}

fn read_from_regular<R: Read + Seek>(
    reader: &mut R,
    advertised_size: u64,
    tail: bool,
    limit: u64,
) -> DiagnosticRead {
    let Some(sentinel) = limit.checked_add(1) else {
        return DiagnosticRead::Omitted(DiagnosticIssue::ReadError {
            operation: "limit",
            kind: std::io::ErrorKind::InvalidInput,
        });
    };
    let truncated = advertised_size > limit;
    if truncated && !tail {
        return DiagnosticRead::Omitted(DiagnosticIssue::Oversize);
    }
    if let Some(Err(error)) =
        truncated.then(|| reader.seek(SeekFrom::Start(advertised_size - limit)))
    {
        return DiagnosticRead::Omitted(DiagnosticIssue::ReadError {
            operation: "seek",
            kind: error.kind(),
        });
    }
    // A known large log exports only its captured tail window. For a source
    // thought to fit, the sentinel detects growth and rejects every prefix.
    let mut bytes = Vec::new();
    let bound = if truncated { limit } else { sentinel };
    if let Err(error) = reader.take(bound).read_to_end(&mut bytes) {
        return DiagnosticRead::Omitted(DiagnosticIssue::ReadError {
            operation: "read",
            kind: error.kind(),
        });
    }
    if bytes.len() as u64 > limit {
        return DiagnosticRead::Omitted(DiagnosticIssue::Oversize);
    }
    DiagnosticRead::Included { bytes, truncated }
}

fn log_text(bytes: &[u8], truncated: bool) -> (String, bool) {
    let bytes = if truncated {
        // Drop the first partial record before decoding, including split UTF-8.
        bytes
            .iter()
            .position(|&byte| byte == b'\n')
            .map_or(&[][..], |end| &bytes[end + 1..])
    } else {
        bytes
    };
    let text = String::from_utf8_lossy(bytes);
    let lossy_utf8 = matches!(text, std::borrow::Cow::Owned(_));
    let contents = if truncated {
        format!("[Earlier log entries omitted]\n{text}")
    } else {
        text.into_owned()
    };
    (contents, lossy_utf8)
}

// Preserve the existing log-tail fixture's small private helper contract.
#[cfg(test)]
fn read_bounded(path: &Path, tail: bool) -> std::io::Result<String> {
    match read_diagnostic(path, tail, MAX_DIAGNOSTIC_BYTES) {
        DiagnosticRead::Included { bytes, truncated } => Ok(log_text(&bytes, truncated).0),
        DiagnosticRead::Omitted(DiagnosticIssue::Missing) => {
            Err(std::io::ErrorKind::NotFound.into())
        }
        DiagnosticRead::Omitted(DiagnosticIssue::ReadError { kind, .. }) => Err(kind.into()),
        DiagnosticRead::Omitted(_) => Err(std::io::ErrorKind::InvalidData.into()),
    }
}

/// Publica un zip completo mediante escritura atómica y privada en Unix.
pub fn write_zip(path: &Path, entries: &[DiagnosticEntry]) -> anyhow::Result<()> {
    // Build and finish the archive before touching its destination. An error
    // in compression or an entry must never truncate an earlier export.
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options: zip::write::FileOptions<'_, ()> =
        zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for entry in entries {
        zip.start_file(entry.name.as_str(), options)?;
        zip.write_all(entry.contents.as_bytes())?;
    }
    let bytes = zip.finish()?.into_inner();
    crate::state::durable_write::write_atomic(path, &bytes)?;
    Ok(())
}

/// Exporta el diagnóstico a Descargas y devuelve el path escrito.
pub fn export(version: &str) -> anyhow::Result<PathBuf> {
    let dirs = super::app_paths::get()?;
    let panic_path = super::app_paths::panic_log_path()
        .ok_or_else(|| anyhow::anyhow!("no se pudo resolver el log de errores"))?;
    let entries = collect_entries(
        &dirs.data,
        &dirs.config.join("config.toml"),
        &panic_path,
        version,
    );
    let downloads = super::app_paths::exports_dir()
        .ok_or_else(|| anyhow::anyhow!("no se pudo resolver el directorio de diagnóstico"))?;
    let path = downloads.join(diagnostics_file_name(chrono::Local::now()));
    write_zip(&path, &entries)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use std::io::Read;

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
    fn nested_and_multiline_secrets_never_reach_the_archive() {
        let raw = "token = '''\nsecret line one\nsecret line two\n'''\nitems = [{ name = 'ok', password = 'hidden' }]\n[integrations.credentials]\nvalue = 'private'\n";
        let redacted = redact_config(raw);
        for secret in ["secret line", "hidden", "private"] {
            assert!(!redacted.contains(secret), "leaked {secret}: {redacted}");
        }
        let parsed: toml::Value = toml::from_str(&redacted).unwrap();
        assert_eq!(parsed["items"][0]["name"].as_str(), Some("ok"));
        assert!(!redact_config("token = '''\nsecret").contains("secret"));
    }

    #[test]
    fn split_leaf_commands_and_sensitive_containers_are_anonymized() {
        let raw = r#"{"leaf_agent_commands":{"leaf-a":"agent --prompt private-task"},"cwd":["/private/path"],"z_index":3}"#;
        let anonymized = anonymize_layout(raw);
        assert!(!anonymized.contains("private"));
        let parsed: serde_json::Value = serde_json::from_str(&anonymized).unwrap();
        assert_eq!(parsed["z_index"], 3);
        assert!(parsed["leaf_agent_commands"]["leaf-a"]
            .as_str()
            .unwrap()
            .starts_with('#'));
    }

    #[test]
    fn orchestration_summaries_and_paths_are_not_exported_as_plaintext() {
        let raw = serde_json::json!({
            "orchestration": {
                "sessions": [{
                    "startup_command": "agent --prompt secret-brief",
                    "command_summary": { "excerpt": "secret-output", "failed": false },
                    "review_summary": { "changed_files": ["/secret-project/main.rs"], "last_error": "secret-failure" }
                }],
                "inbox": [{ "summary": "secret-request", "resolved": true }]
            }
        });
        let sanitized = anonymize_layout(&raw.to_string());
        assert!(!sanitized.contains("secret-"));
        let parsed: serde_json::Value = serde_json::from_str(&sanitized).unwrap();
        assert_eq!(
            parsed["orchestration"]["sessions"][0]["command_summary"]["failed"],
            false
        );
        assert_eq!(parsed["orchestration"]["inbox"][0]["resolved"], true);
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
        let name = diagnostics_file_name(when);
        let prefix = "terminalcanvas-diagnostics-20260806-150409-";
        let suffix = name
            .strip_prefix(prefix)
            .unwrap()
            .strip_suffix(".zip")
            .unwrap();
        assert!(uuid::Uuid::parse_str(suffix).is_ok());
        assert!(name < diagnostics_file_name(when + chrono::Duration::seconds(1)));
    }

    #[test]
    fn collect_reads_what_exists_and_skips_what_does_not() {
        let dir = std::env::temp_dir().join(format!("diag-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("runs.log"), "run 1\n").unwrap();
        std::fs::write(dir.join("config.toml"), "linear_token = \"x\"\n").unwrap();

        let entries = collect_entries(
            &dir,
            &dir.join("config.toml"),
            &dir.join("panic.log"),
            "1.2.3",
        );
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
    fn collects_real_sources_when_config_and_logs_live_outside_data_dir() {
        let root = std::env::temp_dir().join(format!("diag-sources-{}", uuid::Uuid::new_v4()));
        let data = root.join("data");
        std::fs::create_dir_all(&data).unwrap();
        let config = root.join("external-config.toml");
        let panic = root.join("external-panic.log");
        std::fs::write(&config, "token = 'secret'\n").unwrap();
        std::fs::write(&panic, "actual panic\n").unwrap();
        let entries = collect_entries(&data, &config, &panic, "test");
        assert_eq!(
            entries
                .iter()
                .find(|e| e.name == "panic.log")
                .unwrap()
                .contents,
            "actual panic\n"
        );
        assert!(!entries
            .iter()
            .find(|e| e.name == "config.toml")
            .unwrap()
            .contents
            .contains("secret"));
        let large = "é\n".repeat(super::MAX_DIAGNOSTIC_BYTES as usize / 2);
        std::fs::write(&panic, large).unwrap();
        let tail = super::read_bounded(&panic, true).unwrap();
        assert!(tail.starts_with("[Earlier log entries omitted]"));
        assert!(!tail.contains('\u{fffd}'));
        assert!(tail.len() <= super::MAX_DIAGNOSTIC_BYTES as usize + 40);
        std::fs::remove_dir_all(root).unwrap();
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

        let mut contents = String::new();
        archive
            .by_name("runs.log")
            .unwrap()
            .read_to_string(&mut contents)
            .unwrap();
        assert_eq!(contents, "run\n");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        drop(archive);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn exports_created_at_the_same_instant_keep_both_archives() {
        let dir = std::env::temp_dir().join(format!("diag-same-time-{}", uuid::Uuid::new_v4()));
        let now = chrono::Local::now();
        let paths = [
            dir.join(diagnostics_file_name(now)),
            dir.join(diagnostics_file_name(now)),
        ];
        assert_ne!(paths[0], paths[1]);
        for (index, path) in paths.iter().enumerate() {
            let contents = format!("export {index}");
            write_zip(
                path,
                &[super::DiagnosticEntry {
                    name: "version.txt".to_owned(),
                    contents: contents.clone(),
                }],
            )
            .unwrap();
            let mut archive = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
            let mut restored = String::new();
            archive
                .by_name("version.txt")
                .unwrap()
                .read_to_string(&mut restored)
                .unwrap();
            assert_eq!(restored, contents);
        }
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 2);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_archive_construction_error_preserves_the_previous_export() {
        let dir = std::env::temp_dir().join(format!("diag-preserve-{}", uuid::Uuid::new_v4()));
        let path = dir.join("previous.zip");
        let entry = super::DiagnosticEntry {
            name: "version.txt".to_owned(),
            contents: "previous export".to_owned(),
        };
        write_zip(&path, std::slice::from_ref(&entry)).unwrap();
        let previous = std::fs::read(&path).unwrap();

        // ZipWriter rejects duplicate names after it already wrote one entry.
        // The previous implementation had already truncated the destination.
        assert!(write_zip(&path, &[entry.clone(), entry]).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), previous);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_publication_error_preserves_the_destination_and_cleans_temporary_files() {
        let dir = std::env::temp_dir().join(format!("diag-publish-error-{}", uuid::Uuid::new_v4()));
        let destination = dir.join("existing-directory.zip");
        std::fs::create_dir_all(&destination).unwrap();
        let previous = destination.join("existing.txt");
        std::fs::write(&previous, "retain previous contents").unwrap();
        let entries = [super::DiagnosticEntry {
            name: "version.txt".to_owned(),
            contents: "new export".to_owned(),
        }];

        // The completed temporary ZIP cannot replace a nonempty directory.
        assert!(write_zip(&destination, &entries).is_err());
        assert_eq!(
            std::fs::read_to_string(&previous).unwrap(),
            "retain previous contents"
        );
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        assert_eq!(std::fs::read_dir(&destination).unwrap().count(), 1);

        std::fs::remove_dir_all(&dir).unwrap();
    }
}

#[cfg(test)]
mod source_quality_tests {
    use super::*;
    use std::io::{self, Cursor};

    struct TempDirectory(PathBuf);

    impl TempDirectory {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("diag-quality-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TempDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn report(entries: &[DiagnosticEntry]) -> serde_json::Value {
        serde_json::from_str(
            &entries
                .iter()
                .find(|entry| entry.name == "collection-report.json")
                .unwrap()
                .contents,
        )
        .unwrap()
    }

    fn source<'a>(report: &'a serde_json::Value, name: &str) -> &'a serde_json::Value {
        report["sources"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["file"] == name)
            .unwrap()
    }

    #[test]
    fn support_report_exposes_omissions_without_private_paths_or_placeholder_sources() {
        let dir = TempDirectory::new();
        std::fs::create_dir(dir.0.join("panic.log")).unwrap();
        let config = dir.0.join("private-config-path.toml");
        std::fs::write(&config, "token='private-secret'".repeat(8)).unwrap();
        std::fs::write(dir.0.join("layout.json"), "{private-invalid-json").unwrap();
        let entries =
            collect_entries_with_limit(&dir.0, &config, &dir.0.join("panic.log"), "test", 32);
        assert_eq!(
            entries.len(),
            2,
            "only version and explicit report; never placeholder config/layout"
        );
        let source_report = report(&entries);
        assert_eq!(source(&source_report, "panic.log")["reason"], "read-error");
        assert_eq!(source(&source_report, "runs.log")["reason"], "missing");
        assert_eq!(source(&source_report, "config.toml")["reason"], "oversize");
        assert_eq!(
            source(&source_report, "layout.json")["reason"],
            "invalid-format"
        );
        assert_eq!(source_report["optional_missing_is_an_error"], false);
        let encoded = source_report.to_string();
        for private in [
            dir.0.to_str().unwrap(),
            "private-config-path",
            "private-secret",
            "private-invalid-json",
        ] {
            assert!(!encoded.contains(private), "report leaked source data");
        }
        std::fs::write(&config, "token='private-unclosed").unwrap();
        let entries =
            collect_entries_with_limit(&dir.0, &config, &dir.0.join("panic.log"), "test", 32);
        assert_eq!(
            source(&report(&entries), "config.toml")["reason"],
            "invalid-format"
        );
        std::fs::write(dir.0.join("layout.json"), r#""private scalar layout""#).unwrap();
        let entries =
            collect_entries_with_limit(&dir.0, &config, &dir.0.join("panic.log"), "test", 32);
        assert_eq!(
            source(&report(&entries), "layout.json")["reason"],
            "invalid-format"
        );
    }

    #[test]
    fn diagnostic_reads_bound_advertised_size_and_growth_before_accepting_any_prefix() {
        let mut huge = Cursor::new(b"unread".to_vec());
        assert_eq!(
            read_from_regular(&mut huge, u64::MAX, false, 16),
            DiagnosticRead::Omitted(DiagnosticIssue::Oversize)
        );
        assert_eq!(huge.position(), 0, "metadata rejection must not read");
        let valid = b"{\"v\":1}";
        let mut growing = Cursor::new([valid.as_slice(), b" extra-private"].concat());
        assert_eq!(
            read_from_regular(&mut growing, valid.len() as u64, false, valid.len() as u64),
            DiagnosticRead::Omitted(DiagnosticIssue::Oversize)
        );
        assert_eq!(
            growing.position(),
            valid.len() as u64 + 1,
            "only the sentinel byte may be consumed"
        );
        let unicode = "é".as_bytes().to_vec();
        let mut exact = Cursor::new(unicode.clone());
        assert_eq!(
            read_from_regular(&mut exact, 2, false, 2),
            DiagnosticRead::Included {
                bytes: unicode,
                truncated: false
            }
        );
    }

    struct InterruptedThenFailure {
        bytes: Cursor<Vec<u8>>,
        interrupted: bool,
        fail_at_eof: bool,
    }

    impl Read for InterruptedThenFailure {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            if !self.interrupted {
                self.interrupted = true;
                return Err(io::ErrorKind::Interrupted.into());
            }
            if self.fail_at_eof && self.bytes.position() == self.bytes.get_ref().len() as u64 {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "private-path-and-error-text",
                ));
            }
            self.bytes.read(output)
        }
    }

    impl Seek for InterruptedThenFailure {
        fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
            self.bytes.seek(position)
        }
    }

    #[test]
    fn diagnostic_reader_retries_interrupts_but_preserves_an_error_after_complete_json() {
        let bytes = b"{\"v\":1}".to_vec();
        let mut successful = InterruptedThenFailure {
            bytes: Cursor::new(bytes.clone()),
            interrupted: false,
            fail_at_eof: false,
        };
        assert_eq!(
            read_from_regular(&mut successful, bytes.len() as u64, false, 32),
            DiagnosticRead::Included {
                bytes: bytes.clone(),
                truncated: false
            }
        );
        let mut failed = InterruptedThenFailure {
            bytes: Cursor::new(bytes.clone()),
            interrupted: false,
            fail_at_eof: true,
        };
        let DiagnosticRead::Omitted(issue) =
            read_from_regular(&mut failed, bytes.len() as u64, false, 32)
        else {
            panic!("read failure after JSON must not export that prefix");
        };
        assert_eq!(
            issue,
            DiagnosticIssue::ReadError {
                operation: "read",
                kind: io::ErrorKind::PermissionDenied
            }
        );
        let report = omission_report("layout.json", issue).to_string();
        assert!(report.contains("PermissionDenied"));
        assert!(!report.contains("private-path-and-error-text"));
    }

    #[test]
    fn valid_support_zip_preserves_sanitizers_and_reports_the_bounded_utf8_log_tail() {
        let dir = TempDirectory::new();
        let config = dir.0.join("config.toml");
        let panic = dir.0.join("panic.log");
        std::fs::write(&config, "token='hidden'\n").unwrap();
        std::fs::write(&panic, "panic safe\n").unwrap();
        std::fs::write(dir.0.join("layout.json"), r#"{"title":"private"}"#).unwrap();
        std::fs::write(dir.0.join("runs.log"), "é\n".repeat(20)).unwrap();
        let entries = collect_entries_with_limit(&dir.0, &config, &panic, "test", 24);
        let source_report = report(&entries);
        assert_eq!(source(&source_report, "runs.log")["truncated"], true);
        assert_eq!(source(&source_report, "runs.log")["lossy_utf8"], false);
        for row in source_report["sources"].as_array().unwrap() {
            assert_eq!(row["status"], "included");
        }
        let DiagnosticRead::Included { bytes, truncated } =
            read_diagnostic(&dir.0.join("runs.log"), true, 8)
        else {
            panic!("regular log tail available");
        };
        let (tail, lossy) = log_text(&bytes, truncated);
        assert!(tail.starts_with("[Earlier log entries omitted]\n"));
        assert!(!lossy && !tail.contains('\u{fffd}'));
        let destination = dir.0.join("support.zip");
        write_zip(&destination, &entries).unwrap();
        let mut archive = zip::ZipArchive::new(std::fs::File::open(destination).unwrap()).unwrap();
        assert_eq!(archive.len(), entries.len());
        for entry in &entries {
            let mut restored = String::new();
            archive
                .by_name(&entry.name)
                .unwrap()
                .read_to_string(&mut restored)
                .unwrap();
            assert_eq!(restored, entry.contents);
            if entry.name == "config.toml" {
                assert!(!restored.contains("hidden") && restored.contains("<redacted>"));
            }
            if entry.name == "layout.json" {
                assert!(
                    !restored.contains("private")
                        && serde_json::from_str::<serde_json::Value>(&restored).is_ok()
                );
            }
        }
    }

    #[test]
    fn invalid_utf8_structured_sources_are_reported_instead_of_exporting_lossy_data() {
        let dir = TempDirectory::new();
        let config = dir.0.join("config.toml");
        std::fs::write(&config, b"token='\xff'").unwrap();
        std::fs::write(dir.0.join("layout.json"), b"{\"title\":\"\xff\"}").unwrap();
        let entries =
            collect_entries_with_limit(&dir.0, &config, &dir.0.join("panic.log"), "test", 64);
        assert_eq!(entries.len(), 2);
        let source_report = report(&entries);
        assert_eq!(
            source(&source_report, "config.toml")["reason"],
            "invalid-format"
        );
        assert_eq!(
            source(&source_report, "layout.json")["reason"],
            "invalid-format"
        );
        assert!(!entries
            .iter()
            .any(|entry| entry.name == "config.toml" || entry.name == "layout.json"));
    }
}

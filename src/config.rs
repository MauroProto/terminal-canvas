//! Configuración de usuario leída de `config.toml` (directorio de config de
//! la app). Es la única fuente de ajustes runtime; los valores ausentes caen
//! a defaults sensatos. Se carga al arrancar y se expone vía
//! `runtime_config()`; el diálogo de configuración puede actualizarla en vivo
//! (`update_runtime_config`) y persistirla (`save`).

use std::path::PathBuf;
use std::sync::RwLock;

use serde::{Deserialize, Serialize};

use crate::terminal::metrics::{clamp_font_size, DEFAULT_FONT_SIZE};

pub const DEFAULT_SCROLLBACK_LINES: usize = 10_000;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppConfig {
    pub font_size: f32,
    pub scrollback_lines: usize,
    pub allow_osc52: bool,
    pub audio_bell: bool,
    /// Copiar al portapapeles automáticamente al seleccionar (default off:
    /// en macOS la convención es copiar con Cmd+C explícito).
    pub copy_on_select: bool,
    /// Notificación del sistema cuando un agente pasa a necesitar atención
    /// (esperando aprobación, input, o falló).
    pub agent_notifications: bool,
    /// Shell personalizada para los terminales nuevos; `None` = login shell
    /// del sistema.
    pub shell: Option<String>,
    /// Token de API de Linear (`[integrations] linear_token`), para la fuente
    /// de issues de la pestaña Tasks (P3.17). `None` = integración apagada.
    pub linear_token: Option<String>,
    /// El overlay de onboarding ya se cerró (Ship-it 7.2). Se persiste para no
    /// mostrarlo en cada arranque.
    pub onboarding_dismissed: bool,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            font_size: DEFAULT_FONT_SIZE,
            scrollback_lines: DEFAULT_SCROLLBACK_LINES,
            // OSC 52 (clipboard desde el terminal) viene apagado por defecto:
            // lo habilita quien lo necesita, igual que el gate por env previo.
            allow_osc52: false,
            audio_bell: false,
            copy_on_select: false,
            agent_notifications: true,
            shell: None,
            linear_token: None,
            onboarding_dismissed: false,
        }
    }
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct ConfigFile {
    #[serde(default)]
    terminal: TerminalSection,
    #[serde(default)]
    integrations: IntegrationsSection,
    #[serde(default)]
    onboarding: OnboardingSection,
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct IntegrationsSection {
    #[serde(default)]
    linear_token: Option<String>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct OnboardingSection {
    #[serde(default)]
    dismissed: Option<bool>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct TerminalSection {
    #[serde(default)]
    font_size: Option<f32>,
    #[serde(default)]
    scrollback_lines: Option<usize>,
    #[serde(default)]
    allow_osc52: Option<bool>,
    #[serde(default)]
    audio_bell: Option<bool>,
    #[serde(default)]
    copy_on_select: Option<bool>,
    #[serde(default)]
    agent_notifications: Option<bool>,
    #[serde(default)]
    shell: Option<String>,
}

/// Complete configuration files are bounded independently of layout and support ZIP limits.
pub const MAX_CONFIG_FILE_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigCandidate {
    Primary,
    Backup(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigLoadOperation {
    ProfilePath,
    Privacy,
    Open,
    Metadata,
    Read,
    Parse,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigLoadIssueKind {
    NoProfilePath,
    Io(std::io::ErrorKind),
    NotRegular,
    TooLarge,
    InvalidUtf8,
    InvalidToml,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConfigLoadIssue {
    pub candidate: Option<ConfigCandidate>,
    pub operation: ConfigLoadOperation,
    pub kind: ConfigLoadIssueKind,
}

impl std::fmt::Display for ConfigLoadIssue {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let source = match self.candidate {
            Some(ConfigCandidate::Primary) => "config.toml".to_owned(),
            Some(ConfigCandidate::Backup(slot)) => format!("backup {slot}"),
            None => "perfil de configuración".to_owned(),
        };
        let category = match self.kind {
            ConfigLoadIssueKind::NoProfilePath => "ruta de perfil no disponible",
            ConfigLoadIssueKind::Io(_) => "error de acceso al archivo",
            ConfigLoadIssueKind::NotRegular => "archivo no regular o enlace",
            ConfigLoadIssueKind::TooLarge => "archivo supera el límite de 4 MiB",
            ConfigLoadIssueKind::InvalidUtf8 => "contenido UTF-8 inválido",
            ConfigLoadIssueKind::InvalidToml => "contenido TOML inválido",
        };
        write!(formatter, "{source}: {category}")
    }
}

/// The report records provenance separately from the effective runtime value.
/// Parser errors and raw TOML are deliberately excluded from the issue.
#[derive(Clone, PartialEq)]
pub enum ConfigLoadReport {
    Missing,
    Loaded {
        config: AppConfig,
        source: ConfigCandidate,
    },
    Failed(ConfigLoadIssue),
}

impl std::fmt::Debug for ConfigLoadReport {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing => formatter.write_str("Missing"),
            Self::Loaded { source, .. } => formatter
                .debug_struct("Loaded")
                .field("source", source)
                .finish_non_exhaustive(),
            Self::Failed(issue) => formatter.debug_tuple("Failed").field(issue).finish(),
        }
    }
}

impl ConfigLoadReport {
    pub fn effective_config(&self) -> AppConfig {
        match self {
            Self::Loaded { config, .. } => config.clone(),
            Self::Missing | Self::Failed(_) => AppConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigRuntimeStatus {
    /// Compatibility for explicit callers that do not construct the native app.
    Uninitialized,
    Ready,
    Missing,
    Blocked(ConfigLoadIssue),
}

impl ConfigRuntimeStatus {
    pub fn write_permission(self) -> Result<(), ConfigWriteBlocked> {
        match self {
            Self::Blocked(issue) => Err(ConfigWriteBlocked(issue)),
            Self::Uninitialized | Self::Ready | Self::Missing => Ok(()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConfigWriteBlocked(pub ConfigLoadIssue);

impl std::fmt::Display for ConfigWriteBlocked {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "Configuración sin cargar: {}. Recuperá el archivo y reiniciá la app para volver a leerlo; todavía no se guardó ningún cambio.", self.0)
    }
}

impl std::error::Error for ConfigWriteBlocked {}

/// Pure state: tests can exercise permission without mutating the profile global.
#[derive(Clone)]
struct RuntimeConfigState {
    config: AppConfig,
    status: ConfigRuntimeStatus,
}

impl RuntimeConfigState {
    fn from_report(report: ConfigLoadReport) -> Self {
        let status = match &report {
            ConfigLoadReport::Missing => ConfigRuntimeStatus::Missing,
            ConfigLoadReport::Loaded { .. } => ConfigRuntimeStatus::Ready,
            ConfigLoadReport::Failed(issue) => ConfigRuntimeStatus::Blocked(*issue),
        };
        Self {
            config: report.effective_config(),
            status,
        }
    }

    fn known(config: AppConfig) -> Self {
        Self {
            config,
            status: ConfigRuntimeStatus::Ready,
        }
    }

    fn update(&mut self, config: AppConfig) {
        self.config = config;
        // Editing a fallback never establishes that the file was loaded.
    }
}

static RUNTIME_CONFIG: RwLock<Option<RuntimeConfigState>> = RwLock::new(None);

pub fn config_file_path() -> Option<PathBuf> {
    Some(crate::utils::app_paths::config_dir()?.join("config.toml"))
}

/// Compatibility wrapper; production startup consumes the typed report.
pub fn load() -> AppConfig {
    effective_load(load_result())
}

pub fn load_result() -> ConfigLoadReport {
    let Some(path) = config_file_path() else {
        return ConfigLoadReport::Failed(ConfigLoadIssue {
            candidate: None,
            operation: ConfigLoadOperation::ProfilePath,
            kind: ConfigLoadIssueKind::NoProfilePath,
        });
    };
    load_result_from_path(&path)
}

/// Explicit path wrapper for legacy callers and fixtures.
pub fn load_from_path(path: &std::path::Path) -> AppConfig {
    effective_load(load_result_from_path(path))
}

fn effective_load(report: ConfigLoadReport) -> AppConfig {
    if let ConfigLoadReport::Failed(issue) = &report {
        log::warn!("configuration not loaded: {issue}");
    }
    report.effective_config()
}

pub fn load_result_from_path(path: &std::path::Path) -> ConfigLoadReport {
    if let Err(error) = crate::state::durable_write::protect_private_state(path) {
        return ConfigLoadReport::Failed(ConfigLoadIssue {
            candidate: None,
            operation: ConfigLoadOperation::Privacy,
            kind: ConfigLoadIssueKind::Io(error.kind()),
        });
    }
    load_candidates_with(path, read_config_candidate)
}

fn load_candidates_with(
    path: &std::path::Path,
    mut read: impl FnMut(&std::path::Path, ConfigCandidate) -> Result<Option<Vec<u8>>, ConfigLoadIssue>,
) -> ConfigLoadReport {
    let mut invalid = None;
    let candidates = std::iter::once((path.to_path_buf(), ConfigCandidate::Primary)).chain(
        (0..crate::state::durable_write::BACKUP_SLOTS).map(|slot| {
            (
                crate::state::durable_write::backup_path(path, slot),
                ConfigCandidate::Backup(slot),
            )
        }),
    );
    for (candidate_path, candidate) in candidates {
        let bytes = match read(&candidate_path, candidate) {
            Ok(Some(bytes)) => bytes,
            Ok(None) => continue,
            Err(issue) => return ConfigLoadReport::Failed(issue),
        };
        let raw = match std::str::from_utf8(&bytes) {
            Ok(raw) => raw,
            Err(_) => {
                invalid = Some(ConfigLoadIssue {
                    candidate: Some(candidate),
                    operation: ConfigLoadOperation::Parse,
                    kind: ConfigLoadIssueKind::InvalidUtf8,
                });
                continue;
            }
        };
        let file = match toml::from_str::<ConfigFile>(raw) {
            Ok(file) => file,
            Err(_) => {
                invalid = Some(ConfigLoadIssue {
                    candidate: Some(candidate),
                    operation: ConfigLoadOperation::Parse,
                    kind: ConfigLoadIssueKind::InvalidToml,
                });
                continue;
            }
        };
        let mut config = AppConfig::default();
        apply_terminal_section(&mut config, file.terminal);
        if let Some(token) = file.integrations.linear_token {
            let token = token.trim().to_owned();
            config.linear_token = if token.is_empty() { None } else { Some(token) };
        }
        if let Some(dismissed) = file.onboarding.dismissed {
            config.onboarding_dismissed = dismissed;
        }
        return ConfigLoadReport::Loaded {
            config,
            source: candidate,
        };
    }
    match invalid {
        Some(issue) => ConfigLoadReport::Failed(issue),
        None => ConfigLoadReport::Missing,
    }
}

fn candidate_issue(
    candidate: ConfigCandidate,
    operation: ConfigLoadOperation,
    kind: ConfigLoadIssueKind,
) -> ConfigLoadIssue {
    ConfigLoadIssue {
        candidate: Some(candidate),
        operation,
        kind,
    }
}

fn retry_interrupted<T>(mut operation: impl FnMut() -> std::io::Result<T>) -> std::io::Result<T> {
    loop {
        match operation() {
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            result => return result,
        }
    }
}

fn read_config_candidate(
    path: &std::path::Path,
    candidate: ConfigCandidate,
) -> Result<Option<Vec<u8>>, ConfigLoadIssue> {
    // Also reject links before opening on platforms without O_NOFOLLOW.
    match retry_interrupted(|| std::fs::symlink_metadata(path)) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(candidate_issue(
                candidate,
                ConfigLoadOperation::Open,
                ConfigLoadIssueKind::NotRegular,
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(candidate_issue(
                candidate,
                ConfigLoadOperation::Metadata,
                ConfigLoadIssueKind::Io(error.kind()),
            ));
        }
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let mut file = match retry_interrupted(|| options.open(path)) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return match retry_interrupted(|| std::fs::symlink_metadata(path)) {
                Err(confirm) if confirm.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(confirm) => Err(candidate_issue(
                    candidate,
                    ConfigLoadOperation::Metadata,
                    ConfigLoadIssueKind::Io(confirm.kind()),
                )),
                Ok(_) => Err(candidate_issue(
                    candidate,
                    ConfigLoadOperation::Open,
                    ConfigLoadIssueKind::Io(error.kind()),
                )),
            };
        }
        Err(error) => {
            return Err(candidate_issue(
                candidate,
                ConfigLoadOperation::Open,
                ConfigLoadIssueKind::Io(error.kind()),
            ));
        }
    };
    let metadata = retry_interrupted(|| file.metadata()).map_err(|error| {
        candidate_issue(
            candidate,
            ConfigLoadOperation::Metadata,
            ConfigLoadIssueKind::Io(error.kind()),
        )
    })?;
    if !metadata.is_file() {
        return Err(candidate_issue(
            candidate,
            ConfigLoadOperation::Metadata,
            ConfigLoadIssueKind::NotRegular,
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.nlink() != 1 {
            return Err(candidate_issue(
                candidate,
                ConfigLoadOperation::Metadata,
                ConfigLoadIssueKind::NotRegular,
            ));
        }
    }
    read_config_bytes(&mut file, metadata.len(), MAX_CONFIG_FILE_BYTES)
        .map(Some)
        .map_err(|kind| candidate_issue(candidate, ConfigLoadOperation::Read, kind))
}

fn read_config_bytes(
    reader: &mut impl std::io::Read,
    advertised_len: u64,
    limit: usize,
) -> Result<Vec<u8>, ConfigLoadIssueKind> {
    let sentinel = limit.checked_add(1).ok_or(ConfigLoadIssueKind::TooLarge)?;
    if advertised_len > limit as u64 {
        return Err(ConfigLoadIssueKind::TooLarge);
    }
    let mut bytes = Vec::with_capacity((advertised_len as usize).min(8192));
    let mut chunk = [0u8; 8192];
    while bytes.len() < sentinel {
        let remaining = (sentinel - bytes.len()).min(chunk.len());
        let read = retry_interrupted(|| reader.read(&mut chunk[..remaining]))
            .map_err(|error| ConfigLoadIssueKind::Io(error.kind()))?;
        if read == 0 {
            return Ok(bytes);
        }
        bytes.extend_from_slice(&chunk[..read]);
        if bytes.len() > limit {
            return Err(ConfigLoadIssueKind::TooLarge);
        }
    }
    Err(ConfigLoadIssueKind::TooLarge)
}

fn apply_terminal_section(config: &mut AppConfig, terminal: TerminalSection) {
    if let Some(size) = terminal.font_size {
        config.font_size = clamp_font_size(size);
    }
    if let Some(lines) = terminal.scrollback_lines {
        config.scrollback_lines = lines.clamp(100, 1_000_000);
    }
    if let Some(allow) = terminal.allow_osc52 {
        config.allow_osc52 = allow;
    }
    if let Some(audio) = terminal.audio_bell {
        config.audio_bell = audio;
    }
    if let Some(copy_on_select) = terminal.copy_on_select {
        config.copy_on_select = copy_on_select;
    }
    if let Some(agent_notifications) = terminal.agent_notifications {
        config.agent_notifications = agent_notifications;
    }
    if let Some(shell) = terminal.shell {
        let shell = shell.trim().to_owned();
        config.shell = if shell.is_empty() { None } else { Some(shell) };
    }
}

/// Installs the effective value and its provenance together at startup.
pub fn install_runtime_config_load(report: ConfigLoadReport) {
    let mut guard = RUNTIME_CONFIG.write().unwrap_or_else(|p| p.into_inner());
    if guard.is_none() {
        *guard = Some(RuntimeConfigState::from_report(report));
    }
}

/// Compatibility for explicit known snapshots; a later call never heals a blocked load.
pub fn install_runtime_config(config: AppConfig) {
    let mut guard = RUNTIME_CONFIG.write().unwrap_or_else(|p| p.into_inner());
    if guard.is_none() {
        *guard = Some(RuntimeConfigState::known(config));
    }
}

/// Updates RAM without granting authority to overwrite an unread configuration.
pub fn update_runtime_config(config: AppConfig) {
    let mut guard = RUNTIME_CONFIG.write().unwrap_or_else(|p| p.into_inner());
    match guard.as_mut() {
        Some(state) => state.update(config),
        None => *guard = Some(RuntimeConfigState::known(config)),
    }
}

pub fn runtime_config() -> AppConfig {
    RUNTIME_CONFIG
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .as_ref()
        .map(|state| state.config.clone())
        .unwrap_or_default()
}

pub fn runtime_config_status() -> ConfigRuntimeStatus {
    RUNTIME_CONFIG
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .as_ref()
        .map(|state| state.status)
        .unwrap_or(ConfigRuntimeStatus::Uninitialized)
}

pub fn ensure_config_write_allowed() -> Result<(), ConfigWriteBlocked> {
    runtime_config_status().write_permission()
}

/// Persist only an explicit snapshot whose profile load is known.
/// The short state lock is released before serialization or filesystem I/O.
pub fn save(config: &AppConfig) -> anyhow::Result<()> {
    ensure_config_write_allowed()?;
    let path = config_file_path().ok_or_else(|| anyhow::anyhow!("No config dir"))?;
    save_to_path(config, &path)
}

/// Explicit destinations remain independent of the runtime profile global.
pub fn save_to_path(config: &AppConfig, path: &std::path::Path) -> anyhow::Result<()> {
    save_to_path_with_limit(config, path, MAX_CONFIG_FILE_BYTES)
}

fn save_to_path_with_limit(
    config: &AppConfig,
    path: &std::path::Path,
    limit: usize,
) -> anyhow::Result<()> {
    let raw = serialize_config_with_limit(config, limit)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    crate::state::durable_write::write_private_durable(path, raw.as_bytes())?;
    Ok(())
}

fn serialize_config_with_limit(config: &AppConfig, limit: usize) -> anyhow::Result<String> {
    let string_bytes = config
        .shell
        .as_ref()
        .map_or(0, String::len)
        .checked_add(config.linear_token.as_ref().map_or(0, String::len))
        .ok_or_else(|| anyhow::anyhow!("config.toml supera el límite de bytes"))?;
    if string_bytes > limit {
        anyhow::bail!("config.toml supera el límite de bytes");
    }
    let file = ConfigFile {
        terminal: TerminalSection {
            font_size: Some(config.font_size),
            scrollback_lines: Some(config.scrollback_lines),
            allow_osc52: Some(config.allow_osc52),
            audio_bell: Some(config.audio_bell),
            copy_on_select: Some(config.copy_on_select),
            agent_notifications: Some(config.agent_notifications),
            shell: config.shell.clone(),
        },
        integrations: IntegrationsSection {
            linear_token: config.linear_token.clone(),
        },
        onboarding: OnboardingSection {
            dismissed: Some(config.onboarding_dismissed),
        },
    };
    let raw = toml::to_string_pretty(&file)?;
    if raw.len() > limit {
        anyhow::bail!("config.toml supera el límite de bytes");
    }
    Ok(raw)
}

#[cfg(test)]
mod tests {
    use super::{AppConfig, ConfigFile};

    #[test]
    fn defaults_match_documented_values() {
        let config = AppConfig::default();
        assert_eq!(config.scrollback_lines, 10_000);
        assert!(!config.allow_osc52);
        assert!(!config.audio_bell);
    }

    #[test]
    fn parses_terminal_section_with_overrides() {
        let file: ConfigFile = toml::from_str(
            r#"
            [terminal]
            font_size = 13.0
            scrollback_lines = 25000
            allow_osc52 = true
            audio_bell = true
            "#,
        )
        .unwrap();
        assert_eq!(file.terminal.font_size, Some(13.0));
        assert_eq!(file.terminal.scrollback_lines, Some(25000));
        assert_eq!(file.terminal.allow_osc52, Some(true));
        assert_eq!(file.terminal.audio_bell, Some(true));
    }

    #[test]
    fn empty_file_yields_all_none() {
        let file: ConfigFile = toml::from_str("").unwrap();
        assert!(file.terminal.font_size.is_none());
        assert!(file.terminal.scrollback_lines.is_none());
    }

    #[test]
    fn parses_custom_shell() {
        let file: ConfigFile = toml::from_str(
            r#"
            [terminal]
            shell = "/opt/homebrew/bin/fish"
            "#,
        )
        .unwrap();
        assert_eq!(
            file.terminal.shell.as_deref(),
            Some("/opt/homebrew/bin/fish")
        );
    }

    #[test]
    fn save_and_load_round_trip() {
        let dir = std::env::temp_dir().join(format!("config-roundtrip-{}", uuid::Uuid::new_v4()));
        let path = dir.join("config.toml");

        // Cada campo difiere del default para que el round-trip falle si
        // alguno no se serializa o no se vuelve a leer.
        let config = AppConfig {
            font_size: 17.0,
            scrollback_lines: 42_000,
            allow_osc52: true,
            audio_bell: true,
            copy_on_select: true,
            agent_notifications: false,
            shell: Some("/bin/zsh".to_owned()),
            linear_token: Some("lin_api_test".to_owned()),
            onboarding_dismissed: true,
        };
        assert_ne!(config, AppConfig::default());

        super::save_to_path(&config, &path).expect("save config");
        let loaded = super::load_from_path(&path);
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(loaded.font_size, 17.0);
        assert_eq!(loaded.scrollback_lines, 42_000);
        assert!(loaded.allow_osc52);
        assert!(loaded.audio_bell);
        assert!(loaded.copy_on_select);
        assert!(!loaded.agent_notifications);
        assert_eq!(loaded.shell.as_deref(), Some("/bin/zsh"));
    }

    #[test]
    fn load_from_missing_path_yields_defaults() {
        let missing =
            std::env::temp_dir().join(format!("no-such-config-{}.toml", uuid::Uuid::new_v4()));
        let loaded = super::load_from_path(&missing);
        assert_eq!(loaded, AppConfig::default());
    }
}

#[cfg(all(test, unix))]
mod private_config_security_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn security_linear_token_is_saved_with_private_backups() {
        let root = std::env::temp_dir().join(format!("tc-config-private-{}", uuid::Uuid::new_v4()));
        let path = root.join("config.toml");
        let mut config = AppConfig {
            linear_token: Some("dummy-token-not-a-credential".to_owned()),
            ..Default::default()
        };
        save_to_path(&config, &path).unwrap();
        config.font_size += 1.0;
        save_to_path(&config, &path).unwrap();
        for file in [&path, &crate::state::durable_write::backup_path(&path, 0)] {
            assert_eq!(
                std::fs::metadata(file).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
mod config_recovery_tests {
    use super::*;
    use std::io::{self, Read};
    use std::path::{Path, PathBuf};

    struct TempDirectory(PathBuf);

    impl TempDirectory {
        fn new() -> Self {
            let path =
                std::env::temp_dir().join(format!("tc-config-recovery-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> PathBuf {
            self.0.join("config.toml")
        }
    }

    impl Drop for TempDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn entries(path: &Path) -> Vec<(PathBuf, Option<Vec<u8>>)> {
        std::iter::once(path.to_path_buf())
            .chain(
                (0..crate::state::durable_write::BACKUP_SLOTS)
                    .map(|slot| crate::state::durable_write::backup_path(path, slot)),
            )
            .map(|entry| {
                let bytes = std::fs::read(&entry).ok();
                (entry, bytes)
            })
            .collect()
    }

    fn read_error(candidate: ConfigCandidate) -> ConfigLoadIssue {
        candidate_issue(
            candidate,
            ConfigLoadOperation::Read,
            ConfigLoadIssueKind::Io(io::ErrorKind::PermissionDenied),
        )
    }

    #[test]
    fn missing_empty_and_recovered_configs_have_distinct_known_provenance() {
        let dir = TempDirectory::new();
        let path = dir.path();
        assert_eq!(load_result_from_path(&path), ConfigLoadReport::Missing);
        std::fs::write(&path, "").unwrap();
        assert!(
            matches!(load_result_from_path(&path), ConfigLoadReport::Loaded { source: ConfigCandidate::Primary, config } if config == AppConfig::default())
        );
        std::fs::write(&path, "[broken").unwrap();
        let backup = crate::state::durable_write::backup_path(&path, 0);
        std::fs::write(&backup, "[terminal]\nfont_size = 19.0\nshell = ' /bin/fish '\n[integrations]\nlinear_token = ' dummy-token '\n[onboarding]\ndismissed = true\n").unwrap();
        let before = entries(&path);
        let ConfigLoadReport::Loaded { config, source } = load_result_from_path(&path) else {
            panic!("valid backup was not recovered")
        };
        assert_eq!(source, ConfigCandidate::Backup(0));
        assert_eq!(config.font_size, 19.0);
        assert_eq!(config.shell.as_deref(), Some("/bin/fish"));
        assert_eq!(config.linear_token.as_deref(), Some("dummy-token"));
        assert!(config.onboarding_dismissed);
        assert_eq!(entries(&path), before);
    }

    #[test]
    fn all_readable_invalid_candidates_block_and_preserve_the_ring() {
        let dir = TempDirectory::new();
        let path = dir.path();
        for (entry, _) in entries(&path) {
            std::fs::write(entry, "[broken").unwrap();
        }
        let before = entries(&path);
        let report = load_result_from_path(&path);
        assert!(matches!(
            report,
            ConfigLoadReport::Failed(ConfigLoadIssue {
                kind: ConfigLoadIssueKind::InvalidToml,
                ..
            })
        ));
        assert_eq!(entries(&path), before);
        assert!(RuntimeConfigState::from_report(report)
            .status
            .write_permission()
            .is_err());
    }

    #[test]
    fn unreadable_candidates_never_fall_through_to_older_valid_backups() {
        for blocked in [ConfigCandidate::Primary, ConfigCandidate::Backup(0)] {
            let mut calls = Vec::new();
            let report = load_candidates_with(Path::new("config.toml"), |_, candidate| {
                calls.push(candidate);
                if candidate == blocked {
                    Err(read_error(candidate))
                } else if candidate == ConfigCandidate::Primary {
                    Ok(Some(b"[broken".to_vec()))
                } else {
                    Ok(Some(b"[terminal]\nfont_size = 27.0\n".to_vec()))
                }
            });
            assert_eq!(report, ConfigLoadReport::Failed(read_error(blocked)));
            assert_eq!(calls.last(), Some(&blocked));
            assert!(!calls.contains(&ConfigCandidate::Backup(1)));
        }
    }

    struct FaultReader<'a> {
        bytes: &'a [u8],
        error_after_bytes: bool,
        interrupt_once: bool,
        calls: usize,
        requested: usize,
    }

    impl Read for FaultReader<'_> {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            self.calls += 1;
            self.requested += output.len();
            if std::mem::take(&mut self.interrupt_once) {
                return Err(io::Error::from(io::ErrorKind::Interrupted));
            }
            if self.bytes.is_empty() {
                return if self.error_after_bytes {
                    Err(io::Error::from(io::ErrorKind::PermissionDenied))
                } else {
                    Ok(0)
                };
            }
            let read = self.bytes.len().min(output.len());
            output[..read].copy_from_slice(&self.bytes[..read]);
            self.bytes = &self.bytes[read..];
            Ok(read)
        }
    }

    fn reader(bytes: &[u8]) -> FaultReader<'_> {
        FaultReader {
            bytes,
            error_after_bytes: false,
            interrupt_once: false,
            calls: 0,
            requested: 0,
        }
    }

    #[test]
    fn full_toml_prefix_followed_by_read_error_is_never_accepted() {
        let valid = b"[terminal]\nfont_size = 24.0\n";
        assert!(toml::from_str::<ConfigFile>(std::str::from_utf8(valid).unwrap()).is_ok());
        let mut input = reader(valid);
        input.error_after_bytes = true;
        let mut calls = Vec::new();
        let report = load_candidates_with(Path::new("config.toml"), |_, candidate| {
            calls.push(candidate);
            read_config_bytes(&mut input, valid.len() as u64, 128)
                .map(Some)
                .map_err(|kind| candidate_issue(candidate, ConfigLoadOperation::Read, kind))
        });
        assert_eq!(calls, [ConfigCandidate::Primary]);
        assert!(matches!(
            report,
            ConfigLoadReport::Failed(ConfigLoadIssue {
                kind: ConfigLoadIssueKind::Io(io::ErrorKind::PermissionDenied),
                ..
            })
        ));
        assert!(input.calls >= 2);
    }

    #[test]
    fn exact_limit_sentinel_growth_and_huge_metadata_are_bounded() {
        let mut exact = reader(b"    ");
        assert_eq!(read_config_bytes(&mut exact, 4, 4).unwrap(), b"    ");
        let mut growth = reader(b"     ");
        assert_eq!(
            read_config_bytes(&mut growth, 4, 4),
            Err(ConfigLoadIssueKind::TooLarge)
        );
        assert!(growth.requested <= 5);
        let mut huge = reader(b"");
        assert_eq!(
            read_config_bytes(&mut huge, u64::MAX, 4),
            Err(ConfigLoadIssueKind::TooLarge)
        );
        assert_eq!(huge.calls, 0);
        let mut overflow = reader(b"");
        assert_eq!(
            read_config_bytes(&mut overflow, 0, usize::MAX),
            Err(ConfigLoadIssueKind::TooLarge)
        );
        assert_eq!(overflow.calls, 0);
    }
}

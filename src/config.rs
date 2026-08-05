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
        }
    }
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct ConfigFile {
    #[serde(default)]
    terminal: TerminalSection,
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

static RUNTIME_CONFIG: RwLock<Option<AppConfig>> = RwLock::new(None);

pub fn config_file_path() -> Option<PathBuf> {
    let dirs = directories::ProjectDirs::from("", "", "terminal-app")?;
    Some(dirs.config_dir().join("config.toml"))
}

/// Carga la config desde disco, resolviendo ausentes/inválidos a defaults.
pub fn load() -> AppConfig {
    let Some(path) = config_file_path() else {
        return AppConfig::default();
    };
    load_from_path(&path)
}

/// Carga la config desde un path específico (testeable).
pub fn load_from_path(path: &std::path::Path) -> AppConfig {
    let mut config = AppConfig::default();
    let Ok(raw) = std::fs::read_to_string(path) else {
        return config;
    };
    match toml::from_str::<ConfigFile>(&raw) {
        Ok(file) => {
            let terminal = file.terminal;
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
        Err(err) => {
            log::warn!("config.toml inválido ({}): se usan defaults", err);
        }
    }
    config
}

/// Instala la config runtime (arranque). Solo la primera llamada importa.
pub fn install_runtime_config(config: AppConfig) {
    let mut guard = RUNTIME_CONFIG.write().unwrap_or_else(|p| p.into_inner());
    if guard.is_none() {
        *guard = Some(config);
    }
}

/// Reemplaza la config runtime en vivo (desde el diálogo de configuración).
pub fn update_runtime_config(config: AppConfig) {
    let mut guard = RUNTIME_CONFIG.write().unwrap_or_else(|p| p.into_inner());
    *guard = Some(config);
}

/// Config vigente; si nadie instaló nada (tests, arranque raro) devuelve
/// defaults.
pub fn runtime_config() -> AppConfig {
    RUNTIME_CONFIG
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .clone()
        .unwrap_or_default()
}

/// Persiste la config vigente a `config.toml`. Best-effort: devuelve Err si
/// no se pudo escribir.
pub fn save(config: &AppConfig) -> anyhow::Result<()> {
    let path = config_file_path().ok_or_else(|| anyhow::anyhow!("No config dir"))?;
    save_to_path(config, &path)
}

/// Guarda la config en un path específico (testeable).
pub fn save_to_path(config: &AppConfig, path: &std::path::Path) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
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
    };
    let raw = toml::to_string_pretty(&file)?;
    std::fs::write(path, raw)?;
    Ok(())
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

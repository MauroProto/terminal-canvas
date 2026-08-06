//! Detección de agentes instalados (Ship-it 7.2, T1).
//!
//! Al arrancar se resuelve cada `launch_command` contra el PATH en un worker,
//! para que el launcher muestre solo lo que el usuario **puede** lanzar y dé
//! un hint de instalación para el resto, en vez de fallar al spawnear.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::Receiver;

use super::manager::{launch_presets, AgentProvider};

/// Hint de instalación por provider, para el que no está.
pub fn install_hint(provider: AgentProvider) -> Option<&'static str> {
    match provider {
        AgentProvider::ClaudeCode => Some("npm i -g @anthropic-ai/claude-code"),
        AgentProvider::CodexCli => Some("npm i -g @openai/codex"),
        AgentProvider::GeminiCli => Some("npm i -g @google/gemini-cli"),
        AgentProvider::Aider => Some("pipx install aider-chat"),
        AgentProvider::OpenCode => Some("npm i -g opencode-ai"),
        AgentProvider::CursorAgent => Some("curl https://cursor.com/install -fsS | bash"),
        AgentProvider::Copilot => Some("npm i -g @github/copilot"),
        AgentProvider::Goose => Some("brew install block-goose-cli"),
        AgentProvider::Amp => Some("npm i -g @sourcegraph/amp"),
        AgentProvider::Crush => Some("brew install charmbracelet/tap/crush"),
        AgentProvider::Unknown => None,
    }
}

/// ¿Está el binario en el PATH? Resuelve sin ejecutar nada (un `--version`
/// puede colgarse o pedir login).
pub fn resolve_in_path(command: &str, path_var: &str) -> Option<PathBuf> {
    if command.contains('/') {
        // Ya es un path: solo se verifica que exista y sea ejecutable.
        let path = PathBuf::from(command);
        return is_executable(&path).then_some(path);
    }
    for dir in std::env::split_paths(path_var) {
        let candidate = dir.join(command);
        if is_executable(&candidate) {
            return Some(candidate);
        }
    }
    None
}

#[cfg(unix)]
fn is_executable(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &std::path::Path) -> bool {
    path.is_file()
}

/// Qué providers están disponibles ahora mismo.
#[derive(Debug, Clone, Default)]
pub struct InstalledAgents {
    available: HashMap<AgentProvider, PathBuf>,
    /// `false` mientras el worker todavía no contestó: la UI muestra todo
    /// hasta saber, en vez de esconder lo que sí está.
    pub resolved: bool,
}

impl InstalledAgents {
    pub fn is_available(&self, provider: AgentProvider) -> bool {
        // Antes de resolver no escondemos nada.
        !self.resolved || self.available.contains_key(&provider)
    }

    pub fn path_of(&self, provider: AgentProvider) -> Option<&PathBuf> {
        self.available.get(&provider)
    }

    pub fn count(&self) -> usize {
        self.available.len()
    }

    /// ¿Terminó la detección y no hay ni un agente instalado?
    pub fn none_installed(&self) -> bool {
        self.resolved && self.available.is_empty()
    }
}

/// Corre la detección en un hilo: tocar el filesystem por cada provider no va
/// en el frame.
pub struct AgentDetector {
    receiver: Option<Receiver<InstalledAgents>>,
}

impl Default for AgentDetector {
    fn default() -> Self {
        Self::start()
    }
}

impl AgentDetector {
    pub fn start() -> Self {
        let (sender, receiver) = std::sync::mpsc::channel();
        let path_var = std::env::var("PATH").unwrap_or_default();
        let spawned = std::thread::Builder::new()
            .name("agent-detector".to_owned())
            .spawn(move || {
                let _ = sender.send(detect_installed(&path_var));
            })
            .is_ok();
        Self {
            receiver: spawned.then_some(receiver),
        }
    }

    /// Devuelve el resultado apenas esté listo (una sola vez).
    pub fn poll(&mut self) -> Option<InstalledAgents> {
        let result = self.receiver.as_ref()?.try_recv().ok()?;
        self.receiver = None;
        Some(result)
    }
}

/// Resuelve todos los providers contra un PATH dado (testeable).
pub fn detect_installed(path_var: &str) -> InstalledAgents {
    let mut available = HashMap::new();
    for provider in launch_presets() {
        let Some(command) = provider.launch_command() else {
            continue;
        };
        if let Some(path) = resolve_in_path(command, path_var) {
            available.insert(provider, path);
        }
    }
    InstalledAgents {
        available,
        resolved: true,
    }
}

#[cfg(test)]
mod tests {
    use super::{detect_installed, install_hint, resolve_in_path, InstalledAgents};
    use crate::orchestration::AgentProvider;

    /// Crea un PATH falso con un "binario" ejecutable adentro.
    fn fake_path_with(command: &str) -> (std::path::PathBuf, String) {
        let dir = std::env::temp_dir().join(format!("agents-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let binary = dir.join(command);
        std::fs::write(&binary, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let path_var = dir.to_string_lossy().into_owned();
        (dir, path_var)
    }

    #[test]
    fn resolves_a_command_present_in_the_path() {
        let (dir, path_var) = fake_path_with("claude");
        assert!(resolve_in_path("claude", &path_var).is_some());
        assert!(resolve_in_path("codex", &path_var).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_non_executable_file_does_not_count() {
        use std::os::unix::fs::PermissionsExt;
        let (dir, path_var) = fake_path_with("claude");
        std::fs::set_permissions(dir.join("claude"), std::fs::Permissions::from_mode(0o644))
            .unwrap();
        assert!(
            resolve_in_path("claude", &path_var).is_none(),
            "un archivo sin +x no es un agente instalado"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn detection_only_reports_what_is_really_there() {
        let (dir, path_var) = fake_path_with("claude");
        let installed = detect_installed(&path_var);
        assert!(installed.resolved);
        assert!(installed.is_available(AgentProvider::ClaudeCode));
        assert!(!installed.is_available(AgentProvider::CodexCli));
        assert_eq!(installed.count(), 1);
        assert!(!installed.none_installed());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_empty_path_means_nothing_is_installed() {
        let installed = detect_installed("");
        assert!(installed.none_installed());
        assert_eq!(installed.count(), 0);
    }

    #[test]
    fn before_resolving_nothing_is_hidden() {
        // Sin resolver todavía, la UI no puede esconder providers: sería peor
        // mostrar la lista vacía un instante que mostrar de más.
        let pending = InstalledAgents::default();
        assert!(!pending.resolved);
        assert!(pending.is_available(AgentProvider::ClaudeCode));
        assert!(pending.is_available(AgentProvider::Crush));
        assert!(!pending.none_installed(), "sin resolver no se afirma nada");
    }

    #[test]
    fn every_real_provider_has_an_install_hint() {
        for provider in crate::orchestration::launch_presets() {
            assert!(
                install_hint(provider).is_some(),
                "falta hint para {provider:?}"
            );
        }
        assert!(install_hint(AgentProvider::Unknown).is_none());
    }
}

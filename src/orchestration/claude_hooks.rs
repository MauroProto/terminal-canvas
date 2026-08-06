//! Instalación de nuestros hooks en `~/.claude/settings.json` (P2.12, T2).
//!
//! El merge es cuidadoso: se insertan grupos marcados con
//! `"_managed_by": "terminalcanvas"` y **nunca** se tocan los hooks del
//! usuario. Desinstalar saca exactamente los grupos marcados y deja el resto
//! intacto. Si el archivo tiene una forma inesperada, se prefiere no tocar
//! nada antes que romper la config del usuario.

use std::path::PathBuf;

use serde_json::{json, Map, Value};

/// Marca que identifica los grupos de hooks que administra la app.
pub const MANAGED_MARKER: &str = "terminalcanvas";

/// Eventos de Claude que nos interesan.
pub const MANAGED_EVENTS: &[&str] = &[
    "Stop",
    "UserPromptSubmit",
    "PermissionRequest",
    "PreToolUse",
];

/// Path del settings.json de Claude.
pub fn settings_path() -> Option<PathBuf> {
    let base = directories::BaseDirs::new()?;
    Some(base.home_dir().join(".claude").join("settings.json"))
}

/// Path del endpoint file que sourcean los hooks.
fn endpoint_file_literal() -> String {
    super::hook_server::hooks_dir()
        .map(|dir| dir.join("endpoint.sh").to_string_lossy().into_owned())
        .unwrap_or_else(|| "$HOME/.local/share/terminal-app/agent-hooks/endpoint.sh".to_owned())
}

/// Comando sh de 3 líneas: sourcea el endpoint file, corta si no hay servidor,
/// y postea el payload de stdin con timeouts cortos. Nunca falla el hook (el
/// `|| true` final) para no bloquear al agente si la app está cerrada.
pub fn hook_command(endpoint_file: &str) -> String {
    format!(
        ". \"{endpoint_file}\" 2>/dev/null || exit 0\n\
         [ -n \"$TC_HOOK_URL\" ] || exit 0\n\
         curl -sS -m 1.5 --connect-timeout 0.5 -H \"X-TC-Token: $TC_HOOK_TOKEN\" \
-H 'Content-Type: application/json' -d @- \
\"$TC_HOOK_URL/hook/claude?panel=$TC_PANEL_ID&workspace=$TC_WORKSPACE_ID\" >/dev/null 2>&1 || true\n"
    )
}

/// Grupo de hook nuestro, marcado para poder desinstalarlo sin ambigüedad.
fn managed_group(command: &str) -> Value {
    json!({
        "_managed_by": MANAGED_MARKER,
        "matcher": "",
        "hooks": [ { "type": "command", "command": command } ],
    })
}

fn is_managed(group: &Value) -> bool {
    group
        .get("_managed_by")
        .and_then(Value::as_str)
        .is_some_and(|marker| marker == MANAGED_MARKER)
}

/// Inserta nuestros hooks preservando los del usuario. Idempotente: instalar
/// dos veces no duplica grupos (se reemplaza el nuestro por el nuevo comando).
pub fn install_hooks(settings: &mut Value, endpoint_file: &str) {
    let command = hook_command(endpoint_file);
    let root = match settings.as_object_mut() {
        Some(root) => root,
        // Settings que no es un objeto: no lo tocamos.
        None => return,
    };
    let hooks = root
        .entry("hooks")
        .or_insert_with(|| Value::Object(Map::new()));
    let Some(hooks) = hooks.as_object_mut() else {
        // `hooks` con una forma rara: preferimos no pisar nada del usuario.
        return;
    };
    for event in MANAGED_EVENTS {
        let entry = hooks
            .entry((*event).to_owned())
            .or_insert_with(|| Value::Array(Vec::new()));
        let Some(groups) = entry.as_array_mut() else {
            continue;
        };
        // Sacamos el nuestro viejo (si hay) y agregamos el actualizado: así
        // el comando queda al día y nunca se duplica.
        groups.retain(|group| !is_managed(group));
        groups.push(managed_group(&command));
    }
}

/// Saca solo nuestros grupos. Los eventos que quedan vacíos se eliminan, y si
/// `hooks` queda vacío también, para dejar el archivo como estaba.
pub fn uninstall_hooks(settings: &mut Value) {
    let Some(root) = settings.as_object_mut() else {
        return;
    };
    let Some(hooks) = root.get_mut("hooks").and_then(Value::as_object_mut) else {
        return;
    };
    let events: Vec<String> = hooks.keys().cloned().collect();
    for event in events {
        let Some(groups) = hooks.get_mut(&event).and_then(Value::as_array_mut) else {
            continue;
        };
        groups.retain(|group| !is_managed(group));
        if groups.is_empty() {
            hooks.remove(&event);
        }
    }
    if hooks.is_empty() {
        root.remove("hooks");
    }
}

/// Lee el settings.json (o un objeto vacío si no existe / está corrupto).
fn load_settings(path: &std::path::Path) -> Value {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| Value::Object(Map::new()))
}

/// Instala los hooks en el settings real del usuario, con escritura durable.
pub fn install_to_disk() -> anyhow::Result<()> {
    let Some(path) = settings_path() else {
        anyhow::bail!("no se pudo resolver ~/.claude/settings.json");
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut settings = load_settings(&path);
    install_hooks(&mut settings, &endpoint_file_literal());
    let text = serde_json::to_string_pretty(&settings)?;
    crate::state::durable_write::write_durable(&path, text.as_bytes())?;
    Ok(())
}

/// Saca los hooks del settings real del usuario.
pub fn uninstall_from_disk() -> anyhow::Result<()> {
    let Some(path) = settings_path() else {
        anyhow::bail!("no se pudo resolver ~/.claude/settings.json");
    };
    if !path.exists() {
        return Ok(());
    }
    let mut settings = load_settings(&path);
    uninstall_hooks(&mut settings);
    let text = serde_json::to_string_pretty(&settings)?;
    crate::state::durable_write::write_durable(&path, text.as_bytes())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{hook_command, install_hooks, uninstall_hooks, MANAGED_EVENTS, MANAGED_MARKER};
    use serde_json::{json, Value};

    /// Settings realista con hooks propios del usuario.
    fn user_settings() -> Value {
        json!({
            "model": "claude-sonnet-4-6",
            "hooks": {
                "Stop": [
                    {
                        "matcher": "",
                        "hooks": [ { "type": "command", "command": "echo mio" } ]
                    }
                ],
                "PostToolUse": [
                    {
                        "matcher": "Bash",
                        "hooks": [ { "type": "command", "command": "notify-send algo" } ]
                    }
                ]
            }
        })
    }

    fn managed_count(settings: &Value, event: &str) -> usize {
        settings["hooks"][event]
            .as_array()
            .map(|groups| {
                groups
                    .iter()
                    .filter(|group| {
                        group.get("_managed_by").and_then(Value::as_str) == Some(MANAGED_MARKER)
                    })
                    .count()
            })
            .unwrap_or(0)
    }

    #[test]
    fn install_preserves_user_hooks() {
        let mut settings = user_settings();
        install_hooks(&mut settings, "/tmp/endpoint.sh");
        // El hook propio del usuario en Stop sigue ahí.
        let stop = settings["hooks"]["Stop"].as_array().unwrap();
        assert!(
            stop.iter()
                .any(|group| group["hooks"][0]["command"] == "echo mio"),
            "el hook del usuario se perdió: {stop:?}"
        );
        // Y su PostToolUse, que no administramos, queda intacto.
        assert_eq!(
            settings["hooks"]["PostToolUse"][0]["hooks"][0]["command"],
            "notify-send algo"
        );
        // Otras claves del settings tampoco se tocan.
        assert_eq!(settings["model"], "claude-sonnet-4-6");
    }

    #[test]
    fn install_adds_every_managed_event() {
        let mut settings = user_settings();
        install_hooks(&mut settings, "/tmp/endpoint.sh");
        for event in MANAGED_EVENTS {
            assert_eq!(managed_count(&settings, event), 1, "falta {event}");
        }
    }

    #[test]
    fn installing_twice_is_idempotent() {
        let mut settings = user_settings();
        install_hooks(&mut settings, "/tmp/endpoint.sh");
        install_hooks(&mut settings, "/tmp/endpoint.sh");
        for event in MANAGED_EVENTS {
            assert_eq!(managed_count(&settings, event), 1, "duplicado en {event}");
        }
    }

    #[test]
    fn reinstalling_updates_the_command() {
        let mut settings = user_settings();
        install_hooks(&mut settings, "/viejo/endpoint.sh");
        install_hooks(&mut settings, "/nuevo/endpoint.sh");
        let command = settings["hooks"]["Stop"]
            .as_array()
            .unwrap()
            .iter()
            .find(|group| group.get("_managed_by").is_some())
            .unwrap()["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(command.contains("/nuevo/endpoint.sh"), "got {command}");
        assert!(!command.contains("/viejo/endpoint.sh"));
    }

    #[test]
    fn uninstall_removes_only_ours() {
        let mut settings = user_settings();
        install_hooks(&mut settings, "/tmp/endpoint.sh");
        uninstall_hooks(&mut settings);
        // El del usuario sobrevive.
        assert_eq!(
            settings["hooks"]["Stop"][0]["hooks"][0]["command"],
            "echo mio"
        );
        assert_eq!(
            settings["hooks"]["PostToolUse"][0]["hooks"][0]["command"],
            "notify-send algo"
        );
        for event in MANAGED_EVENTS {
            assert_eq!(managed_count(&settings, event), 0, "quedó uno en {event}");
        }
    }

    #[test]
    fn uninstall_drops_events_that_end_up_empty() {
        let mut settings = json!({});
        install_hooks(&mut settings, "/tmp/endpoint.sh");
        uninstall_hooks(&mut settings);
        // Sin hooks del usuario, el objeto entero se va: el archivo queda
        // como antes de que lo tocáramos.
        assert!(settings.get("hooks").is_none(), "got {settings:?}");
    }

    #[test]
    fn uninstall_without_our_hooks_is_a_noop() {
        let mut settings = user_settings();
        let before = settings.clone();
        uninstall_hooks(&mut settings);
        assert_eq!(settings, before);
    }

    #[test]
    fn a_hooks_key_with_a_weird_shape_is_left_alone() {
        // `hooks` como string: no es algo que sepamos mergear.
        let mut settings = json!({ "hooks": "no soy un objeto" });
        let before = settings.clone();
        install_hooks(&mut settings, "/tmp/endpoint.sh");
        assert_eq!(settings, before, "no pisamos config que no entendemos");
    }

    #[test]
    fn the_command_is_three_lines_and_times_out() {
        let command = hook_command("/tmp/endpoint.sh");
        assert_eq!(command.lines().count(), 3, "got {command}");
        assert!(command.contains("-m 1.5"));
        assert!(command.contains("--connect-timeout 0.5"));
        assert!(command.contains("X-TC-Token"));
        assert!(command.contains("/hook/claude"));
        assert!(command.ends_with("|| true\n"), "nunca falla el hook");
    }
}

use crate::collab::PanelShareScope;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum SnapSlot {
    LeftHalf,
    RightHalf,
    TopHalf,
    BottomHalf,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
    #[default]
    Maximized,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum PanelPlacement {
    #[default]
    Floating,
    Snapped(SnapSlot),
    Maximized,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SavedPanelBounds {
    pub position: [f32; 2],
    pub size: [f32; 2],
}

impl SavedPanelBounds {
    pub fn new(position: [f32; 2], size: [f32; 2]) -> Self {
        Self { position, size }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PanelState {
    #[serde(default)]
    pub leaf_memory_task_ids: BTreeMap<String, String>,
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub custom_title: Option<String>,
    pub position: [f32; 2],
    pub size: [f32; 2],
    pub color: [u8; 3],
    pub z_index: u32,
    pub focused: bool,
    #[serde(default)]
    pub minimized: bool,
    #[serde(default)]
    pub placement: PanelPlacement,
    #[serde(default)]
    pub restore_placement: Option<PanelPlacement>,
    #[serde(default)]
    pub restore_bounds: Option<SavedPanelBounds>,
    #[serde(default)]
    pub share_scope: PanelShareScope,
    /// Comando de agente con el que se lanzó este panel (`claude`, `opencode`,
    /// …). Se persiste para poder volver a entrar al agente al restaurar; sin
    /// esto el panel volvía como shell pelado y la conversación quedaba
    /// huérfana en el historial del CLI.
    #[serde(default)]
    pub agent_command: Option<String>,
    /// Comando de agente por hoja de split. `agent_command` sigue siendo el
    /// alias legado de la raíz.
    #[serde(default)]
    pub leaf_agent_commands: BTreeMap<String, String>,
    /// El panel produjo atención (bell / agente esperando) mientras no estaba
    /// enfocado y nadie lo interactuó todavía (P1.8). Se limpia al interactuar.
    #[serde(default)]
    pub unread: bool,
    /// Árbol de splits del panel (P2.11), serializado como JSON opaco.
    #[serde(default)]
    pub split_tree: Option<serde_json::Value>,
    /// Hoja con el foco de teclado dentro del split (P2.11).
    #[serde(default)]
    pub focused_leaf: Option<String>,
    /// Identidad estable de la hoja respaldada por la sesión raíz del panel.
    ///
    /// Es opcional para poder leer layouts anteriores a la persistencia
    /// multi-hoja. En ese caso se infiere de la primera hoja válida del árbol.
    #[serde(default)]
    pub root_leaf: Option<String>,
    /// Issue de GitHub que este panel está trabajando (P2.13).
    #[serde(default)]
    pub linked_issue: Option<u64>,
    /// Id de sesión que reportó el hook del agente (P2.12, T3). Con esto el
    /// restore reanuda con `--resume <id>` exacto en vez de `--continue`.
    #[serde(default)]
    pub agent_session_id: Option<String>,
    /// Id de conversación exacto por hoja. `agent_session_id` conserva
    /// compatibilidad con layouts anteriores de una sola hoja.
    #[serde(default)]
    pub leaf_agent_session_ids: BTreeMap<String, String>,
    /// Id de la sesión de runtime (P3.15, T4). Con el daemon hosteando, esto
    /// deja que al reabrir la app el panel se **reengancha** a su PTY vivo en
    /// vez de arrancar uno nuevo.
    #[serde(default)]
    pub runtime_session_id: Option<String>,
    /// Sesión de runtime por identidad de hoja. Un `BTreeMap` mantiene estable
    /// el JSON generado y facilita inspeccionar/diffear el estado persistido.
    ///
    /// `runtime_session_id` sigue escribiéndose y leyéndose como alias legado
    /// de la raíz para que el cambio no requiera una migración destructiva.
    #[serde(default)]
    pub leaf_runtime_session_ids: BTreeMap<String, String>,
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::json;

    use super::{PanelPlacement, PanelState, SavedPanelBounds};
    use crate::collab::PanelShareScope;

    fn sample_panel_state() -> PanelState {
        PanelState {
            leaf_memory_task_ids: BTreeMap::new(),
            id: "panel-1".to_owned(),
            title: "Terminal".to_owned(),
            custom_title: None,
            position: [40.0, 72.0],
            size: [920.0, 640.0],
            color: [30, 40, 50],
            z_index: 4,
            focused: true,
            minimized: false,
            placement: PanelPlacement::Floating,
            restore_placement: None,
            restore_bounds: Some(SavedPanelBounds::new([40.0, 72.0], [920.0, 640.0])),
            share_scope: PanelShareScope::VisibleOnly,
            agent_command: None,
            leaf_agent_commands: BTreeMap::new(),
            unread: false,
            split_tree: None,
            focused_leaf: None,
            root_leaf: None,
            linked_issue: None,
            agent_session_id: None,
            leaf_agent_session_ids: BTreeMap::new(),
            runtime_session_id: None,
            leaf_runtime_session_ids: BTreeMap::new(),
        }
    }

    #[test]
    fn panel_state_serializes_placement_and_restore_bounds() {
        let value =
            serde_json::to_value(sample_panel_state()).expect("panel state should serialize");

        assert_eq!(value["placement"], json!("floating"));
        assert_eq!(
            value["restore_bounds"],
            json!({
                "position": [40.0, 72.0],
                "size": [920.0, 640.0],
            })
        );
    }

    #[test]
    fn panel_state_deserializes_legacy_without_placement_fields() {
        let value = json!({
            "id": "panel-1",
            "title": "Terminal",
            "custom_title": null,
            "position": [40.0, 72.0],
            "size": [920.0, 640.0],
            "color": [30, 40, 50],
            "z_index": 4,
            "focused": true,
            "minimized": false,
            "share_scope": "VisibleOnly"
        });

        let state: PanelState =
            serde_json::from_value(value).expect("legacy panel state should deserialize");

        assert_eq!(state.position, [40.0, 72.0]);
        assert_eq!(state.size, [920.0, 640.0]);
        assert_eq!(state.root_leaf, None);
        assert!(state.leaf_runtime_session_ids.is_empty());
    }

    #[test]
    fn panel_state_round_trips_stable_leaf_runtime_identity() {
        let root = uuid::Uuid::new_v4();
        let runtime = uuid::Uuid::new_v4();
        let root_text = root.to_string();
        let runtime_text = runtime.to_string();
        let mut state = sample_panel_state();
        state.root_leaf = Some(root_text.clone());
        state
            .leaf_runtime_session_ids
            .insert(root_text.clone(), runtime_text.clone());

        let encoded = serde_json::to_value(&state).expect("panel state should serialize");
        let decoded: PanelState =
            serde_json::from_value(encoded).expect("panel state should deserialize");

        assert_eq!(decoded.root_leaf.as_deref(), Some(root_text.as_str()));
        assert_eq!(
            decoded
                .leaf_runtime_session_ids
                .get(&root_text)
                .map(String::as_str),
            Some(runtime_text.as_str())
        );
    }
}

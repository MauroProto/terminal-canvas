use crate::collab::PanelShareScope;
use serde::{Deserialize, Serialize};

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
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{PanelPlacement, PanelState, SavedPanelBounds};
    use crate::collab::PanelShareScope;

    fn sample_panel_state() -> PanelState {
        PanelState {
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
    }
}

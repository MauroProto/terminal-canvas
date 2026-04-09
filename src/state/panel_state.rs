use serde::{Deserialize, Serialize};

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
}

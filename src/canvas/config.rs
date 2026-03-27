use egui::{vec2, Color32, Vec2};

pub const ZOOM_MIN: f32 = 0.125;
pub const ZOOM_MAX: f32 = 4.0;
pub const ZOOM_KEYBOARD_FACTOR: f32 = 1.15;
pub const GRID_SPACING: f32 = 40.0;
pub const GRID_COLOR: Color32 = Color32::from_rgb(30, 30, 30);
pub const SNAP_THRESHOLD: f32 = 8.0;
pub const MINIMAP_WIDTH: f32 = 200.0;
pub const MINIMAP_HEIGHT: f32 = 150.0;
pub const MINIMAP_PADDING: f32 = 10.0;
pub const MINIMAP_BG: Color32 = Color32::from_rgba_premultiplied(15, 15, 15, 200);
pub const MINIMAP_VIEWPORT_BORDER: Color32 = Color32::from_rgb(100, 100, 100);
pub const DEFAULT_PANEL_WIDTH: f32 = 760.0;
pub const DEFAULT_PANEL_HEIGHT: f32 = 460.0;
pub const LEGACY_DEFAULT_PANEL_WIDTH: f32 = 1904.0;
pub const LEGACY_DEFAULT_PANEL_HEIGHT: f32 = 720.0;
pub const PREVIOUS_DEFAULT_PANEL_WIDTH: f32 = 980.0;
pub const PREVIOUS_DEFAULT_PANEL_HEIGHT: f32 = 620.0;
pub const CURRENT_OLD_PANEL_WIDTH: f32 = 900.0;
pub const CURRENT_OLD_PANEL_HEIGHT: f32 = 520.0;
pub const PANEL_GAP: f32 = 30.0;
pub const CANVAS_BG: Color32 = Color32::from_rgb(10, 10, 10);
pub const SNAP_GUIDE_COLOR: Color32 = Color32::from_rgba_premultiplied(100, 160, 255, 150);

pub fn normalize_panel_size(size: Vec2) -> Vec2 {
    let is_legacy_default = (size.x - LEGACY_DEFAULT_PANEL_WIDTH).abs() < 0.1
        && (size.y - LEGACY_DEFAULT_PANEL_HEIGHT).abs() < 0.1;
    let is_previous_default = (size.x - PREVIOUS_DEFAULT_PANEL_WIDTH).abs() < 0.1
        && (size.y - PREVIOUS_DEFAULT_PANEL_HEIGHT).abs() < 0.1;
    let is_current_old_default = (size.x - CURRENT_OLD_PANEL_WIDTH).abs() < 0.1
        && (size.y - CURRENT_OLD_PANEL_HEIGHT).abs() < 0.1;
    let is_oversized = size.x >= 1500.0 || size.y >= 900.0;

    if is_legacy_default || is_previous_default || is_current_old_default || is_oversized {
        vec2(DEFAULT_PANEL_WIDTH, DEFAULT_PANEL_HEIGHT)
    } else {
        size
    }
}

#[cfg(test)]
mod tests {
    use egui::vec2;

    use super::{normalize_panel_size, DEFAULT_PANEL_HEIGHT, DEFAULT_PANEL_WIDTH};

    #[test]
    fn migrates_legacy_default_panel_size() {
        let size = normalize_panel_size(vec2(1904.0, 720.0));
        assert_eq!(size, vec2(DEFAULT_PANEL_WIDTH, DEFAULT_PANEL_HEIGHT));
    }

    #[test]
    fn migrates_previous_default_panel_size() {
        let size = normalize_panel_size(vec2(980.0, 620.0));
        assert_eq!(size, vec2(DEFAULT_PANEL_WIDTH, DEFAULT_PANEL_HEIGHT));
    }

    #[test]
    fn migrates_latest_old_default_panel_size() {
        let size = normalize_panel_size(vec2(900.0, 520.0));
        assert_eq!(size, vec2(DEFAULT_PANEL_WIDTH, DEFAULT_PANEL_HEIGHT));
    }

    #[test]
    fn preserves_custom_panel_size() {
        let size = normalize_panel_size(vec2(1200.0, 800.0));
        assert_eq!(size, vec2(1200.0, 800.0));
    }
}

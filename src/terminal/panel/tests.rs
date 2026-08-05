use crate::app::gesture_pointer_pos;
use crate::canvas::viewport::Viewport;
use crate::collab::PanelShareScope;
use crate::runtime::RenderTier;
use crate::terminal::input::{mouse_scroll_sgr_sequence, ScrollAccumulator};
use crate::terminal::layout::{grid_point_from_position, GridMetrics};
use crate::terminal::scrollbar::{
    scrollbar_pointer_to_scrollback, scrollbar_thumb_height, terminal_scrollbar_rect,
};
use egui::{pos2, vec2, Color32};

use super::{
    branch_badge_rect, chrome_zoom, close_rect, infer_activity_label, minimize_rect,
    panel_corner_radius, panel_lod, panel_roundings, preview_label_text, render_tier_for_panel,
    resize_target_from_origin, shell_label, should_defer_terminal_resize, should_draw_resize_grip,
    should_draw_title_text, should_draw_window_controls, should_render_live_terminal,
    should_render_terminal_contents, terminal_mouse_cell_from_pointer, title_bar_height,
    title_drag_hit_rect, PanelHitArea, PanelLod, ResizeHandle, TerminalPanel, BORDER_RADIUS,
    MIN_HEIGHT, MIN_WIDTH, TITLE_BAR_HEIGHT,
};
#[test]
fn custom_title_survives_shell_title_updates() {
    let mut panel = TerminalPanel::new(pos2(0.0, 0.0), vec2(400.0, 300.0), Color32::WHITE, 0);
    panel.rename_title("Deploy".to_owned());
    panel.apply_shell_title("bash".to_owned());

    assert_eq!(panel.title, "Deploy");
}

#[test]
fn empty_custom_title_falls_back_to_shell_title() {
    let mut panel = TerminalPanel::new(pos2(0.0, 0.0), vec2(400.0, 300.0), Color32::WHITE, 0);
    panel.apply_shell_title("zsh".to_owned());
    panel.rename_title("   ".to_owned());

    assert_eq!(panel.title, "zsh");
}

#[test]
fn default_window_title_falls_back_to_shell_label_when_no_custom_or_runtime_title() {
    let mut panel = TerminalPanel::new(pos2(0.0, 0.0), vec2(400.0, 300.0), Color32::WHITE, 0);
    panel.cwd_label = "mauro".to_owned();
    panel.shell_label = shell_label();

    assert_eq!(panel.window_title(720.0), shell_label());
}

#[test]
fn close_button_is_on_left_like_macos() {
    let title_rect = egui::Rect::from_min_size(pos2(100.0, 50.0), vec2(500.0, 42.0));
    let close = close_rect(title_rect);

    assert!(close.center().x < title_rect.center().x);
    assert!((close.center().x - 126.0).abs() < 0.001);
}

#[test]
fn minimize_button_sits_to_right_of_close_button() {
    let title_rect = egui::Rect::from_min_size(pos2(100.0, 50.0), vec2(500.0, 42.0));
    let close = close_rect(title_rect);
    let minimize = minimize_rect(title_rect);

    assert!(minimize.center().x > close.center().x);
}

#[test]
fn resize_target_uses_original_rect_instead_of_accumulating() {
    let origin = egui::Rect::from_min_size(pos2(50.0, 60.0), vec2(400.0, 300.0));
    let after_small_drag =
        resize_target_from_origin(ResizeHandle::BottomRight, origin, vec2(10.0, 0.0), 1.0);
    let after_larger_drag =
        resize_target_from_origin(ResizeHandle::BottomRight, origin, vec2(15.0, 0.0), 1.0);

    assert_eq!(after_small_drag.size(), vec2(410.0, 300.0));
    assert_eq!(after_larger_drag.size(), vec2(415.0, 300.0));
}

#[test]
fn resize_respects_new_smaller_minimum_size() {
    let origin = egui::Rect::from_min_size(pos2(50.0, 60.0), vec2(320.0, 220.0));
    let resized =
        resize_target_from_origin(ResizeHandle::BottomRight, origin, vec2(-500.0, -500.0), 1.0);

    assert_eq!(resized.size(), vec2(MIN_WIDTH, MIN_HEIGHT));
}

#[test]
fn custom_title_takes_precedence_over_shell_label() {
    let mut panel = TerminalPanel::new(pos2(0.0, 0.0), vec2(400.0, 300.0), Color32::WHITE, 0);
    panel.rename_title("My terminal".to_owned());
    assert_eq!(panel.window_title(420.0), "My terminal");
}

#[test]
fn chrome_zoom_is_capped_at_normal_size() {
    assert_eq!(chrome_zoom(0.5), 0.5);
    assert_eq!(chrome_zoom(1.0), 1.0);
    assert_eq!(chrome_zoom(2.5), 1.0);
}

#[test]
fn title_bar_height_shrinks_when_zooming_out_but_not_when_zooming_in() {
    assert!((title_bar_height(0.5) - TITLE_BAR_HEIGHT * 0.5).abs() < 0.001);
    assert!((title_bar_height(1.0) - TITLE_BAR_HEIGHT).abs() < 0.001);
    assert!((title_bar_height(3.0) - TITLE_BAR_HEIGHT).abs() < 0.001);
}

#[test]
fn tiny_panels_hide_header_details_and_terminal_text() {
    let screen_rect = egui::Rect::from_min_size(pos2(0.0, 0.0), vec2(120.0, 70.0));
    let title_rect = egui::Rect::from_min_size(pos2(0.0, 0.0), vec2(120.0, 12.0));
    let content_rect = egui::Rect::from_min_size(pos2(0.0, 12.0), vec2(120.0, 58.0));

    assert!(matches!(
        panel_lod(screen_rect, title_rect),
        PanelLod::Compact
    ));
    assert!(should_draw_window_controls(screen_rect, title_rect));
    assert!(!should_draw_title_text(screen_rect, title_rect));
    assert!(!should_draw_resize_grip(screen_rect));
    assert!(should_render_terminal_contents(content_rect, 0.24));
    assert!(!should_render_terminal_contents(content_rect, 0.22));
    assert!(should_render_terminal_contents(content_rect, 0.3));
    assert!(!should_render_terminal_contents(content_rect, 0.05));
}

#[test]
fn large_panels_keep_full_ui_details() {
    let screen_rect = egui::Rect::from_min_size(pos2(0.0, 0.0), vec2(420.0, 260.0));
    let title_rect = egui::Rect::from_min_size(pos2(0.0, 0.0), vec2(420.0, 42.0));
    let content_rect = egui::Rect::from_min_size(pos2(0.0, 42.0), vec2(420.0, 218.0));

    assert!(should_draw_window_controls(screen_rect, title_rect));
    assert!(should_draw_title_text(screen_rect, title_rect));
    assert!(should_draw_resize_grip(screen_rect));
    assert!(should_render_terminal_contents(content_rect, 1.0));
}

#[test]
fn microscopic_panels_switch_to_minimal_lod_and_reduce_corner_radius() {
    let screen_rect = egui::Rect::from_min_size(pos2(0.0, 0.0), vec2(80.0, 52.0));
    let title_rect = egui::Rect::from_min_size(pos2(0.0, 0.0), vec2(80.0, 7.0));

    assert!(matches!(
        panel_lod(screen_rect, title_rect),
        PanelLod::Minimal
    ));
    assert!(panel_corner_radius(screen_rect) <= BORDER_RADIUS);
}

#[test]
fn zoomed_out_panel_roundings_fit_visible_header_and_body() {
    let screen_rect = egui::Rect::from_min_size(pos2(0.0, 0.0), vec2(84.0, 34.0));
    let title_rect = egui::Rect::from_min_size(pos2(0.0, 0.0), vec2(84.0, 5.0));
    let body_rect = egui::Rect::from_min_max(pos2(0.0, 5.0), pos2(84.0, 34.0));

    let roundings = panel_roundings(screen_rect, title_rect, body_rect);

    assert!(roundings.panel.nw <= title_rect.height() * 0.5);
    assert!(roundings.panel.ne <= title_rect.height() * 0.5);
    assert!(roundings.panel.sw <= body_rect.height() * 0.5);
    assert!(roundings.panel.se <= body_rect.height() * 0.5);
    assert_eq!(roundings.title.nw, roundings.panel.nw);
    assert_eq!(roundings.body.sw, roundings.panel.sw);
}

#[test]
fn fast_path_keeps_background_panels_live() {
    let content_rect = egui::Rect::from_min_size(pos2(0.0, 42.0), vec2(420.0, 218.0));

    assert!(should_render_live_terminal(
        content_rect,
        1.0,
        PanelLod::Full,
        false
    ));
    assert!(should_render_live_terminal(
        content_rect,
        1.0,
        PanelLod::Full,
        true
    ));
}

#[test]
fn focused_panels_get_full_render_tier() {
    let content_rect = egui::Rect::from_min_size(pos2(0.0, 42.0), vec2(420.0, 218.0));

    assert_eq!(
        render_tier_for_panel(content_rect, 1.0, PanelLod::Full, false, true),
        RenderTier::Full
    );
}

#[test]
fn fast_path_still_keeps_focused_panels_live() {
    let content_rect = egui::Rect::from_min_size(pos2(0.0, 42.0), vec2(420.0, 218.0));

    assert_eq!(
        render_tier_for_panel(content_rect, 1.0, PanelLod::Full, true, true),
        RenderTier::Full
    );
}

#[test]
fn background_streaming_panels_render_every_row() {
    let content_rect = egui::Rect::from_min_size(pos2(0.0, 42.0), vec2(200.0, 120.0));

    // Sin gesto activo, un panel visible renderiza completo aunque no
    // tenga foco: saltear filas se ve como texto roto.
    assert_eq!(
        render_tier_for_panel(content_rect, 1.0, PanelLod::Compact, false, false),
        RenderTier::Full
    );
}

#[test]
fn background_idle_panels_keep_full_cached_render_when_visible() {
    let content_rect = egui::Rect::from_min_size(pos2(0.0, 42.0), vec2(200.0, 120.0));

    assert_eq!(
        render_tier_for_panel(content_rect, 1.0, PanelLod::Compact, false, false),
        RenderTier::Full
    );
}

#[test]
fn drag_fast_path_uses_reduced_live_without_falling_to_preview() {
    let content_rect = egui::Rect::from_min_size(pos2(0.0, 42.0), vec2(200.0, 120.0));

    assert_eq!(
        render_tier_for_panel(content_rect, 1.0, PanelLod::Compact, true, false),
        RenderTier::ReducedLive
    );
}

#[test]
fn minimal_panels_keep_preview_badge_visible() {
    let content_rect = egui::Rect::from_min_size(pos2(0.0, 7.0), vec2(84.0, 44.0));

    assert_eq!(
        render_tier_for_panel(content_rect, 0.18, PanelLod::Minimal, false, false),
        RenderTier::Preview
    );
}

#[test]
fn fast_path_only_defers_resize_during_active_resize_gesture() {
    let rect = egui::Rect::from_min_size(pos2(0.0, 0.0), vec2(320.0, 220.0));

    assert!(should_defer_terminal_resize(true, Some(rect)));
    assert!(!should_defer_terminal_resize(true, None));
    assert!(!should_defer_terminal_resize(false, Some(rect)));
}

#[test]
fn intelligent_preview_detects_claude_code_from_prompt_command() {
    let label = infer_activity_label("Terminal", "Terminal", "(base) mauro@Mac ~ % claude");

    assert_eq!(label.as_deref(), Some("Claude Code"));
}

#[test]
fn intelligent_preview_prefers_openclaude_over_generic_claude() {
    let label = infer_activity_label("Claude", "Terminal", "running openclaude in this panel");

    assert_eq!(label.as_deref(), Some("OpenClaude"));
}

#[test]
fn preview_label_falls_back_to_clean_title_when_no_agent_is_detected() {
    let label = preview_label_text(None, "Deploy API");

    assert_eq!(label, "Deploy API");
}

#[test]
fn mac_native_upward_input_scroll_moves_toward_recent_output() {
    let mut accumulator = ScrollAccumulator::default();
    #[cfg(target_os = "macos")]
    assert_eq!(accumulator.take_lines(-48.0), -2);
    #[cfg(not(target_os = "macos"))]
    assert_eq!(accumulator.take_lines(-48.0), 2);
}

#[test]
fn mac_native_downward_input_scroll_moves_toward_history() {
    let mut accumulator = ScrollAccumulator::default();
    #[cfg(target_os = "macos")]
    assert_eq!(accumulator.take_lines(48.0), 2);
    #[cfg(not(target_os = "macos"))]
    assert_eq!(accumulator.take_lines(48.0), -2);
}

#[test]
fn mouse_mode_scroll_reports_pointer_cell_instead_of_fixed_origin() {
    let content_rect = egui::Rect::from_min_size(pos2(10.0, 20.0), vec2(400.0, 240.0));
    let pointer = pos2(10.0 + 7.2 * 5.4, 20.0 + 14.4 * 3.2);

    let (column, row) = terminal_mouse_cell_from_pointer(content_rect, pointer, 1.0).unwrap();
    let seq = mouse_scroll_sgr_sequence(64, column, row);

    assert_eq!((column, row), (3, 2));
    assert_eq!(seq, b"\x1b[<64;4;3M".to_vec());
}

#[test]
fn grid_point_from_position_clamps_to_visible_terminal_bounds() {
    let rect = egui::Rect::from_min_size(pos2(100.0, 80.0), vec2(80.0, 48.0));
    let metrics = GridMetrics {
        char_width: 8.0,
        line_height: 16.0,
    };

    let point = grid_point_from_position(rect, pos2(179.0, 127.0), &metrics, 3, 10).expect("point");

    assert_eq!(point.line, 2);
    assert_eq!(point.column, 9);
}

#[test]
fn tiny_title_bar_keeps_a_real_drag_hit_area() {
    let screen_rect = egui::Rect::from_min_size(pos2(0.0, 0.0), vec2(90.0, 52.0));
    let title_rect = egui::Rect::from_min_size(pos2(0.0, 0.0), vec2(90.0, 4.0));
    let hit_rect = title_drag_hit_rect(screen_rect, title_rect);

    assert!(hit_rect.height() >= 16.0);
    assert!(hit_rect.width() > 20.0);
}

#[test]
fn gesture_pointer_uses_latest_pointer_position_when_available() {
    let pointer = gesture_pointer_pos(
        Some(pos2(120.0, 80.0)),
        Some(pos2(100.0, 70.0)),
        Some(pos2(90.0, 60.0)),
    );

    assert_eq!(pointer, Some(pos2(120.0, 80.0)));
}

#[test]
fn compact_panels_drag_from_the_body_instead_of_terminal_selection() {
    let panel = TerminalPanel::new(pos2(0.0, 0.0), vec2(400.0, 300.0), Color32::WHITE, 0);
    let viewport = Viewport {
        pan: egui::Vec2::ZERO,
        zoom: 0.2,
    };
    let canvas_rect = egui::Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 600.0));
    let hit = panel.hit_test(pos2(40.0, 34.0), &viewport, canvas_rect);

    assert!(matches!(hit, Some(PanelHitArea::TitleBar)));
}

#[test]
fn hit_test_detects_minimize_button() {
    let panel = TerminalPanel::new(pos2(0.0, 0.0), vec2(400.0, 300.0), Color32::WHITE, 0);
    let viewport = Viewport::default();
    let canvas_rect = egui::Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 600.0));
    let title_rect = egui::Rect::from_min_size(pos2(0.0, 0.0), vec2(400.0, 42.0));
    let hit = panel.hit_test(minimize_rect(title_rect).center(), &viewport, canvas_rect);

    assert_eq!(hit, Some(PanelHitArea::MinimizeButton));
}

#[test]
fn resize_hit_areas_are_slightly_more_generous() {
    let screen_rect = egui::Rect::from_min_size(pos2(0.0, 0.0), vec2(420.0, 260.0));

    assert!(ResizeHandle::Right.hit_rect(screen_rect).width() >= 12.0);
    assert!(ResizeHandle::Bottom.hit_rect(screen_rect).height() >= 12.0);
    assert!(ResizeHandle::BottomRight.hit_rect(screen_rect).width() >= 28.0);
    assert!(ResizeHandle::BottomRight.hit_rect(screen_rect).height() >= 28.0);
}

#[test]
fn scrollbar_thumb_height_stays_within_track_bounds() {
    assert!((scrollbar_thumb_height(12.0, 50, 0) - 12.0).abs() <= f32::EPSILON);

    let thumb_height = scrollbar_thumb_height(120.0, 24, 240);
    assert!(thumb_height >= 18.0);
    assert!(thumb_height <= 120.0);
}

#[test]
fn scrollbar_pointer_maps_to_expected_scrollback_extremes() {
    let track_rect = egui::Rect::from_min_size(pos2(10.0, 20.0), vec2(12.0, 100.0));
    let thumb_height = 20.0;

    assert_eq!(
        scrollbar_pointer_to_scrollback(
            pos2(16.0, track_rect.max.y),
            track_rect,
            thumb_height,
            200
        ),
        0
    );
    assert_eq!(
        scrollbar_pointer_to_scrollback(
            pos2(16.0, track_rect.min.y),
            track_rect,
            thumb_height,
            200
        ),
        200
    );
}

#[test]
fn scroll_hit_target_includes_scrollbar_track() {
    let panel = TerminalPanel::new(pos2(0.0, 0.0), vec2(400.0, 300.0), Color32::WHITE, 0);
    let viewport = Viewport::default();
    let canvas_rect = egui::Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 600.0));
    let body_rect = egui::Rect::from_min_max(pos2(0.0, 42.0), pos2(400.0, 300.0));
    let pointer = terminal_scrollbar_rect(body_rect).center();

    assert!(panel.scroll_hit_test(pointer, &viewport, canvas_rect));
}

#[test]
fn shared_snapshot_reports_private_scope() {
    let mut panel = TerminalPanel::new(pos2(0.0, 0.0), vec2(400.0, 300.0), Color32::WHITE, 0);
    panel.set_share_scope(PanelShareScope::Private);

    let snapshot = panel.shared_snapshot();

    assert_eq!(snapshot.share_scope, PanelShareScope::Private);
    assert!(snapshot.visible_text.is_empty());
    assert!(snapshot.history_text.is_empty());
}

fn title_bar(width: f32) -> egui::Rect {
    egui::Rect::from_min_size(pos2(100.0, 50.0), vec2(width, 28.0))
}

#[test]
fn branch_badge_sits_against_the_right_edge_of_the_title_bar() {
    let bar = title_bar(400.0);
    let badge = branch_badge_rect(bar, 150.0, vec2(90.0, 16.0)).expect("badge fits");
    assert!(
        badge.right() < bar.right(),
        "badge must keep a right margin"
    );
    assert!(
        badge.right() > bar.right() - 20.0,
        "badge hugs the right edge"
    );
    assert_eq!(badge.width(), 90.0);
    assert_eq!(badge.height(), 16.0);
}

#[test]
fn branch_badge_is_vertically_centred_in_the_title_bar() {
    let bar = title_bar(400.0);
    let badge = branch_badge_rect(bar, 100.0, vec2(80.0, 16.0)).expect("badge fits");
    assert!((badge.center().y - bar.center().y).abs() < 0.01);
}

#[test]
fn branch_badge_is_dropped_when_it_would_touch_the_title_text() {
    let bar = title_bar(400.0);
    // Un título que llega casi al borde derecho no deja lugar al badge.
    assert!(branch_badge_rect(bar, bar.right() - 40.0, vec2(90.0, 16.0)).is_none());
}

#[test]
fn branch_badge_is_dropped_on_narrow_panels() {
    let narrow = title_bar(80.0);
    assert!(branch_badge_rect(narrow, 60.0, vec2(90.0, 16.0)).is_none());
}

#[test]
fn branch_badge_is_dropped_when_taller_than_the_title_bar() {
    let bar = title_bar(400.0);
    assert!(branch_badge_rect(bar, 100.0, vec2(90.0, 40.0)).is_none());
}

#[test]
fn branch_badge_is_dropped_for_degenerate_sizes() {
    let bar = title_bar(400.0);
    assert!(branch_badge_rect(bar, 100.0, vec2(0.0, 16.0)).is_none());
    assert!(branch_badge_rect(bar, 100.0, vec2(90.0, 0.0)).is_none());
}

#[test]
fn branch_badge_never_overlaps_the_title_for_any_title_width() {
    let bar = title_bar(500.0);
    let size = vec2(70.0, 16.0);
    let mut drawn = 0usize;
    for step in 0..100 {
        let title_right = bar.left() + step as f32 * 5.0;
        if let Some(badge) = branch_badge_rect(bar, title_right, size) {
            assert!(
                badge.left() > title_right,
                "badge at {badge:?} overlaps title ending at {title_right}"
            );
            assert!(badge.right() <= bar.right());
            drawn += 1;
        }
    }
    assert!(drawn > 0, "the badge should fit for at least some widths");
}

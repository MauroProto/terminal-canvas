use std::collections::HashSet;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use egui::{
    vec2, Align2, Area, CentralPanel, Color32, FontId, Id, Key, Order, Pos2, Rect, SidePanel,
};
use uuid::Uuid;

use crate::canvas::config::{CANVAS_BG, SNAP_GUIDE_COLOR, ZOOM_KEYBOARD_FACTOR};
use crate::canvas::grid::draw_grid;
use crate::canvas::minimap;
use crate::canvas::scene::handle_canvas_input;
use crate::canvas::snap::guide_endpoints;
use crate::canvas::viewport::Viewport;
use crate::command_palette::commands::Command;
use crate::command_palette::CommandPalette;
use crate::shortcuts::shortcut_command;
use crate::sidebar::{Sidebar, SidebarResponse};
use crate::state::persistence::{AutosaveController, AutosaveDecision};
use crate::state::{load_state, save_state, AppState, Workspace};
use crate::terminal::panel::{PanelHitArea, ResizeHandle};
use crate::theme::fonts::setup_fonts;
use crate::update::{RepaintPolicy, UpdateChecker};
use crate::utils::platform::home_dir;

const AUTOSAVE_INTERVAL: Duration = Duration::from_secs(2);
const RUNTIME_REPAINT_BATCH: Duration = Duration::from_millis(33);
const VIEWPORT_FOCUS_PADDING: f32 = 72.0;
const VIEWPORT_FOCUS_MAX_ZOOM: f32 = 2.0;
const VIEWPORT_OVERVIEW_PADDING: f32 = 84.0;
const VIEWPORT_OVERVIEW_MAX_ZOOM: f32 = 1.0;
const VIEWPORT_FOCUS_ANIMATION_SECS: f64 = 0.36;

#[derive(Clone, Copy)]
enum PanelGestureKind {
    Drag { origin: Pos2 },
    Resize { handle: ResizeHandle, origin: Rect },
}

#[derive(Clone, Copy)]
struct PanelGesture {
    panel_id: Uuid,
    pointer_origin: Pos2,
    kind: PanelGestureKind,
}

#[derive(Clone, Copy)]
struct ViewportAnimation {
    start: Viewport,
    target: Viewport,
    started_at: f64,
    duration: f64,
}

pub struct TerminalApp {
    workspaces: Vec<Workspace>,
    active_ws: usize,
    viewport: Viewport,
    sidebar_visible: bool,
    show_grid: bool,
    show_minimap: bool,
    ctx: Option<egui::Context>,
    command_palette: CommandPalette,
    renaming_panel: Option<Uuid>,
    rename_buf: String,
    brand_texture: Option<egui::TextureHandle>,
    sidebar: Sidebar,
    update_checker: UpdateChecker,
    fullscreen: bool,
    panel_gesture: Option<PanelGesture>,
    autosave: AutosaveController,
    persisted_state: Option<AppState>,
    viewport_animation: Option<ViewportAnimation>,
    repaint_policy: RepaintPolicy,
}

impl TerminalApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        setup_fonts(cc);
        let brand_texture = load_brand_texture(cc);
        let update_checker = UpdateChecker::new(&cc.egui_ctx);
        let loaded_state = load_state();
        let has_saved_state = loaded_state.is_some();

        let mut app = if let Some(saved) = loaded_state {
            let mut workspaces = Vec::new();
            for workspace in saved.workspaces {
                workspaces.push(Workspace::from_saved(workspace, &cc.egui_ctx));
            }
            let active_ws = saved.active_ws.min(workspaces.len().saturating_sub(1));
            let viewport = workspaces
                .get(active_ws)
                .map(|workspace| Viewport {
                    pan: workspace.viewport_pan,
                    zoom: workspace.viewport_zoom,
                })
                .unwrap_or_default();
            Self {
                workspaces,
                active_ws,
                viewport,
                sidebar_visible: saved.sidebar_visible,
                show_grid: saved.show_grid,
                show_minimap: saved.show_minimap,
                ctx: Some(cc.egui_ctx.clone()),
                command_palette: CommandPalette::default(),
                renaming_panel: None,
                rename_buf: String::new(),
                brand_texture,
                sidebar: Sidebar::default(),
                update_checker,
                fullscreen: false,
                panel_gesture: None,
                autosave: AutosaveController::new(AUTOSAVE_INTERVAL),
                persisted_state: None,
                viewport_animation: None,
                repaint_policy: RepaintPolicy::new(RUNTIME_REPAINT_BATCH),
            }
        } else {
            let mut workspace = Workspace::new("Default", None);
            workspace.spawn_terminal(&cc.egui_ctx);
            Self {
                workspaces: vec![workspace],
                active_ws: 0,
                viewport: Viewport::default(),
                sidebar_visible: true,
                show_grid: true,
                show_minimap: true,
                ctx: Some(cc.egui_ctx.clone()),
                command_palette: CommandPalette::default(),
                renaming_panel: None,
                rename_buf: String::new(),
                brand_texture,
                sidebar: Sidebar::default(),
                update_checker,
                fullscreen: false,
                panel_gesture: None,
                autosave: AutosaveController::new(AUTOSAVE_INTERVAL),
                persisted_state: None,
                viewport_animation: None,
                repaint_policy: RepaintPolicy::new(RUNTIME_REPAINT_BATCH),
            }
        };

        if let Some(workspace) = app.workspaces.get(app.active_ws) {
            app.viewport.pan = workspace.viewport_pan;
            app.viewport.zoom = workspace.viewport_zoom.max(0.125);
        }

        if has_saved_state {
            app.persisted_state = Some(app.snapshot_state());
        }

        app
    }

    fn ws(&self) -> &Workspace {
        &self.workspaces[self.active_ws]
    }

    fn ws_mut(&mut self) -> &mut Workspace {
        &mut self.workspaces[self.active_ws]
    }

    fn snapshot_state(&self) -> AppState {
        let workspaces = self
            .workspaces
            .iter()
            .enumerate()
            .map(|(index, workspace)| {
                let mut saved = workspace.to_saved();
                if index == self.active_ws {
                    saved.viewport_pan = [self.viewport.pan.x, self.viewport.pan.y];
                    saved.viewport_zoom = self.viewport.zoom;
                }
                saved
            })
            .collect();

        AppState {
            workspaces,
            active_ws: self.active_ws,
            sidebar_visible: self.sidebar_visible,
            show_grid: self.show_grid,
            show_minimap: self.show_minimap,
        }
    }

    fn handle_shortcuts(&mut self, ctx: &egui::Context) -> Option<Command> {
        for event in ctx.input(|i| i.events.clone()) {
            if let egui::Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } = event
            {
                if modifiers.ctrl && modifiers.shift && key == Key::P {
                    self.command_palette.toggle();
                    return None;
                }
                if let Some(command) = shortcut_command(&modifiers, key) {
                    return Some(command);
                }
            }
        }
        None
    }

    fn execute_command(&mut self, command: Command, ctx: &egui::Context, canvas_rect: Rect) {
        self.viewport_animation = None;
        match command {
            Command::NewTerminal => {
                self.ws_mut().spawn_terminal(ctx);
            }
            Command::OpenFolder => self.pick_workspace_folder(ctx),
            Command::CloseTerminal => {
                if let Some(panel_id) = self.ws().focused_panel().map(|panel| panel.id()) {
                    self.ws_mut().close_panel(panel_id);
                }
            }
            Command::RenameTerminal => {
                if let Some(panel) = self.ws().focused_panel() {
                    let panel_id = panel.id();
                    let panel_title = panel.title().to_owned();
                    self.renaming_panel = Some(panel_id);
                    self.rename_buf = panel_title;
                }
            }
            Command::FocusNext => self.focus_relative(1),
            Command::FocusPrev => self.focus_relative(-1),
            Command::ZoomToFitAll => self.zoom_to_fit_all(canvas_rect),
            Command::ToggleSidebar => self.sidebar_visible = !self.sidebar_visible,
            Command::ToggleMinimap => self.show_minimap = !self.show_minimap,
            Command::ToggleGrid => self.show_grid = !self.show_grid,
            Command::ZoomIn => {
                let center = canvas_rect.center();
                self.viewport
                    .zoom_around(center, canvas_rect, ZOOM_KEYBOARD_FACTOR);
            }
            Command::ZoomOut => {
                let center = canvas_rect.center();
                self.viewport
                    .zoom_around(center, canvas_rect, 1.0 / ZOOM_KEYBOARD_FACTOR);
            }
            Command::ResetZoom => {
                self.viewport.zoom = 1.0;
                self.viewport.pan = egui::Vec2::ZERO;
            }
            Command::ToggleFullscreen => {
                self.fullscreen = !self.fullscreen;
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(self.fullscreen));
            }
        }
    }

    fn focus_relative(&mut self, direction: isize) {
        if self.ws().panels.is_empty() {
            return;
        }
        let mut order: Vec<_> = self
            .ws()
            .panels
            .iter()
            .map(|panel| (panel.z_index(), panel.id()))
            .collect();
        order.sort_by_key(|(z, _)| *z);
        let current = self
            .ws()
            .focused_panel()
            .map(|panel| panel.id())
            .and_then(|id| order.iter().position(|(_, current)| *current == id))
            .unwrap_or(0);
        let next = (current as isize + direction).rem_euclid(order.len() as isize) as usize;
        self.ws_mut().bring_to_front(order[next].1);
    }

    fn zoom_to_fit_all(&mut self, canvas_rect: Rect) {
        if self.ws().panels.is_empty() {
            return;
        }
        let bounds = self
            .ws()
            .panels
            .iter()
            .map(|panel| panel.rect())
            .reduce(|a, b| a.union(b))
            .unwrap()
            .expand(50.0);

        let scale_x = canvas_rect.width() / bounds.width().max(1.0);
        let scale_y = canvas_rect.height() / bounds.height().max(1.0);
        self.viewport.zoom = scale_x.min(scale_y).clamp(0.125, 4.0);
        self.viewport.pan_to_center(bounds.center(), canvas_rect);
    }

    fn switch_workspace(&mut self, index: usize) {
        if index == self.active_ws || index >= self.workspaces.len() {
            return;
        }
        self.viewport_animation = None;
        self.workspaces[self.active_ws].viewport_pan = self.viewport.pan;
        self.workspaces[self.active_ws].viewport_zoom = self.viewport.zoom;
        self.active_ws = index;
        self.viewport.pan = self.workspaces[self.active_ws].viewport_pan;
        self.viewport.zoom = self.workspaces[self.active_ws].viewport_zoom;
    }

    fn pick_workspace_folder(&mut self, ctx: &egui::Context) {
        let start_dir = self
            .workspaces
            .get(self.active_ws)
            .and_then(|workspace| workspace.cwd().map(|path| path.to_path_buf()))
            .or_else(home_dir);
        let mut dialog = rfd::FileDialog::new();
        if let Some(start_dir) = start_dir {
            dialog = dialog.set_directory(start_dir);
        }

        if let Some(path) = dialog.pick_folder() {
            self.open_workspace_folder(ctx, path);
        }
    }

    fn open_workspace_folder(&mut self, ctx: &egui::Context, path: PathBuf) {
        let already_open = self
            .workspaces
            .iter()
            .any(|workspace| workspace.matches_cwd(&path));
        let index = upsert_workspace_for_folder(&mut self.workspaces, path);
        self.switch_workspace(index);
        if !already_open || self.ws().panels.is_empty() {
            self.ws_mut().spawn_terminal(ctx);
        }
    }

    fn handle_sidebar_responses(&mut self, responses: Vec<SidebarResponse>, ctx: &egui::Context) {
        for response in responses {
            match response {
                SidebarResponse::SwitchWorkspace(index) => self.switch_workspace(index),
                SidebarResponse::OpenFolder => self.pick_workspace_folder(ctx),
                SidebarResponse::DeleteWorkspace(index) => {
                    if self.workspaces.len() > 1 && index < self.workspaces.len() {
                        self.workspaces.remove(index);
                        self.active_ws =
                            self.active_ws.min(self.workspaces.len().saturating_sub(1));
                    }
                }
                SidebarResponse::FocusPanel(panel_id) => {
                    self.ws_mut().bring_to_front(panel_id);
                }
                SidebarResponse::SpawnTerminal(index) => {
                    if let Some(workspace) = self.workspaces.get_mut(index) {
                        workspace.spawn_terminal(ctx);
                    }
                }
                SidebarResponse::RenamePanel(panel_id) => {
                    self.renaming_panel = Some(panel_id);
                    if let Some(panel) =
                        self.ws().panels.iter().find(|panel| panel.id() == panel_id)
                    {
                        self.rename_buf = panel.title().to_owned();
                    }
                }
                SidebarResponse::ClosePanel(panel_id) => {
                    self.ws_mut().close_panel(panel_id);
                }
            }
        }
    }

    fn maybe_persist_state(&mut self, ctx: &egui::Context) {
        let snapshot = self.snapshot_state();
        let now = Instant::now();
        match self
            .autosave
            .should_persist(&snapshot, self.persisted_state.as_ref(), now)
        {
            AutosaveDecision::Idle => {}
            AutosaveDecision::ScheduleAfter(delay) => ctx.request_repaint_after(delay),
            AutosaveDecision::SaveNow => {
                match crate::state::persistence::try_save_state(&snapshot) {
                    Ok(()) => {
                        self.persisted_state = Some(snapshot);
                        self.autosave.mark_saved(now);
                    }
                    Err(err) => {
                        log::warn!("Autosave failed: {err}");
                        ctx.request_repaint_after(AUTOSAVE_INTERVAL);
                    }
                }
            }
        }
    }

    fn start_focus_animation(&mut self, panel_id: Uuid, canvas_rect: Rect, now: f64) {
        let Some(panel) = self.ws().panels.iter().find(|panel| panel.id() == panel_id) else {
            return;
        };
        let target = self.viewport.focus_on_rect(
            panel.rect(),
            canvas_rect,
            VIEWPORT_FOCUS_PADDING,
            VIEWPORT_FOCUS_MAX_ZOOM,
        );
        if target.pan == self.viewport.pan && (target.zoom - self.viewport.zoom).abs() < 0.001 {
            self.viewport_animation = None;
            return;
        }
        self.viewport_animation = Some(ViewportAnimation {
            start: self.viewport,
            target,
            started_at: now,
            duration: VIEWPORT_FOCUS_ANIMATION_SECS,
        });
    }

    fn start_overview_animation(&mut self, canvas_rect: Rect, now: f64) {
        let target = overview_viewport_for_panels(
            &self.ws().panels,
            canvas_rect,
            VIEWPORT_OVERVIEW_PADDING,
            VIEWPORT_OVERVIEW_MAX_ZOOM,
        );
        if target.pan == self.viewport.pan && (target.zoom - self.viewport.zoom).abs() < 0.001 {
            self.viewport_animation = None;
            return;
        }
        self.viewport_animation = Some(ViewportAnimation {
            start: self.viewport,
            target,
            started_at: now,
            duration: VIEWPORT_FOCUS_ANIMATION_SECS,
        });
    }

    fn update_viewport_animation(&mut self, ctx: &egui::Context) {
        let Some(animation) = self.viewport_animation else {
            return;
        };
        let now = ctx.input(|i| i.time);
        let progress = ((now - animation.started_at) / animation.duration).clamp(0.0, 1.0) as f32;
        let eased = ease_in_out_cubic(progress);
        self.viewport = interpolate_viewport(animation.start, animation.target, eased);
        if progress >= 1.0 {
            self.viewport_animation = None;
        } else {
            ctx.request_repaint();
        }
    }

    fn show_rename_dialog(&mut self, ctx: &egui::Context) {
        let Some(panel_id) = self.renaming_panel else {
            return;
        };
        Area::new(Id::new("rename-dialog"))
            .order(Order::Debug)
            .anchor(egui::Align2::CENTER_CENTER, vec2(0.0, 0.0))
            .show(ctx, |ui| {
                egui::Frame::default()
                    .fill(Color32::from_rgb(24, 24, 28))
                    .stroke(egui::Stroke::new(1.0, Color32::from_rgb(55, 55, 65)))
                    .rounding(10.0)
                    .inner_margin(egui::Margin::same(12.0))
                    .show(ui, |ui: &mut egui::Ui| {
                        ui.label("Rename terminal");
                        let response = ui.add_sized(
                            vec2(280.0, 28.0),
                            egui::TextEdit::singleline(&mut self.rename_buf),
                        );
                        let confirm = response.lost_focus()
                            && ui.input(|i: &egui::InputState| i.key_pressed(Key::Enter));
                        ui.horizontal(|ui: &mut egui::Ui| {
                            if ui.button("Cancel").clicked() {
                                self.renaming_panel = None;
                            }
                            if ui.button("Save").clicked() || confirm {
                                let title = self.rename_buf.clone();
                                self.ws_mut().rename_panel(panel_id, title);
                                self.renaming_panel = None;
                            }
                        });
                    });
            });
    }
}

impl eframe::App for TerminalApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.ctx = Some(ctx.clone());

        if let Some(command) = self.handle_shortcuts(ctx) {
            let canvas_rect = ctx.available_rect();
            self.execute_command(command, ctx, canvas_rect);
        }

        let runtime_updates = self.ws().drain_runtime_updates();
        if !runtime_updates.session_updates.is_empty() {
            let dirty_sessions = runtime_updates
                .session_updates
                .iter()
                .map(|update| update.session_id)
                .collect::<HashSet<_>>();
            let focused_dirty = self
                .ws()
                .focused_panel()
                .and_then(|panel| panel.runtime_session_id())
                .map(|session_id| dirty_sessions.contains(&session_id))
                .unwrap_or(false);
            if focused_dirty {
                self.repaint_policy.note_focused_runtime_event();
            } else {
                self.repaint_policy.note_runtime_event();
            }
            for panel in &mut self.ws_mut().panels {
                if panel
                    .runtime_session_id()
                    .map(|session_id| dirty_sessions.contains(&session_id))
                    .unwrap_or(false)
                {
                    panel.sync_title();
                }
            }
        }
        let runtime_repaint_now = self.repaint_policy.should_repaint_now();

        if !self.command_palette.open && self.renaming_panel.is_none() {
            if let Some(panel) = self.ws_mut().focused_panel_mut() {
                panel.handle_input(ctx);
            }
        }

        if self.sidebar_visible {
            SidePanel::left("sidebar")
                .resizable(false)
                .default_width(212.0)
                .show(ctx, |ui| {
                    let state = self.update_checker.snapshot();
                    let responses = self.sidebar.show(
                        ui,
                        self.brand_texture.as_ref(),
                        &self.workspaces,
                        self.active_ws,
                        &state,
                    );
                    self.handle_sidebar_responses(responses, ctx);
                });
        }

        CentralPanel::default().show(ctx, |ui| {
            let canvas_rect = ui.max_rect();
            ui.painter().rect_filled(canvas_rect, 0.0, CANVAS_BG);

            let (
                latest_pos,
                hover_pos,
                interact_pos,
                primary_pressed,
                primary_released,
                primary_clicked,
                primary_double_clicked,
                primary_down,
                smooth_scroll_delta,
                zoom_delta,
                modifiers,
            ) = ctx.input(|i| {
                (
                    i.pointer.latest_pos(),
                    i.pointer.hover_pos(),
                    i.pointer.interact_pos(),
                    i.pointer.primary_pressed(),
                    i.pointer.primary_released(),
                    i.pointer.primary_clicked(),
                    i.pointer
                        .button_double_clicked(egui::PointerButton::Primary),
                    i.pointer.primary_down(),
                    i.smooth_scroll_delta,
                    i.zoom_delta(),
                    i.modifiers,
                )
            });
            let pointer_pos = gesture_pointer_pos(latest_pos, interact_pos, hover_pos);
            let hovered_hit = pointer_pos
                .filter(|pos| canvas_rect.contains(*pos))
                .and_then(|pos| top_panel_hit(self.ws(), pos, &self.viewport, canvas_rect));
            let scroll_target = pointer_pos
                .filter(|pos| canvas_rect.contains(*pos))
                .and_then(|pos| top_panel_scroll_hit(self.ws(), pos, &self.viewport, canvas_rect));
            let panel_interacting = self.panel_gesture.is_some();
            let hovered_panel = hovered_hit.is_some();
            let scroll_capture_active = panel_scroll_capture_active(
                hovered_panel,
                smooth_scroll_delta,
                zoom_delta,
                modifiers,
            );

            if scroll_capture_active {
                if let (Some(index), scroll_y) = (scroll_target, smooth_scroll_delta.y) {
                    if scroll_y != 0.0 {
                        let panel = &mut self.ws_mut().panels[index];
                        panel.handle_scroll(scroll_y, ctx);
                    }
                }
            }

            let canvas_input = handle_canvas_input(
                ui,
                &mut self.viewport,
                canvas_rect,
                hovered_panel || panel_interacting,
                scroll_capture_active,
            );
            let mut fast_path_render = canvas_input.navigating || self.panel_gesture.is_some();
            let mut needs_interaction_repaint = canvas_input.navigating || scroll_capture_active;

            if primary_pressed || canvas_input.navigating {
                self.viewport_animation = None;
            }

            let mut guides = Vec::new();
            if primary_pressed {
                match hovered_hit {
                    Some(hit) => {
                        let panel_id = self.ws().panels[hit.index].id();
                        let already_focused =
                            self.ws().focused_panel().map(|panel| panel.id()) == Some(panel_id);
                        if !already_focused {
                            self.ws_mut().bring_to_front(panel_id);
                        }
                        match hit.area {
                            PanelHitArea::TitleBar => {
                                let origin = self.ws().panels[hit.index].position();
                                self.panel_gesture = Some(PanelGesture {
                                    panel_id,
                                    pointer_origin: hit.pointer,
                                    kind: PanelGestureKind::Drag { origin },
                                });
                                if let Some(panel) = self
                                    .ws_mut()
                                    .panels
                                    .iter_mut()
                                    .find(|panel| panel.id() == panel_id)
                                {
                                    panel.set_drag_virtual_pos(Some(origin));
                                }
                            }
                            PanelHitArea::Resize(handle) => {
                                let origin = self.ws().panels[hit.index].rect();
                                self.panel_gesture = Some(PanelGesture {
                                    panel_id,
                                    pointer_origin: hit.pointer,
                                    kind: PanelGestureKind::Resize { handle, origin },
                                });
                                if let Some(panel) = self
                                    .ws_mut()
                                    .panels
                                    .iter_mut()
                                    .find(|panel| panel.id() == panel_id)
                                {
                                    panel.set_resize_virtual_rect(Some(origin));
                                }
                            }
                            PanelHitArea::Body | PanelHitArea::CloseButton => {
                                self.panel_gesture = None;
                            }
                        }
                    }
                    None => {
                        self.panel_gesture = None;
                        self.ws_mut().unfocus_all();
                    }
                }
            }

            if primary_down {
                if let (Some(gesture), Some(pointer)) = (self.panel_gesture, pointer_pos) {
                    let pointer_delta = pointer - gesture.pointer_origin;
                    let other_rects = self.ws().panel_rects_except(gesture.panel_id);
                    let zoom = self.viewport.zoom;
                    if let Some(panel) = self
                        .ws_mut()
                        .panels
                        .iter_mut()
                        .find(|panel| panel.id() == gesture.panel_id)
                    {
                        guides = match gesture.kind {
                            PanelGestureKind::Drag { origin } => {
                                panel.set_drag_virtual_pos(Some(origin));
                                panel.drag_to(origin, pointer_delta, zoom, &other_rects)
                            }
                            PanelGestureKind::Resize { handle, origin } => {
                                panel.set_resize_virtual_rect(Some(origin));
                                panel.resize_to(handle, origin, pointer_delta, zoom, &other_rects)
                            }
                        };
                    }
                }
            }
            fast_path_render |= self.panel_gesture.is_some();
            needs_interaction_repaint |= self.panel_gesture.is_some();

            if primary_released {
                if let Some(gesture) = self.panel_gesture.take() {
                    if let Some(panel) = self
                        .ws_mut()
                        .panels
                        .iter_mut()
                        .find(|panel| panel.id() == gesture.panel_id)
                    {
                        panel.set_drag_virtual_pos(None);
                        panel.set_resize_virtual_rect(None);
                    }
                }
            }

            if primary_clicked {
                if let Some(hit) = hovered_hit {
                    if matches!(hit.area, PanelHitArea::CloseButton) {
                        let panel_id = self.ws().panels[hit.index].id();
                        self.ws_mut().close_panel(panel_id);
                    }
                }
            }

            if primary_double_clicked {
                if let Some(hit) = hovered_hit {
                    if matches!(hit.area, PanelHitArea::Body | PanelHitArea::TitleBar) {
                        let panel_id = self.ws().panels[hit.index].id();
                        self.start_focus_animation(panel_id, canvas_rect, ctx.input(|i| i.time));
                    }
                }
            }

            self.update_viewport_animation(ctx);

            if self.show_grid {
                draw_grid(ui.painter(), &self.viewport, canvas_rect);
            }

            let mut panel_order: Vec<_> = (0..self.ws().panels.len()).collect();
            panel_order.sort_by_key(|index| self.ws().panels[*index].z_index());

            for index in panel_order {
                if !self
                    .viewport
                    .is_visible(self.ws().panels[index].rect(), canvas_rect)
                {
                    continue;
                }
                let viewport = self.viewport;
                let interaction = {
                    let panel = &mut self.ws_mut().panels[index];
                    panel.show(ui, &viewport, canvas_rect, fast_path_render)
                };
                guides.extend(interaction.guides);
            }

            for guide in guides {
                let [start, end] = guide_endpoints(guide);
                let start = self.viewport.canvas_to_screen(start, canvas_rect);
                let end = self.viewport.canvas_to_screen(end, canvas_rect);
                ui.painter()
                    .line_segment([start, end], egui::Stroke::new(1.0, SNAP_GUIDE_COLOR));
            }

            ui.painter().text(
                canvas_rect.left_bottom() + vec2(12.0, -10.0),
                Align2::LEFT_BOTTOM,
                format!(
                    "Zoom {:.0}% · Pan ({:.0}, {:.0}) · Panels {}",
                    self.viewport.zoom * 100.0,
                    self.viewport.pan.x,
                    self.viewport.pan.y,
                    self.ws().panel_count()
                ),
                FontId::proportional(11.0),
                Color32::from_rgb(115, 115, 115),
            );

            if self.show_minimap {
                let result = minimap::show(ui, &self.ws().panels, &self.viewport, canvas_rect);
                if result.hide_clicked {
                    self.show_minimap = false;
                }
                if result.focus_all_clicked {
                    self.start_overview_animation(canvas_rect, ctx.input(|i| i.time));
                }
                if let Some(target) = result.navigate_to {
                    self.viewport.pan_to_center(target, canvas_rect);
                }
            }

            if needs_interaction_repaint {
                ui.ctx().request_repaint();
            }
        });

        if let Some(command) = self.command_palette.show(ctx) {
            self.execute_command(command, ctx, ctx.available_rect());
        }

        self.show_rename_dialog(ctx);
        self.maybe_persist_state(ctx);

        if runtime_repaint_now {
            ctx.request_repaint();
        }

        if let Some(delay) = self.repaint_policy.next_repaint_delay(Instant::now()) {
            ctx.request_repaint_after(delay.max(Duration::from_millis(1)));
        } else if self
            .ws()
            .panels
            .iter()
            .any(|panel| panel.focused() && panel.is_alive())
        {
            ctx.request_repaint_after(Duration::from_millis(120));
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        save_state(&self.snapshot_state());
    }
}

fn load_brand_texture(cc: &eframe::CreationContext<'_>) -> Option<egui::TextureHandle> {
    let image = image::load_from_memory(include_bytes!("../assets/brand.png")).ok()?;
    let rgba = image.to_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    let pixels = rgba.into_raw();
    Some(cc.egui_ctx.load_texture(
        "brand",
        egui::ColorImage::from_rgba_unmultiplied(size, &pixels),
        Default::default(),
    ))
}

#[derive(Clone, Copy)]
struct PanelHit {
    index: usize,
    area: PanelHitArea,
    pointer: Pos2,
}

pub(crate) fn gesture_pointer_pos(
    latest_pos: Option<Pos2>,
    interact_pos: Option<Pos2>,
    hover_pos: Option<Pos2>,
) -> Option<Pos2> {
    latest_pos.or(interact_pos).or(hover_pos)
}

fn panel_scroll_capture_active(
    hovered_panel: bool,
    smooth_scroll_delta: egui::Vec2,
    zoom_delta: f32,
    modifiers: egui::Modifiers,
) -> bool {
    hovered_panel
        && smooth_scroll_delta != egui::Vec2::ZERO
        && !panel_zoom_gesture_active(smooth_scroll_delta, zoom_delta, modifiers)
}

fn panel_zoom_gesture_active(
    smooth_scroll_delta: egui::Vec2,
    zoom_delta: f32,
    modifiers: egui::Modifiers,
) -> bool {
    (zoom_delta - 1.0).abs() > f32::EPSILON
        || ((modifiers.ctrl || modifiers.command) && smooth_scroll_delta != egui::Vec2::ZERO)
}

fn upsert_workspace_for_folder(workspaces: &mut Vec<Workspace>, path: PathBuf) -> usize {
    if let Some(index) = workspaces
        .iter()
        .position(|workspace| workspace.matches_cwd(&path))
    {
        return index;
    }

    workspaces.push(Workspace::from_folder(path));
    workspaces.len() - 1
}

fn overview_viewport_for_panels(
    panels: &[crate::panel::CanvasPanel],
    screen_rect: Rect,
    padding: f32,
    max_zoom: f32,
) -> Viewport {
    let Some(bounds) = panels
        .iter()
        .map(|panel| panel.rect())
        .reduce(|a, b| a.union(b))
    else {
        return Viewport::default();
    };

    Viewport::fit_rect(bounds.expand(48.0), screen_rect, padding, max_zoom)
}

fn interpolate_viewport(start: Viewport, target: Viewport, progress: f32) -> Viewport {
    let progress = progress.clamp(0.0, 1.0);
    Viewport {
        pan: start.pan + (target.pan - start.pan) * progress,
        zoom: start.zoom + (target.zoom - start.zoom) * progress,
    }
}

fn ease_in_out_cubic(progress: f32) -> f32 {
    let progress = progress.clamp(0.0, 1.0);
    if progress < 0.5 {
        4.0 * progress * progress * progress
    } else {
        1.0 - (-2.0 * progress + 2.0).powi(3) * 0.5
    }
}

fn top_panel_hit(
    workspace: &Workspace,
    pointer: Pos2,
    viewport: &Viewport,
    canvas_rect: Rect,
) -> Option<PanelHit> {
    let mut panel_order: Vec<_> = (0..workspace.panels.len()).collect();
    panel_order.sort_by_key(|index| workspace.panels[*index].z_index());
    panel_order.reverse();

    panel_order.into_iter().find_map(|index| {
        workspace.panels[index]
            .hit_test(pointer, viewport, canvas_rect)
            .map(|area| PanelHit {
                index,
                area,
                pointer,
            })
    })
}

fn top_panel_scroll_hit(
    workspace: &Workspace,
    pointer: Pos2,
    viewport: &Viewport,
    canvas_rect: Rect,
) -> Option<usize> {
    let mut panel_order: Vec<_> = (0..workspace.panels.len()).collect();
    panel_order.sort_by_key(|index| workspace.panels[*index].z_index());
    panel_order.reverse();

    panel_order
        .into_iter()
        .find(|index| workspace.panels[*index].scroll_hit_test(pointer, viewport, canvas_rect))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use egui::{pos2, vec2, CentralPanel, Color32, Modifiers, RawInput, Rect};
    use uuid::Uuid;

    use super::{
        interpolate_viewport, overview_viewport_for_panels, panel_scroll_capture_active,
        top_panel_hit, top_panel_scroll_hit, upsert_workspace_for_folder,
    };
    use crate::canvas::config::{MINIMAP_BG, MINIMAP_HEIGHT, MINIMAP_PADDING, MINIMAP_WIDTH};
    use crate::canvas::minimap;
    use crate::canvas::viewport::Viewport;
    use crate::panel::CanvasPanel;
    use crate::state::Workspace;
    use crate::terminal::panel::{TerminalPanel, PANEL_BG};

    #[test]
    fn top_panel_hit_prefers_frontmost_panel() {
        let mut workspace = Workspace::new("Default", None);
        let mut back = TerminalPanel::new(pos2(0.0, 0.0), vec2(300.0, 200.0), Color32::WHITE, 0);
        back.z_index = 1;
        let mut front =
            TerminalPanel::new(pos2(20.0, 20.0), vec2(300.0, 200.0), Color32::LIGHT_BLUE, 1);
        front.z_index = 2;
        workspace.add_restored_terminal(back);
        workspace.add_restored_terminal(front);

        let hit = top_panel_hit(
            &workspace,
            pos2(50.0, 50.0),
            &Viewport::default(),
            Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 600.0)),
        )
        .unwrap();

        assert_eq!(hit.index, 1);
    }

    #[test]
    fn top_panel_scroll_hit_prefers_frontmost_terminal_body() {
        let mut workspace = Workspace::new("Default", None);
        let mut back = TerminalPanel::new(pos2(0.0, 0.0), vec2(300.0, 200.0), Color32::WHITE, 0);
        back.z_index = 1;
        let mut front =
            TerminalPanel::new(pos2(20.0, 20.0), vec2(300.0, 200.0), Color32::LIGHT_BLUE, 1);
        front.z_index = 2;
        workspace.add_restored_terminal(back);
        workspace.add_restored_terminal(front);

        let hit = top_panel_scroll_hit(
            &workspace,
            pos2(80.0, 120.0),
            &Viewport::default(),
            Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 600.0)),
        );

        assert_eq!(hit, Some(1));
    }

    #[test]
    fn zoom_scroll_over_panel_does_not_get_captured_as_terminal_scroll() {
        assert!(!panel_scroll_capture_active(
            true,
            vec2(0.0, 120.0),
            1.0,
            Modifiers {
                command: true,
                ..Default::default()
            },
        ));
    }

    #[test]
    fn plain_scroll_over_panel_stays_captured_by_terminal() {
        assert!(panel_scroll_capture_active(
            true,
            vec2(0.0, 120.0),
            1.0,
            Modifiers::default(),
        ));
    }

    #[test]
    fn minimap_paints_above_overlapping_panels() {
        let ctx = egui::Context::default();
        let raw_input = RawInput {
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(900.0, 700.0))),
            ..Default::default()
        };
        let viewport = Viewport::default();
        let panel_pos = pos2(560.0, 420.0);
        let panel_size = vec2(280.0, 220.0);
        let mut canvas_rect = Rect::NOTHING;

        let output = ctx.run(raw_input, |ctx| {
            CentralPanel::default().show(ctx, |ui| {
                canvas_rect = ui.max_rect();

                let mut drawn_panel =
                    TerminalPanel::new(panel_pos, panel_size, Color32::LIGHT_BLUE, 1);
                drawn_panel.show(ui, &viewport, canvas_rect, false);

                let minimap_panels = [CanvasPanel::Terminal(TerminalPanel::new(
                    panel_pos,
                    panel_size,
                    Color32::LIGHT_BLUE,
                    1,
                ))];
                minimap::show(ui, &minimap_panels, &viewport, canvas_rect);
            });
        });

        let panel_rect = Rect::from_min_size(
            canvas_rect.min + panel_pos.to_vec2(),
            panel_size * viewport.zoom,
        );
        let minimap_rect = Rect::from_min_size(
            pos2(
                canvas_rect.right() - MINIMAP_WIDTH - MINIMAP_PADDING,
                canvas_rect.bottom() - MINIMAP_HEIGHT - MINIMAP_PADDING,
            ),
            vec2(MINIMAP_WIDTH, MINIMAP_HEIGHT),
        );

        let panel_bg_idx = last_rect_shape_index(&output.shapes, panel_rect, PANEL_BG)
            .expect("expected panel background shape");
        let minimap_bg_idx = last_rect_shape_index(&output.shapes, minimap_rect, MINIMAP_BG)
            .expect("expected minimap background shape");

        assert!(
            minimap_bg_idx > panel_bg_idx,
            "minimap should paint after overlapping panels, got panel idx {panel_bg_idx} and minimap idx {minimap_bg_idx}"
        );
    }

    #[test]
    fn upsert_workspace_for_folder_reuses_existing_workspace() {
        let path = unique_temp_dir("existing-folder");
        let mut workspaces = vec![Workspace::from_folder(path.clone())];

        let index = upsert_workspace_for_folder(&mut workspaces, path);

        assert_eq!(index, 0);
        assert_eq!(workspaces.len(), 1);
    }

    #[test]
    fn upsert_workspace_for_folder_creates_workspace_for_new_folder() {
        let first = unique_temp_dir("first-folder");
        let second = unique_temp_dir("second-folder");
        let mut workspaces = vec![Workspace::from_folder(first)];

        let index = upsert_workspace_for_folder(&mut workspaces, second.clone());

        assert_eq!(index, 1);
        assert_eq!(workspaces.len(), 2);
        assert_eq!(workspaces[index].cwd(), Some(second.as_path()));
    }

    #[test]
    fn overview_viewport_contains_all_panels() {
        let panels = vec![
            CanvasPanel::Terminal(TerminalPanel::new(
                pos2(-320.0, -160.0),
                vec2(640.0, 420.0),
                Color32::WHITE,
                0,
            )),
            CanvasPanel::Terminal(TerminalPanel::new(
                pos2(980.0, 760.0),
                vec2(760.0, 460.0),
                Color32::LIGHT_BLUE,
                1,
            )),
        ];
        let screen = Rect::from_min_max(pos2(0.0, 0.0), pos2(1280.0, 820.0));

        let overview = overview_viewport_for_panels(&panels, screen, 84.0, 1.0);
        let visible = overview.visible_canvas_rect(screen);
        let bounds = panels
            .iter()
            .map(CanvasPanel::rect)
            .reduce(|a, b| a.union(b))
            .unwrap()
            .expand(48.0);

        assert!(visible.contains_rect(bounds));
        assert!(overview.zoom <= 1.0);
    }

    #[test]
    fn overview_viewport_defaults_when_no_panels_exist() {
        let screen = Rect::from_min_max(pos2(0.0, 0.0), pos2(1280.0, 820.0));

        let overview = overview_viewport_for_panels(&[], screen, 84.0, 1.0);

        assert_eq!(overview.pan, egui::Vec2::ZERO);
        assert_eq!(overview.zoom, 1.0);
    }

    #[test]
    fn interpolate_viewport_reaches_target_at_completion() {
        let start = Viewport {
            pan: vec2(-120.0, 40.0),
            zoom: 0.7,
        };
        let target = Viewport {
            pan: vec2(280.0, -160.0),
            zoom: 1.8,
        };

        let interpolated = interpolate_viewport(start, target, 1.0);

        assert_eq!(interpolated.pan, target.pan);
        assert_eq!(interpolated.zoom, target.zoom);
    }

    fn unique_temp_dir(name: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("{name}-{}", Uuid::new_v4()));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn last_rect_shape_index(
        shapes: &[egui::epaint::ClippedShape],
        expected_rect: Rect,
        expected_fill: Color32,
    ) -> Option<usize> {
        shapes
            .iter()
            .enumerate()
            .filter_map(|(index, clipped)| match &clipped.shape {
                egui::epaint::Shape::Rect(rect_shape)
                    if rect_shape.fill == expected_fill
                        && approx_rect(rect_shape.rect, expected_rect) =>
                {
                    Some(index)
                }
                _ => None,
            })
            .last()
    }

    fn approx_rect(a: Rect, b: Rect) -> bool {
        approx_eq(a.min.x, b.min.x)
            && approx_eq(a.min.y, b.min.y)
            && approx_eq(a.max.x, b.max.x)
            && approx_eq(a.max.y, b.max.y)
    }

    fn approx_eq(a: f32, b: f32) -> bool {
        (a - b).abs() <= 0.5
    }
}

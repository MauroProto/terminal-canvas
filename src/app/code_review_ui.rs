//! Visor de código/diffs estilo IDE: lista de archivos cambiados a la
//! izquierda y el diff unificado coloreado a la derecha. Se abre desde la
//! paleta (Review Changes) o del flujo de agentes; carga el diff en un worker.

use std::path::{Path, PathBuf};

use egui::{pos2, vec2, Align2, Color32, FontId, RichText, ScrollArea, Sense, Stroke};
use uuid::Uuid;

use crate::orchestration::{
    list_git_worktrees, remove_git_worktree, DiffLine, DiffLineKind, FileDiff, WorktreeInfo,
};
use crate::theme::colors as palette;

use super::TerminalApp;

const ADD_BG: Color32 = Color32::from_rgb(22, 44, 30);
const ADD_FG: Color32 = Color32::from_rgb(126, 222, 152);
const DEL_BG: Color32 = Color32::from_rgb(48, 24, 26);
const DEL_FG: Color32 = Color32::from_rgb(238, 130, 130);
const HUNK_FG: Color32 = Color32::from_rgb(108, 156, 220);
const GUTTER_FG: Color32 = Color32::from_rgb(92, 92, 92);
const MONO_SIZE: f32 = 12.5;
const LINE_HEIGHT: f32 = 18.0;
const GUTTER_W: f32 = 44.0;
const FILE_LIST_W: f32 = 240.0;

pub(super) struct CodeReviewState {
    pub(super) key: Uuid,
    pub(super) repo_root: PathBuf,
    pub(super) label: String,
    pub(super) loading: bool,
    pub(super) branch: String,
    pub(super) files: Vec<FileDiff>,
    pub(super) selected: usize,
    pub(super) failed: bool,
    /// Panel del agente asociado (para devolverle feedback).
    pub(super) target_panel: Option<Uuid>,
    pub(super) feedback: String,
    pub(super) feedback_sent: bool,
    /// Worktrees del repo (ciclo de vida: listar/limpiar).
    pub(super) worktrees: Vec<WorktreeInfo>,
    pub(super) show_worktrees: bool,
    pub(super) worktree_error: Option<String>,
}

impl TerminalApp {
    /// Resuelve el repo a revisar: primero el de la sesión de agente
    /// enfocada (worktree o repo), después el cwd del workspace activo.
    /// Devuelve (repo_root, etiqueta, panel_del_agente).
    fn code_review_target(&self) -> Option<(PathBuf, String, Option<Uuid>)> {
        if let Some(panel) = self.ws().focused_panel() {
            let panel_id = panel.id();
            if let Some(session) = self
                .orchestrator
                .sessions()
                .iter()
                .find(|session| session.panel_id == Some(panel_id))
            {
                if let Some(root) = session
                    .worktree_path
                    .clone()
                    .or_else(|| session.repo_root.clone())
                    .or_else(|| session.cwd.clone())
                {
                    let label = if session.label.trim().is_empty() {
                        session.provider.label().to_owned()
                    } else {
                        session.label.clone()
                    };
                    return Some((root, label, Some(panel_id)));
                }
            }
        }
        let workspace = self.ws();
        let cwd = workspace.cwd.clone()?;
        Some((cwd, workspace.name.clone(), None))
    }

    pub(super) fn open_code_review(&mut self) {
        let Some((repo_root, label, target_panel)) = self.code_review_target() else {
            return;
        };
        let key = Uuid::new_v4();
        self.code_review = Some(CodeReviewState {
            key,
            repo_root: repo_root.clone(),
            label,
            loading: true,
            branch: String::new(),
            files: Vec::new(),
            selected: 0,
            failed: false,
            target_panel,
            feedback: String::new(),
            feedback_sent: false,
            worktrees: Vec::new(),
            show_worktrees: false,
            worktree_error: None,
        });
        self.diff_loader.request(key, repo_root);
    }

    pub(super) fn poll_diff_loader(&mut self) {
        for result in self.diff_loader.poll() {
            let Some(state) = self.code_review.as_mut() else {
                continue;
            };
            if state.key != result.key {
                continue;
            }
            state.loading = false;
            match result.diff {
                Some(diff) => {
                    state.branch = diff.branch.clone();
                    state.files = diff.files;
                    state.selected = 0;
                    state.failed = false;
                    state.worktrees = list_git_worktrees(&state.repo_root);
                }
                None => {
                    state.failed = true;
                }
            }
        }
    }

    pub(super) fn show_code_review(&mut self, ctx: &egui::Context) {
        if self.code_review.is_none() {
            return;
        }
        // Escape cierra el visor.
        if ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
            self.code_review = None;
            return;
        }

        let mut close = false;
        let screen = ctx.screen_rect();
        egui::Area::new(egui::Id::new("code-review-backdrop"))
            .order(egui::Order::Middle)
            .fixed_pos(screen.min)
            .show(ctx, |ui| {
                ui.painter().rect_filled(
                    screen,
                    0.0,
                    Color32::from_rgba_premultiplied(0, 0, 0, 200),
                );
            });

        egui::Area::new(egui::Id::new("code-review"))
            .order(egui::Order::Foreground)
            .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
            .show(ctx, |ui| {
                let size = vec2(screen.width() * 0.92, screen.height() * 0.88);
                egui::Frame::default()
                    .fill(palette::INK)
                    .stroke(Stroke::new(1.0, palette::LINE))
                    .rounding(10.0)
                    .inner_margin(egui::Margin::same(0.0))
                    .show(ui, |ui| {
                        ui.set_min_size(size);
                        self.code_review_header(ui, &mut close);
                        ui.separator();
                        let has_agent = self
                            .code_review
                            .as_ref()
                            .is_some_and(|state| state.target_panel.is_some());
                        let show_worktrees = self
                            .code_review
                            .as_ref()
                            .is_some_and(|state| state.show_worktrees);
                        let mut reserved = 60.0;
                        if has_agent {
                            reserved += 58.0;
                        }
                        if show_worktrees {
                            reserved += 96.0;
                        }
                        let body_height = size.y - reserved;
                        if show_worktrees {
                            self.code_review_worktrees(ui);
                            ui.separator();
                        }
                        self.code_review_body(ui, size, body_height);
                        if has_agent {
                            ui.separator();
                            self.code_review_feedback(ui);
                        }
                    });
            });

        if close {
            self.code_review = None;
        }
    }

    fn code_review_header(&mut self, ui: &mut egui::Ui, close: &mut bool) {
        let (branch, additions, deletions, file_count, label, loading, failed, repo_root) = self
            .code_review
            .as_ref()
            .map(|state| {
                let additions: usize = state.files.iter().map(|f| f.additions).sum();
                let deletions: usize = state.files.iter().map(|f| f.deletions).sum();
                (
                    state.branch.clone(),
                    additions,
                    deletions,
                    state.files.len(),
                    state.label.clone(),
                    state.loading,
                    state.failed,
                    state.repo_root.display().to_string(),
                )
            })
            .unwrap_or_default();

        ui.horizontal(|ui| {
            ui.add_space(16.0);
            ui.label(
                RichText::new("Code Review")
                    .size(15.0)
                    .color(palette::TEXT_STRONG)
                    .strong(),
            );
            ui.add_space(8.0);
            if !label.is_empty() {
                ui.label(RichText::new(label).size(11.5).color(palette::DIM));
            }
            if !repo_root.is_empty() {
                ui.label(RichText::new(repo_root).size(10.5).color(palette::DIM));
            }
            if !branch.is_empty() {
                ui.add_space(8.0);
                branch_badge(ui, &branch);
            }
            if loading {
                ui.add_space(10.0);
                ui.label(
                    RichText::new("Cargando diff…")
                        .size(11.5)
                        .color(palette::DIM),
                );
            } else if failed {
                ui.add_space(10.0);
                ui.label(
                    RichText::new("No es un repositorio git o no hay cambios")
                        .size(11.5)
                        .color(DEL_FG),
                );
            } else {
                ui.add_space(10.0);
                ui.label(
                    RichText::new(format!("{file_count} archivos"))
                        .size(11.5)
                        .color(palette::TEXT),
                );
                ui.label(
                    RichText::new(format!("+{additions}"))
                        .size(11.5)
                        .color(ADD_FG),
                );
                ui.label(
                    RichText::new(format!("−{deletions}"))
                        .size(11.5)
                        .color(DEL_FG),
                );
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(10.0);
                if ui.small_button("Cerrar ✕").clicked() {
                    *close = true;
                }
                let (worktree_count, show_worktrees) = self
                    .code_review
                    .as_ref()
                    .map(|state| (state.worktrees.len(), state.show_worktrees))
                    .unwrap_or_default();
                if worktree_count > 1 {
                    let toggle_label = if show_worktrees {
                        "Ocultar worktrees".to_owned()
                    } else {
                        format!("Worktrees ({worktree_count})")
                    };
                    if ui.small_button(&toggle_label).clicked() {
                        if let Some(state) = self.code_review.as_mut() {
                            state.show_worktrees = !state.show_worktrees;
                        }
                    }
                }
            });
        });
        ui.add_space(8.0);
    }

    fn code_review_body(&mut self, ui: &mut egui::Ui, size: egui::Vec2, body_height: f32) {
        let body_height = body_height.max(120.0);
        let (loading, failed, file_count) = self
            .code_review
            .as_ref()
            .map(|state| (state.loading, state.failed, state.files.len()))
            .unwrap_or_default();

        if loading {
            ui.allocate_ui(vec2(size.x, body_height), |ui| {
                ui.add_space(40.0);
                ui.label(RichText::new("Cargando…").size(12.0).color(palette::DIM));
            });
            return;
        }
        if failed || file_count == 0 {
            ui.allocate_ui(vec2(size.x, body_height), |ui| {
                ui.add_space(40.0);
                let message = if failed {
                    "No se pudo leer el diff. ¿Es un repositorio git?"
                } else {
                    "Sin cambios. El árbol de trabajo está limpio."
                };
                ui.label(RichText::new(message).size(12.0).color(palette::DIM));
            });
            return;
        }

        ui.horizontal(|ui| {
            ui.set_height(body_height);
            self.code_review_file_list(ui, body_height);
            ui.separator();
            self.code_review_diff_view(ui, body_height);
        });
    }

    fn code_review_file_list(&mut self, ui: &mut egui::Ui, height: f32) {
        let selected = self
            .code_review
            .as_ref()
            .map(|state| state.selected)
            .unwrap_or(0);
        let items: Vec<(String, usize, usize, bool, bool)> = self
            .code_review
            .as_ref()
            .map(|state| {
                state
                    .files
                    .iter()
                    .map(|file| {
                        (
                            file.path.clone(),
                            file.additions,
                            file.deletions,
                            file.is_new,
                            file.is_deleted,
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();

        ScrollArea::vertical()
            .id_salt("code-review-file-list")
            .show(ui, |ui| {
                ui.set_min_width(FILE_LIST_W);
                ui.set_height(height);
                for (index, (path, additions, deletions, is_new, is_deleted)) in
                    items.iter().enumerate()
                {
                    let row = ui.horizontal(|ui| {
                        let response =
                            ui.allocate_exact_size(vec2(FILE_LIST_W - 16.0, 30.0), Sense::click());
                        let rect = response.0;
                        if index == selected {
                            ui.painter().rect_filled(rect, 4.0, palette::FOCUS);
                        } else if response.1.hovered() {
                            ui.painter().rect_filled(rect, 4.0, palette::HOVER);
                        }
                        let name = file_name(path);
                        let color = if index == selected {
                            palette::TEXT_STRONG
                        } else {
                            palette::TEXT
                        };
                        ui.painter().text(
                            pos2(rect.left() + 10.0, rect.center().y),
                            Align2::LEFT_CENTER,
                            name,
                            FontId::monospace(11.5),
                            color,
                        );
                        let badge = if *is_new {
                            "nuevo".to_owned()
                        } else if *is_deleted {
                            "borrado".to_owned()
                        } else {
                            format!("+{additions} −{deletions}")
                        };
                        let badge_color = if *is_new {
                            ADD_FG
                        } else if *is_deleted {
                            DEL_FG
                        } else {
                            palette::DIM
                        };
                        ui.painter().text(
                            pos2(rect.right() - 10.0, rect.center().y),
                            Align2::RIGHT_CENTER,
                            badge,
                            FontId::monospace(10.0),
                            badge_color,
                        );
                        response.1
                    });
                    if row.inner.clicked() {
                        if let Some(state) = self.code_review.as_mut() {
                            state.selected = index;
                        }
                    }
                }
            });
    }

    fn code_review_diff_view(&mut self, ui: &mut egui::Ui, height: f32) {
        let file: Option<FileDiff> = self.code_review.as_ref().and_then(|state| {
            state
                .files
                .get(state.selected.min(state.files.len().saturating_sub(1)))
                .cloned()
        });
        let Some(file) = file else {
            return;
        };

        ui.vertical(|ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.add_space(6.0);
                ui.label(
                    RichText::new(file.path.clone())
                        .size(11.5)
                        .color(palette::TEXT_STRONG)
                        .monospace(),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_space(8.0);
                    if ui.small_button("Abrir en editor").clicked() {
                        self.open_selected_file_in_editor();
                    }
                });
            });
            ui.separator();
            // Virtualizado: un diff de miles de líneas solo renderiza las
            // visibles (show_rows), no miles de widgets.
            ui.scope(|ui| {
                ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
                ScrollArea::vertical()
                    .id_salt("code-review-diff-view")
                    .max_height((height - 44.0).max(80.0))
                    .show_rows(ui, LINE_HEIGHT, file.lines.len(), |ui, range| {
                        for line in &file.lines[range] {
                            draw_diff_line(ui, line);
                        }
                    });
            });
        });
    }

    /// Abre el archivo seleccionado del code review con la app default del SO.
    fn open_selected_file_in_editor(&self) {
        let Some(state) = self.code_review.as_ref() else {
            return;
        };
        let Some(file) = state
            .files
            .get(state.selected.min(state.files.len().saturating_sub(1)))
        else {
            return;
        };
        let full = state.repo_root.join(&file.path);
        if let Err(err) = crate::utils::platform::open_path_external(&full) {
            log::warn!("No se pudo abrir {} en el editor: {err}", full.display());
        }
    }

    /// Footer para devolverle feedback al agente (estilo "annotate diffs"):
    /// un cuadro de texto que se inyecta en el terminal del agente.
    fn code_review_feedback(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            ui.vertical(|ui| {
                ui.add_space(6.0);
                let sent = self
                    .code_review
                    .as_ref()
                    .is_some_and(|state| state.feedback_sent);
                if sent {
                    ui.label(
                        RichText::new("Feedback enviado al agente ✓")
                            .size(11.0)
                            .color(ADD_FG),
                    );
                } else {
                    ui.label(
                        RichText::new("Feedback para el agente")
                            .size(11.0)
                            .color(palette::DIM),
                    );
                }
                let edit =
                    egui::TextEdit::multiline(&mut self.code_review.as_mut().unwrap().feedback)
                        .hint_text("Ej: \"cambiá el manejo de errores de esta función…\"")
                        .text_color(palette::TEXT_STRONG)
                        .desired_rows(2)
                        .margin(egui::Margin::symmetric(8.0, 5.0));
                ui.add_sized(vec2(ui.available_width() - 130.0, 44.0), edit);
            });
            ui.vertical(|ui| {
                ui.add_space(20.0);
                if ui.button("Enviar al agente").clicked() {
                    self.send_code_review_feedback();
                }
            });
            ui.add_space(12.0);
        });
        ui.add_space(6.0);
    }

    fn send_code_review_feedback(&mut self) {
        let Some((target_panel, feedback)) = self
            .code_review
            .as_ref()
            .map(|state| (state.target_panel, state.feedback.trim().to_owned()))
        else {
            return;
        };
        let Some(panel_id) = target_panel else {
            return;
        };
        if feedback.is_empty() {
            return;
        }
        let mut delivered = false;
        for workspace in &mut self.workspaces {
            if workspace.send_prompt_to_panel(panel_id, &feedback) {
                delivered = true;
                break;
            }
        }
        if delivered {
            if let Some(state) = self.code_review.as_mut() {
                state.feedback.clear();
                state.feedback_sent = true;
            }
        }
    }

    /// Sección de ciclo de vida de worktrees: lista los worktrees del repo y
    /// permite limpiar los gestionados (`.terminalcanvas/worktrees`).
    fn code_review_worktrees(&mut self, ui: &mut egui::Ui) {
        let (worktrees, error) = {
            let state = self.code_review.as_ref().unwrap();
            (state.worktrees.clone(), state.worktree_error.clone())
        };

        ui.horizontal(|ui| {
            ui.add_space(14.0);
            ui.label(
                RichText::new(format!("Worktrees ({})", worktrees.len()))
                    .size(12.0)
                    .color(palette::TEXT_STRONG)
                    .strong(),
            );
            if let Some(error) = error {
                ui.add_space(10.0);
                ui.label(RichText::new(error).size(10.5).color(DEL_FG));
            }
        });
        ui.add_space(4.0);

        let mut to_remove: Option<PathBuf> = None;
        ScrollArea::vertical()
            .id_salt("code-review-worktrees")
            .max_height(70.0)
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                for worktree in &worktrees {
                    ui.horizontal(|ui| {
                        ui.add_space(14.0);
                        let tag = if worktree.is_main { "main" } else { "linked" };
                        let tag_color = if worktree.is_main {
                            HUNK_FG
                        } else {
                            palette::DIM
                        };
                        ui.label(RichText::new(tag).size(10.0).color(tag_color));
                        ui.label(
                            RichText::new(worktree.path.display().to_string())
                                .size(10.5)
                                .color(palette::TEXT)
                                .monospace(),
                        );
                        if !worktree.branch.is_empty() {
                            ui.label(
                                RichText::new(format!("⎇ {}", worktree.branch))
                                    .size(10.0)
                                    .color(palette::DIM),
                            );
                        }
                        let is_managed = worktree.path.components().any(|component| {
                            matches!(component, std::path::Component::Normal(name) if name == ".terminalcanvas")
                        });
                        if !worktree.is_main && is_managed {
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                ui.add_space(14.0);
                                if ui.small_button("Limpiar").clicked() {
                                    to_remove = Some(worktree.path.clone());
                                }
                            });
                        }
                    });
                }
            });

        if let Some(path) = to_remove {
            self.remove_code_review_worktree(&path);
        }
        ui.add_space(4.0);
    }

    fn remove_code_review_worktree(&mut self, path: &Path) {
        let repo_root = {
            let Some(state) = self.code_review.as_ref() else {
                return;
            };
            state.repo_root.clone()
        };
        let result = remove_git_worktree(&repo_root, path);
        if let Some(state) = self.code_review.as_mut() {
            match result {
                Ok(()) => {
                    state.worktree_error = None;
                    state.worktrees = list_git_worktrees(&repo_root);
                }
                Err(err) => {
                    state.worktree_error = Some(err.to_string());
                }
            }
        }
    }
}

fn file_name(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_owned()
}

fn branch_badge(ui: &mut egui::Ui, branch: &str) {
    let font = FontId::monospace(10.5);
    let text_w = ui.fonts(|fonts| {
        fonts
            .layout_no_wrap(branch.to_owned(), font.clone(), palette::TEXT)
            .size()
            .x
    });
    let (rect, _) = ui.allocate_exact_size(vec2(text_w + 16.0, 18.0), Sense::hover());
    ui.painter()
        .rect_filled(rect, 9.0, Color32::from_rgb(30, 38, 50));
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        format!("⎇ {branch}"),
        font,
        HUNK_FG,
    );
}

fn draw_diff_line(ui: &mut egui::Ui, line: &DiffLine) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), LINE_HEIGHT), Sense::hover());
    let (bg, fg, marker) = match line.kind {
        DiffLineKind::Added => (Some(ADD_BG), ADD_FG, "+"),
        DiffLineKind::Removed => (Some(DEL_BG), DEL_FG, "-"),
        DiffLineKind::HunkHeader => (None, HUNK_FG, ""),
        DiffLineKind::Context => (None, palette::TEXT, " "),
    };
    if let Some(bg) = bg {
        ui.painter().rect_filled(rect, 0.0, bg);
    }
    let font = FontId::monospace(MONO_SIZE);
    let mono = MONO_SIZE * 0.6;

    // Gutters: número viejo, número nuevo, marcador.
    let old_x = rect.left() + 6.0;
    let new_x = old_x + GUTTER_W;
    let marker_x = new_x + GUTTER_W;
    let text_x = marker_x + 18.0;

    if let Some(old_ln) = line.old_ln {
        ui.painter().text(
            pos2(old_x + GUTTER_W - 8.0, rect.center().y),
            Align2::RIGHT_CENTER,
            old_ln.to_string(),
            font.clone(),
            GUTTER_FG,
        );
    }
    if let Some(new_ln) = line.new_ln {
        ui.painter().text(
            pos2(new_x + GUTTER_W - 8.0, rect.center().y),
            Align2::RIGHT_CENTER,
            new_ln.to_string(),
            font.clone(),
            GUTTER_FG,
        );
    }
    if !marker.is_empty() {
        ui.painter().text(
            pos2(marker_x, rect.center().y),
            Align2::LEFT_CENTER,
            marker,
            font.clone(),
            fg,
        );
    }

    if line.kind == DiffLineKind::HunkHeader {
        ui.painter().text(
            pos2(text_x, rect.center().y),
            Align2::LEFT_CENTER,
            &line.text,
            font,
            HUNK_FG,
        );
        return;
    }

    // Texto de la línea (truncado al ancho disponible).
    let max_chars = ((rect.right() - text_x) / mono).floor().max(0.0) as usize;
    let text: String = line.text.chars().take(max_chars).collect();
    ui.painter().text(
        pos2(text_x, rect.center().y),
        Align2::LEFT_CENTER,
        text,
        font,
        fg,
    );
}

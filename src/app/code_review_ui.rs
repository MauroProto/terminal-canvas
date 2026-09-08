//! Visor de código/diffs estilo IDE: lista de archivos cambiados a la
//! izquierda y el diff unificado coloreado a la derecha. Se abre desde la
//! paleta (Review Changes) o del flujo de agentes; carga el diff en un worker.

use std::path::{Path, PathBuf};

use egui::{pos2, vec2, Align2, Color32, FontId, RichText, ScrollArea, Sense, Stroke};
use uuid::Uuid;

use crate::orchestration::{DiffLine, DiffLineKind, FileDiff, WorktreeInfo, WorktreeJob};
use crate::theme::colors as palette;

use super::TerminalApp;

const ADD_BG: Color32 = Color32::from_rgb(22, 44, 30);
const ADD_FG: Color32 = Color32::from_rgb(126, 222, 152);
const DEL_BG: Color32 = Color32::from_rgb(48, 24, 26);
const DEL_FG: Color32 = Color32::from_rgb(238, 130, 130);
const HUNK_FG: Color32 = Color32::from_rgb(108, 156, 220);
const GUTTER_FG: Color32 = palette::DIM;
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
    /// Notas por línea sobre el diff, persistidas por repo.
    pub(super) notes: crate::orchestration::DiffNotes,
    /// Editor de nota activo: `note_id == None` = creando una nueva.
    pub(super) editing_note: Option<NoteEditState>,
    /// Selector de destinos abierto para mandar las notas pendientes.
    pub(super) note_target_picker: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct NoteEditState {
    pub(super) note_id: Option<Uuid>,
    pub(super) file_path: String,
    pub(super) line: u32,
    pub(super) body: String,
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
            notes: crate::orchestration::load_notes(&repo_root),
            editing_note: None,
            note_target_picker: false,
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
                    // Las notas que apuntan a archivos que ya no cambiaron se
                    // descartan: el feedback localizado pierde destino.
                    let current_files: Vec<String> =
                        state.files.iter().map(|file| file.path.clone()).collect();
                    let repo_root = state.repo_root.clone();
                    state.notes.prune_missing_files(&current_files);
                    let notes_snapshot = state.notes.clone();
                    let _ = crate::orchestration::save_notes(&repo_root, &notes_snapshot);
                    // Listar lanza un subproceso git: va al worker para no
                    // gastar milisegundos del frame.
                    let repo_root = state.repo_root.clone();
                    self.worktree_ops.request(WorktreeJob::List { repo_root });
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
        // Escape cancela primero lo modal (editor de nota, selector de
        // destinos); solo después cierra el visor.
        if ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
            let had_editor_or_picker = self
                .code_review
                .as_mut()
                .map(|state| {
                    if state.editing_note.is_some() {
                        state.editing_note = None;
                        true
                    } else if state.note_target_picker {
                        state.note_target_picker = false;
                        true
                    } else {
                        false
                    }
                })
                .unwrap_or(false);
            if !had_editor_or_picker {
                self.code_review = None;
            }
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
                    .corner_radius(10.0)
                    .inner_margin(egui::Margin::same(0))
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
                        let show_note_picker = self
                            .code_review
                            .as_ref()
                            .is_some_and(|state| state.note_target_picker);
                        let mut reserved = 60.0;
                        if has_agent {
                            reserved += 58.0;
                        }
                        if show_worktrees {
                            reserved += 96.0;
                        }
                        if show_note_picker {
                            reserved += 110.0;
                        }
                        let body_height = size.y - reserved;
                        if show_worktrees {
                            self.code_review_worktrees(ui);
                            ui.separator();
                        }
                        if show_note_picker {
                            self.note_target_picker_ui(ui);
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
            // PR asociado a la branch del review (P2.13, T4): sale del
            // snapshot de `gh` que ya trajo la pestaña Tasks.
            if let Some(number) = self
                .tasks_state
                .snapshot
                .pull_requests
                .iter()
                .find(|pr| !branch.is_empty() && pr.head_ref_name == branch)
                .map(|pr| pr.number)
            {
                ui.add_space(8.0);
                if ui
                    .small_button(RichText::new(format!("PR #{number}")).size(10.5))
                    .clicked()
                {
                    self.open_github_task(number);
                }
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
                    RichText::new("No se pudo cargar la revisión")
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
                // "Enviar N notas": las pendientes de entregar al agente.
                let pending_count = self
                    .code_review
                    .as_ref()
                    .map(|state| state.notes.pending().len())
                    .unwrap_or(0);
                if pending_count > 0 {
                    let send_label = format!(
                        "Enviar {pending_count} nota{}",
                        if pending_count == 1 { "" } else { "s" }
                    );
                    if ui.small_button(&send_label).clicked() {
                        self.begin_note_send();
                    }
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
        let (file, notes, editing): (
            Option<FileDiff>,
            Vec<crate::orchestration::DiffNote>,
            Option<NoteEditState>,
        ) = self
            .code_review
            .as_ref()
            .map(|state| {
                (
                    state
                        .files
                        .get(state.selected.min(state.files.len().saturating_sub(1)))
                        .cloned(),
                    state.notes.notes.clone(),
                    state.editing_note.clone(),
                )
            })
            .unwrap_or_default();
        let Some(file) = file else {
            return;
        };

        // Plan de filas: cada línea del diff puede ir seguida de sus notas y,
        // si se está creando/editando una, de la fila del editor.
        let (rows, offsets, total_height) = build_review_rows(&file, &notes, editing.as_ref());

        let mut actions: Vec<NoteAction> = Vec::new();

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
                let file_note_count = notes
                    .iter()
                    .filter(|note| note.file_path == file.path)
                    .count();
                if file_note_count > 0 {
                    ui.label(
                        RichText::new(format!("{file_note_count} notas"))
                            .size(10.5)
                            .color(NOTE_ACCENT),
                    );
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_space(8.0);
                    if ui.small_button("Abrir en editor").clicked() {
                        self.open_selected_file_in_editor();
                    }
                });
            });
            ui.separator();
            if let Some(reason) = &file.unavailable_reason {
                ui.label(RichText::new(reason).color(palette::DIM));
            } else if file.is_binary {
                ui.label(RichText::new("Cambios en archivo binario").color(palette::DIM));
            }
            // Virtualizado a mano (alturas variables: línea de diff, fila de
            // nota, fila de editor): un diff de miles de líneas solo renderiza
            // lo visible.
            ui.scope(|ui| {
                ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
                ScrollArea::vertical()
                    .id_salt("code-review-diff-view")
                    .max_height((height - 44.0).max(80.0))
                    .show_viewport(ui, |ui, viewport| {
                        ui.set_height(total_height.max(1.0));
                        let left = ui.min_rect().left();
                        let width = ui.min_rect().width();
                        let start = offsets
                            .partition_point(|offset| *offset <= viewport.top())
                            .saturating_sub(1)
                            .min(rows.len().saturating_sub(1));
                        for index in start..rows.len() {
                            let y = offsets[index];
                            if y >= viewport.bottom() {
                                break;
                            }
                            let row_rect = egui::Rect::from_min_size(
                                pos2(left, y),
                                vec2(width, review_row_height(&rows[index])),
                            );
                            match &rows[index] {
                                ReviewRow::Diff(line_index) => {
                                    let anchor = diff_line_anchor(&file.lines[*line_index]);
                                    draw_diff_line_at(ui, row_rect, &file.lines[*line_index]);
                                    // "+" en el gutter al hover: anotar esa línea.
                                    if let Some(line) = anchor {
                                        let plus_rect = egui::Rect::from_center_size(
                                            pos2(
                                                row_rect.left() + GUTTER_W * 2.0 + 10.0,
                                                row_rect.center().y,
                                            ),
                                            vec2(14.0, 14.0),
                                        );
                                        let response = ui.allocate_rect(plus_rect, Sense::click());
                                        if response.hovered() || response.clicked() {
                                            ui.painter().circle_filled(
                                                plus_rect.center(),
                                                6.5,
                                                palette::RAISED,
                                            );
                                            ui.painter().text(
                                                plus_rect.center(),
                                                Align2::CENTER_CENTER,
                                                "+",
                                                FontId::monospace(11.0),
                                                NOTE_ACCENT,
                                            );
                                        }
                                        if response.clicked() {
                                            actions.push(NoteAction::Create {
                                                file_path: file.path.clone(),
                                                line,
                                            });
                                        }
                                    }
                                }
                                ReviewRow::Note(note) => {
                                    draw_note_row(ui, row_rect, note, &mut actions);
                                }
                                ReviewRow::Editor => {
                                    self.draw_note_editor(ui, row_rect, &mut actions);
                                }
                            }
                        }
                    });
            });
        });

        for action in actions {
            self.apply_note_action(action);
        }
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
                        .margin(egui::Margin::symmetric(8, 5));
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
        let (repo_root, worktrees, error) = {
            let state = self.code_review.as_ref().unwrap();
            (
                state.repo_root.clone(),
                state.worktrees.clone(),
                state.worktree_error.clone(),
            )
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
                        let is_managed =
                            crate::orchestration::is_managed_worktree(&repo_root, &worktree.path);
                        if !worktree.is_main && is_managed {
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.add_space(14.0);
                                    if ui.small_button("Limpiar").clicked() {
                                        to_remove = Some(worktree.path.clone());
                                    }
                                },
                            );
                        }
                    });
                }
            });

        if let Some(path) = to_remove {
            self.remove_code_review_worktree(&path);
        }
        ui.add_space(4.0);
    }

    /// Pide el borrado al worker. `git worktree remove` borra un árbol de
    /// trabajo entero y en un repo grande tarda segundos: hacerlo acá
    /// congelaba la ventana.
    fn remove_code_review_worktree(&mut self, path: &Path) {
        let Some(state) = self.code_review.as_ref() else {
            return;
        };
        let repo_root = state.repo_root.clone();
        self.worktree_ops.request(WorktreeJob::Remove {
            repo_root,
            worktree_path: path.to_path_buf(),
        });
    }

    /// Recoge los resultados del worker de worktrees.
    pub(super) fn poll_worktree_ops(&mut self) {
        let outcomes = self.worktree_ops.poll();
        let Some(state) = self.code_review.as_mut() else {
            return;
        };
        for outcome in outcomes {
            state.worktrees = outcome.worktrees;
            state.worktree_error = outcome.error;
        }
    }

    // ----- Notas por línea sobre el diff (feedback localizado al agente) -----

    /// Aplica las acciones de notas colectadas durante el render (el render no
    /// puede mutar el estado: las filas se construyen desde un snapshot).
    fn apply_note_action(&mut self, action: NoteAction) {
        match action {
            NoteAction::Create { file_path, line } => {
                if let Some(state) = self.code_review.as_mut() {
                    state.note_target_picker = false;
                    state.editing_note = Some(NoteEditState {
                        note_id: None,
                        file_path,
                        line,
                        body: String::new(),
                    });
                }
            }
            NoteAction::Edit(id) => {
                if let Some(state) = self.code_review.as_mut() {
                    if let Some(note) = state.notes.notes.iter().find(|note| note.id == id) {
                        state.note_target_picker = false;
                        state.editing_note = Some(NoteEditState {
                            note_id: Some(id),
                            file_path: note.file_path.clone(),
                            line: note.line,
                            body: note.body.clone(),
                        });
                    }
                }
            }
            NoteAction::Delete(id) => {
                if let Some(state) = self.code_review.as_mut() {
                    state.notes.remove(id);
                }
                self.persist_review_notes();
            }
            NoteAction::SaveEditor => self.save_note_editor(),
            NoteAction::CancelEditor => {
                if let Some(state) = self.code_review.as_mut() {
                    state.editing_note = None;
                }
            }
        }
    }

    fn save_note_editor(&mut self) {
        let Some(editing) = self
            .code_review
            .as_ref()
            .and_then(|state| state.editing_note.clone())
        else {
            return;
        };
        if editing.body.trim().is_empty() {
            // Cuerpo vacío: si era una nota existente se borra (editar a
            // vacío = sacar la nota), si era nueva se descarta.
            if let Some(state) = self.code_review.as_mut() {
                if let Some(id) = editing.note_id {
                    state.notes.remove(id);
                }
                state.editing_note = None;
            }
            self.persist_review_notes();
            return;
        }
        if let Some(state) = self.code_review.as_mut() {
            match editing.note_id {
                Some(id) => state.notes.edit(id, &editing.body),
                None => {
                    state
                        .notes
                        .add(&editing.file_path, None, editing.line, &editing.body);
                }
            }
            state.editing_note = None;
        }
        self.persist_review_notes();
    }

    /// Guarda las notas del review actual en disco (por repo).
    fn persist_review_notes(&mut self) {
        let Some((repo_root, notes)) = self
            .code_review
            .as_ref()
            .map(|state| (state.repo_root.clone(), state.notes.clone()))
        else {
            return;
        };
        if let Err(err) = crate::orchestration::save_notes(&repo_root, &notes) {
            log::warn!("No se pudieron guardar las notas del diff: {err}");
        }
    }

    /// Fila del editor de notas: un TextEdit de una línea + Guardar/Cancelar.
    fn draw_note_editor(
        &mut self,
        ui: &mut egui::Ui,
        rect: egui::Rect,
        actions: &mut Vec<NoteAction>,
    ) {
        ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
            ui.painter().rect_filled(rect, 0.0, palette::RAISED);
            ui.painter().rect_stroke(
                rect,
                0.0,
                Stroke::new(1.0, NOTE_ACCENT),
                egui::StrokeKind::Middle,
            );
            ui.horizontal_centered(|ui| {
                ui.add_space(GUTTER_W * 2.0 + 18.0);
                let Some(state) = self.code_review.as_mut() else {
                    return;
                };
                let Some(editing) = state.editing_note.as_mut() else {
                    return;
                };
                let edit = egui::TextEdit::singleline(&mut editing.body)
                    .hint_text("Nota para el agente…")
                    .font(FontId::monospace(MONO_SIZE))
                    .text_color(palette::TEXT_STRONG)
                    .margin(egui::Margin::symmetric(6, 4));
                let response = ui.add_sized(
                    vec2(
                        (rect.width() - GUTTER_W * 2.0 - 220.0).max(80.0),
                        rect.height() - 6.0,
                    ),
                    edit,
                );
                // Foco automático al abrir el editor.
                if !response.has_focus() && editing.body.is_empty() {
                    response.request_focus();
                }
                if response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter)) {
                    actions.push(NoteAction::SaveEditor);
                }
                if ui.small_button("Guardar").clicked() {
                    actions.push(NoteAction::SaveEditor);
                }
                if ui.small_button("Cancelar").clicked() {
                    actions.push(NoteAction::CancelEditor);
                }
            });
        });
    }

    /// Manda todas las notas pendientes al agente con el formato de contrato.
    fn send_pending_notes(&mut self, panel_id: Uuid) {
        let Some((pending, repo_root)) = self.code_review.as_ref().map(|state| {
            (
                state
                    .notes
                    .pending()
                    .iter()
                    .map(|note| crate::orchestration::format_note(note))
                    .collect::<Vec<_>>(),
                state.repo_root.clone(),
            )
        }) else {
            return;
        };
        if pending.is_empty() {
            return;
        }
        let prompt = pending.join("\n\n");
        let mut delivered = false;
        for workspace in &mut self.workspaces {
            if workspace.send_prompt_to_panel(panel_id, &prompt) {
                delivered = true;
                break;
            }
        }
        if !delivered {
            self.toast_error("No se pudo escribir en ese terminal");
            return;
        }
        let sent_count = pending.len();
        if let Some(state) = self.code_review.as_mut() {
            let ids: Vec<Uuid> = state.notes.pending().iter().map(|note| note.id).collect();
            state.notes.mark_sent(&ids, chrono::Utc::now());
            state.note_target_picker = false;
        }
        self.persist_review_notes();
        let _ = repo_root;
        self.toast_success(format!(
            "{sent_count} nota{} enviada{} al agente",
            if sent_count == 1 { "" } else { "s" },
            if sent_count == 1 { "" } else { "s" }
        ));
    }

    /// Arranque del envío de notas: si el agente asociado al review está vivo
    /// se manda directo; si no, se abre el selector de destinos.
    fn begin_note_send(&mut self) {
        let target_panel = self
            .code_review
            .as_ref()
            .and_then(|state| state.target_panel);
        if let Some(panel_id) = target_panel {
            let alive = self
                .workspaces
                .iter()
                .flat_map(|workspace| workspace.panels.iter())
                .any(|panel| panel.id() == panel_id && panel.is_alive());
            if alive {
                self.send_pending_notes(panel_id);
                return;
            }
        }
        if let Some(state) = self.code_review.as_mut() {
            state.note_target_picker = true;
        }
    }

    /// Selector de destinos para las notas: paneles vivos del workspace
    /// activo (el agente del review ya se probó antes en `begin_note_send`).
    fn note_target_picker_ui(&mut self, ui: &mut egui::Ui) {
        let targets: Vec<(Uuid, String)> = self
            .ws()
            .panels
            .iter()
            .filter(|panel| panel.is_alive())
            .map(|panel| (panel.id(), panel.title().to_owned()))
            .collect();

        ui.horizontal(|ui| {
            ui.add_space(14.0);
            ui.label(
                RichText::new("Enviar las notas a…")
                    .size(12.0)
                    .color(palette::TEXT_STRONG)
                    .strong(),
            );
            if targets.is_empty() {
                ui.label(
                    RichText::new("No hay terminales vivos")
                        .size(11.0)
                        .color(DEL_FG),
                );
            }
        });
        ui.add_space(4.0);
        ScrollArea::vertical()
            .id_salt("note-target-picker")
            .max_height(76.0)
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                for (panel_id, title) in targets {
                    ui.horizontal(|ui| {
                        ui.add_space(14.0);
                        if ui.small_button(&title).clicked() {
                            self.send_pending_notes(panel_id);
                        }
                    });
                }
            });
        ui.add_space(4.0);
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

/// Dibuja una línea del diff en un rect absoluto (virtualización manual).
fn draw_diff_line_at(ui: &mut egui::Ui, rect: egui::Rect, line: &DiffLine) {
    ui.allocate_rect(rect, Sense::hover());
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

const NOTE_ROW_HEIGHT: f32 = 26.0;
const EDITOR_ROW_HEIGHT: f32 = 34.0;
const NOTE_ACCENT: Color32 = Color32::from_rgb(240, 200, 110);

/// Una fila del visor de diff: línea del diff, nota existente, o el editor.
enum ReviewRow<'a> {
    Diff(usize),
    Note(&'a crate::orchestration::DiffNote),
    Editor,
}

fn review_row_height(row: &ReviewRow<'_>) -> f32 {
    match row {
        ReviewRow::Diff(_) => LINE_HEIGHT,
        ReviewRow::Note(_) => NOTE_ROW_HEIGHT,
        ReviewRow::Editor => EDITOR_ROW_HEIGHT,
    }
}

/// Número de línea al que se anclan las notas: el del archivo nuevo si
/// existe, si no el del viejo (líneas borradas).
fn diff_line_anchor(line: &DiffLine) -> Option<u32> {
    line.new_ln.or(line.old_ln).map(|ln| ln as u32)
}

/// Plan de filas + offsets acumulados + altura total, para la virtualización
/// con alturas variables. Las notas van debajo de su línea; las que apuntan a
/// líneas fuera del diff visible van al final (mejor que perderlas).
fn build_review_rows<'a>(
    file: &FileDiff,
    notes: &'a [crate::orchestration::DiffNote],
    editing: Option<&NoteEditState>,
) -> (Vec<ReviewRow<'a>>, Vec<f32>, f32) {
    let file_notes: Vec<&crate::orchestration::DiffNote> = notes
        .iter()
        .filter(|note| note.file_path == file.path)
        .collect();
    let editor_anchor = editing
        .filter(|edit| edit.file_path == file.path)
        .map(|edit| edit.line);
    let mut anchored_notes: std::collections::HashSet<u32> = std::collections::HashSet::new();
    let mut editor_anchored = false;

    let mut rows: Vec<ReviewRow<'a>> = Vec::with_capacity(file.lines.len() + file_notes.len() + 1);
    for (index, line) in file.lines.iter().enumerate() {
        rows.push(ReviewRow::Diff(index));
        let Some(anchor) = diff_line_anchor(line) else {
            continue;
        };
        for note in file_notes.iter().filter(|note| note.line == anchor) {
            rows.push(ReviewRow::Note(note));
            anchored_notes.insert(anchor);
        }
        if editor_anchor == Some(anchor) {
            rows.push(ReviewRow::Editor);
            editor_anchored = true;
        }
    }
    for note in file_notes
        .iter()
        .filter(|note| !anchored_notes.contains(&note.line))
    {
        rows.push(ReviewRow::Note(note));
    }
    if editor_anchor.is_some() && !editor_anchored {
        rows.push(ReviewRow::Editor);
    }

    let mut offsets = Vec::with_capacity(rows.len() + 1);
    let mut acc = 0.0;
    for row in &rows {
        offsets.push(acc);
        acc += review_row_height(row);
    }
    (rows, offsets, acc)
}

/// Fila de una nota existente: fondo elevado, cuerpo, botones al hover.
fn draw_note_row(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    note: &crate::orchestration::DiffNote,
    actions: &mut Vec<NoteAction>,
) {
    let response = ui.allocate_rect(rect, Sense::hover());
    ui.painter().rect_filled(rect, 0.0, palette::RAISED);
    ui.painter().vline(
        rect.left() + GUTTER_W * 2.0 + 12.0,
        rect.y_range(),
        Stroke::new(2.0, NOTE_ACCENT),
    );
    let label = match note.start_line {
        Some(start) if start != note.line => format!("L{start}-{} · {}", note.line, note.body),
        _ => format!("L{} · {}", note.line, note.body),
    };
    let sent_suffix = if note.sent_at.is_some() { "  ✓" } else { "" };
    let font = FontId::monospace(MONO_SIZE * 0.95);
    let max_w = rect.width() - GUTTER_W * 2.0 - 120.0;
    let text_w = ui.fonts(|fonts| {
        fonts
            .layout_no_wrap(format!("{label}{sent_suffix}"), font.clone(), palette::TEXT)
            .size()
            .x
    });
    let shown = if text_w > max_w {
        let mono = MONO_SIZE * 0.95 * 0.6;
        let max_chars = (max_w / mono).floor().max(4.0) as usize;
        format!(
            "{}…{sent_suffix}",
            label.chars().take(max_chars).collect::<String>()
        )
    } else {
        format!("{label}{sent_suffix}")
    };
    ui.painter().text(
        pos2(rect.left() + GUTTER_W * 2.0 + 22.0, rect.center().y),
        Align2::LEFT_CENTER,
        shown,
        font,
        palette::TEXT,
    );
    if response.hovered() {
        let edit_rect = egui::Rect::from_center_size(
            pos2(rect.right() - 76.0, rect.center().y),
            vec2(56.0, 18.0),
        );
        let delete_rect = egui::Rect::from_center_size(
            pos2(rect.right() - 22.0, rect.center().y),
            vec2(36.0, 18.0),
        );
        if ui.allocate_rect(edit_rect, Sense::click()).clicked() {
            actions.push(NoteAction::Edit(note.id));
        }
        ui.painter().text(
            edit_rect.center(),
            Align2::CENTER_CENTER,
            "Editar",
            FontId::monospace(10.0),
            NOTE_ACCENT,
        );
        if ui.allocate_rect(delete_rect, Sense::click()).clicked() {
            actions.push(NoteAction::Delete(note.id));
        }
        ui.painter().text(
            delete_rect.center(),
            Align2::CENTER_CENTER,
            "✕",
            FontId::monospace(10.0),
            DEL_FG,
        );
    }
}

/// Acciones que el render de notas produce y el app aplica después (el render
/// trabaja sobre un snapshot y no puede mutar el estado directamente).
enum NoteAction {
    Create { file_path: String, line: u32 },
    Edit(Uuid),
    Delete(Uuid),
    SaveEditor,
    CancelEditor,
}

#[cfg(test)]
mod tests {
    use super::{build_review_rows, diff_line_anchor, ReviewRow, NOTE_ROW_HEIGHT};
    use crate::orchestration::{DiffLine, DiffLineKind, FileDiff};

    fn diff_line(new_ln: usize) -> DiffLine {
        DiffLine {
            kind: DiffLineKind::Added,
            old_ln: None,
            new_ln: Some(new_ln),
            text: format!("línea {new_ln}"),
        }
    }

    fn sample_file() -> FileDiff {
        FileDiff {
            path: "src/a.rs".to_owned(),
            lines: vec![diff_line(1), diff_line(2), diff_line(3)],
            ..Default::default()
        }
    }

    fn note_kinds(rows: &[ReviewRow<'_>]) -> Vec<&'static str> {
        rows.iter()
            .map(|row| match row {
                ReviewRow::Diff(_) => "diff",
                ReviewRow::Note(_) => "note",
                ReviewRow::Editor => "editor",
            })
            .collect()
    }

    #[test]
    fn a_note_is_anchored_right_below_its_line() {
        let file = sample_file();
        let mut notes = crate::orchestration::DiffNotes::default();
        notes.add("src/a.rs", None, 2, "acá");

        let (rows, offsets, total) = build_review_rows(&file, &notes.notes, None);
        assert_eq!(note_kinds(&rows), ["diff", "diff", "note", "diff"]);
        // Altura variable: la fila de nota ocupa NOTE_ROW_HEIGHT.
        assert_eq!(offsets.len(), rows.len());
        let expected_total = 3.0 * super::LINE_HEIGHT + NOTE_ROW_HEIGHT;
        assert!((total - expected_total).abs() < 0.01);
    }

    #[test]
    fn the_editor_row_follows_its_target_line() {
        let file = sample_file();
        let editing = super::NoteEditState {
            note_id: None,
            file_path: "src/a.rs".to_owned(),
            line: 1,
            body: String::new(),
        };
        let (rows, _, _) = build_review_rows(&file, &[], Some(&editing));
        assert_eq!(note_kinds(&rows), ["diff", "editor", "diff", "diff"]);
    }

    #[test]
    fn notes_for_lines_outside_the_diff_go_to_the_end() {
        // El diff puede no mostrar la línea (hunk lejano): la nota no se
        // pierde, va al final del archivo.
        let file = sample_file();
        let mut notes = crate::orchestration::DiffNotes::default();
        notes.add("src/a.rs", None, 999, "lejos");
        let (rows, _, _) = build_review_rows(&file, &notes.notes, None);
        assert_eq!(note_kinds(&rows), ["diff", "diff", "diff", "note"]);
    }

    #[test]
    fn notes_from_other_files_are_not_shown() {
        let file = sample_file();
        let mut notes = crate::orchestration::DiffNotes::default();
        notes.add("src/otro.rs", None, 1, "de otro archivo");
        let (rows, _, _) = build_review_rows(&file, &notes.notes, None);
        assert_eq!(note_kinds(&rows), ["diff", "diff", "diff"]);
    }

    #[test]
    fn removed_lines_anchor_by_old_line_number() {
        let removed = DiffLine {
            kind: DiffLineKind::Removed,
            old_ln: Some(7),
            new_ln: None,
            text: "borrada".to_owned(),
        };
        assert_eq!(diff_line_anchor(&removed), Some(7));
    }
}

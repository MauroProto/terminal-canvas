//! Superficie mínima de memoria: activas, pendientes, remember, approve, forget, export.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::thread;

use egui::{Align2, RichText, ScrollArea};
use uuid::Uuid;

use crate::memory::{
    format_remember_status, remember_selection, Actor, MemoryRecord, MemoryStatus, MemoryStore,
    ScopeKind,
};
use crate::theme::colors as palette;
use crate::utils::platform::downloads_dir;

use super::TerminalApp;

pub(super) struct MemoryUiState {
    pub(super) key: String,
    pub(super) content: String,
    pub(super) handoff: String,
    pub(super) status: Option<String>,
    pub(super) active: Vec<MemoryRecord>,
    pub(super) pending: Vec<MemoryRecord>,
    pub(super) project_label: String,
    pub(super) confirm_forget: Option<String>,
    cwd: Option<PathBuf>,
    workspace_id: Option<Uuid>,
    task_id: Option<Uuid>,
    worker: MemoryWorker,
    busy: bool,
}

enum MemoryJob {
    Load {
        cwd: Option<PathBuf>,
        workspace_id: Option<Uuid>,
        task_id: Option<Uuid>,
    },
    Remember {
        cwd: PathBuf,
        key: String,
        content: String,
    },
    Export {
        cwd: PathBuf,
        path: PathBuf,
        task_id: Option<Uuid>,
    },
    Handoff {
        cwd: PathBuf,
        summary: String,
        task_id: Option<Uuid>,
    },
    Forget(String),
    Approve(String),
    Reject(String),
}

enum MemoryCompletion {
    Loaded {
        project_label: String,
        active: Vec<MemoryRecord>,
        pending: Vec<MemoryRecord>,
        status: Option<String>,
    },
    Action {
        status: String,
        reload: bool,
    },
}

struct MemoryWorker {
    jobs: SyncSender<MemoryJob>,
    completions: Receiver<MemoryCompletion>,
}

impl MemoryWorker {
    fn new() -> Self {
        let (jobs_tx, jobs_rx) = mpsc::sync_channel(2);
        let (completion_tx, completion_rx) = mpsc::channel();
        thread::Builder::new()
            .name("memory-ui-worker".to_owned())
            .spawn(move || {
                while let Ok(job) = jobs_rx.recv() {
                    let completion = run_memory_job(job);
                    if completion_tx.send(completion).is_err() {
                        break;
                    }
                }
            })
            .expect("memory UI worker");
        Self {
            jobs: jobs_tx,
            completions: completion_rx,
        }
    }

    fn submit(&self, job: MemoryJob) -> bool {
        self.jobs.try_send(job).is_ok()
    }
}

fn run_memory_job(job: MemoryJob) -> MemoryCompletion {
    match job {
        MemoryJob::Load {
            cwd,
            workspace_id,
            task_id,
        } => {
            let Some(cwd) = cwd else {
                return MemoryCompletion::Loaded {
                    project_label: "sin proyecto".to_owned(),
                    active: Vec::new(),
                    pending: Vec::new(),
                    status: Some("Abrí un workspace con carpeta para usar memoria.".to_owned()),
                };
            };
            let result = (|| -> anyhow::Result<_> {
                let store = MemoryStore::open_default()?;
                let project = store.resolve_project(&cwd)?;
                let label = format!(
                    "{} · {}",
                    match project.identity_kind {
                        crate::memory::IdentityKind::GitCommonDir => "git",
                        crate::memory::IdentityKind::WorkspaceRoot => "folder",
                    },
                    project.canonical_id
                );
                let active =
                    store.list_visible_scoped(&cwd, MemoryStatus::Active, task_id, workspace_id)?;
                let pending = store.list_visible_scoped(
                    &cwd,
                    MemoryStatus::Candidate,
                    task_id,
                    workspace_id,
                )?;
                Ok((label, active, pending))
            })();
            match result {
                Ok((project_label, active, pending)) => MemoryCompletion::Loaded {
                    project_label,
                    active,
                    pending,
                    status: None,
                },
                Err(err) => MemoryCompletion::Loaded {
                    project_label: "sin proyecto".to_owned(),
                    active: Vec::new(),
                    pending: Vec::new(),
                    status: Some(format!("Memoria no disponible: {err}")),
                },
            }
        }
        MemoryJob::Remember { cwd, key, content } => {
            let result = MemoryStore::open_default().and_then(|store| {
                remember_selection(&store, &cwd, &key, &content, ScopeKind::Project)
            });
            match result {
                Ok(result) => MemoryCompletion::Action {
                    status: format_remember_status(&result),
                    reload: true,
                },
                Err(err) => memory_error(err),
            }
        }
        MemoryJob::Export { cwd, path, task_id } => {
            let result = MemoryStore::open_default()
                .and_then(|store| store.export_markdown_scoped(&cwd, task_id))
                .and_then(|text| {
                    std::fs::write(&path, text)
                        .map_err(anyhow::Error::from)
                        .map(|_| ())
                });
            match result {
                Ok(()) => MemoryCompletion::Action {
                    status: format!("Exportado a {}", path.display()),
                    reload: false,
                },
                Err(err) => memory_error(err),
            }
        }
        MemoryJob::Handoff {
            cwd,
            summary,
            task_id,
        } => {
            let result = MemoryStore::open_default().and_then(|store| {
                store.create_handoff(crate::memory::HandoffRequest {
                    cwd,
                    summary,
                    provider: None,
                    session_id: None,
                    orchestrator_task_id: task_id,
                    ttl_secs: Some(72 * 3600),
                })
            });
            match result {
                Ok(_) => MemoryCompletion::Action {
                    status: "Handoff registrado.".into(),
                    reload: true,
                },
                Err(err) => memory_error(err),
            }
        }
        MemoryJob::Forget(id) => run_store_mutation(|store| store.forget(&id, &Actor::human())),
        MemoryJob::Approve(id) => run_store_mutation(|store| store.approve(&id, &Actor::human())),
        MemoryJob::Reject(id) => run_store_mutation(|store| store.reject(&id, &Actor::human())),
    }
}

fn run_store_mutation<T>(
    mutation: impl FnOnce(&MemoryStore) -> anyhow::Result<T>,
) -> MemoryCompletion {
    match MemoryStore::open_default().and_then(|store| mutation(&store)) {
        Ok(_) => MemoryCompletion::Action {
            status: "Memoria actualizada.".to_owned(),
            reload: true,
        },
        Err(err) => memory_error(err),
    }
}

fn memory_error(error: anyhow::Error) -> MemoryCompletion {
    MemoryCompletion::Action {
        status: error.to_string(),
        reload: false,
    }
}

impl MemoryUiState {
    fn new(cwd: Option<PathBuf>, workspace_id: Option<Uuid>, task_id: Option<Uuid>) -> Self {
        let worker = MemoryWorker::new();
        let busy = worker.submit(MemoryJob::Load {
            cwd: cwd.clone(),
            workspace_id,
            task_id,
        });
        Self {
            key: String::new(),
            content: String::new(),
            handoff: String::new(),
            status: Some("Cargando memoria…".to_owned()),
            active: Vec::new(),
            pending: Vec::new(),
            project_label: "sin proyecto".to_owned(),
            confirm_forget: None,
            cwd,
            workspace_id,
            task_id,
            worker,
            busy,
        }
    }

    fn submit(&mut self, job: MemoryJob) {
        if !self.busy && self.worker.submit(job) {
            self.busy = true;
            self.status = Some("Guardando…".to_owned());
        }
    }

    fn request_reload(&mut self) {
        if self.worker.submit(MemoryJob::Load {
            cwd: self.cwd.clone(),
            workspace_id: self.workspace_id,
            task_id: self.task_id,
        }) {
            self.busy = true;
        }
    }

    fn poll(&mut self) {
        while let Ok(completion) = self.worker.completions.try_recv() {
            self.busy = false;
            match completion {
                MemoryCompletion::Loaded {
                    project_label,
                    active,
                    pending,
                    status,
                } => {
                    self.project_label = project_label;
                    self.active = active;
                    self.pending = pending;
                    if status.is_some() || self.status.as_deref() == Some("Cargando memoria…") {
                        self.status = status;
                    }
                }
                MemoryCompletion::Action { status, reload } => {
                    self.status = Some(status);
                    if reload {
                        self.request_reload();
                    }
                }
            }
        }
    }
}

impl TerminalApp {
    pub(super) fn open_memory_hub(&mut self) {
        let cwd = self.ws().cwd.clone();
        let workspace_id = self.ws().id;
        self.memory_ui = Some(MemoryUiState::new(
            cwd,
            Some(workspace_id),
            self.ws()
                .focused_panel()
                .and_then(|panel| panel.focused_memory_task_id()),
        ));
    }

    pub(super) fn open_remember_selection(&mut self) {
        let selected = self
            .ws()
            .focused_panel()
            .and_then(|panel| panel.selected_text())
            .unwrap_or_default();
        let mut state = MemoryUiState::new(
            self.ws().cwd.clone(),
            Some(self.ws().id),
            self.ws()
                .focused_panel()
                .and_then(|panel| panel.focused_memory_task_id()),
        );
        if !selected.trim().is_empty() {
            state.content = selected.trim().to_owned();
            if state.key.is_empty() {
                state.key = "note/selection".to_owned();
            }
        } else {
            state.status = Some("No hay texto seleccionado en el terminal.".to_owned());
        }
        self.memory_ui = Some(state);
    }

    pub(super) fn open_create_handoff(&mut self) {
        let mut state = MemoryUiState::new(
            self.ws().cwd.clone(),
            Some(self.ws().id),
            self.ws()
                .focused_panel()
                .and_then(|panel| panel.focused_memory_task_id()),
        );
        if state.handoff.is_empty() {
            state.handoff = "Continuar acá. Hecho: …  Siguiente: …  Bloqueos: …".to_owned();
        }
        self.memory_ui = Some(state);
    }

    pub(super) fn show_memory_hub(&mut self, ctx: &egui::Context) {
        if self.memory_ui.is_none() {
            return;
        }
        if ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
            self.memory_ui = None;
            return;
        }

        let mut close = false;
        if let Some(state) = self.memory_ui.as_mut() {
            state.poll();
            if state.busy {
                ctx.request_repaint_after(std::time::Duration::from_millis(16));
            }
        }
        let screen = ctx.screen_rect();

        egui::Area::new(egui::Id::new("memory-backdrop"))
            .order(egui::Order::Middle)
            .fixed_pos(screen.min)
            .show(ctx, |ui| {
                let rect = ui.max_rect().union(screen);
                ui.allocate_rect(rect, egui::Sense::click());
                ui.painter().rect_filled(
                    rect,
                    0.0,
                    egui::Color32::from_rgba_premultiplied(0, 0, 0, 170),
                );
            });

        egui::Window::new("Shared memory")
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .resizable(true)
            .default_size([560.0, 480.0])
            .show(ctx, |ui| {
                let Some(state) = self.memory_ui.as_mut() else {
                    return;
                };
                ui.label(
                    RichText::new(state.project_label.clone())
                        .size(11.0)
                        .color(palette::DIM),
                );
                if let Some(status) = &state.status {
                    ui.label(RichText::new(status).size(12.0).color(palette::TEXT));
                }
                ui.add_space(8.0);
                ui.label(
                    RichText::new("Remember")
                        .size(13.0)
                        .color(palette::TEXT_STRONG),
                );
                ui.add(
                    egui::TextEdit::singleline(&mut state.key)
                        .hint_text("key (architecture/auth-strategy)")
                        .desired_width(f32::INFINITY),
                );
                ui.add(
                    egui::TextEdit::multiline(&mut state.content)
                        .hint_text("hecho o decisión")
                        .desired_width(f32::INFINITY)
                        .desired_rows(3),
                );
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(!state.busy, egui::Button::new("Remember"))
                        .clicked()
                    {
                        if let Some(cwd) = state.cwd.clone() {
                            state.submit(MemoryJob::Remember {
                                cwd,
                                key: state.key.trim().to_owned(),
                                content: state.content.trim().to_owned(),
                            });
                        } else {
                            state.status = Some("No hay carpeta de workspace.".into());
                        }
                    }
                    if ui
                        .add_enabled(!state.busy, egui::Button::new("Export markdown"))
                        .clicked()
                    {
                        if let Some(cwd) = state.cwd.clone() {
                            state.submit(MemoryJob::Export {
                                cwd,
                                path: downloads_dir().join("terminalcanvas-memory.md"),
                                task_id: state.task_id,
                            });
                        }
                    }
                });

                ui.add_space(10.0);
                ui.label(
                    RichText::new("Handoff")
                        .size(13.0)
                        .color(palette::TEXT_STRONG),
                );
                ui.add(
                    egui::TextEdit::multiline(&mut state.handoff)
                        .hint_text("qué quedó y qué sigue")
                        .desired_width(f32::INFINITY)
                        .desired_rows(2),
                );
                if ui
                    .add_enabled(!state.busy, egui::Button::new("Create handoff"))
                    .clicked()
                {
                    if let Some(cwd) = state.cwd.clone() {
                        state.submit(MemoryJob::Handoff {
                            cwd,
                            summary: state.handoff.trim().to_owned(),
                            task_id: state.task_id,
                        });
                    }
                }

                ui.add_space(10.0);
                ui.label(
                    RichText::new("Active")
                        .size(13.0)
                        .color(palette::TEXT_STRONG),
                );
                ScrollArea::vertical()
                    .id_salt("memory-active")
                    .max_height(140.0)
                    .show(ui, |ui| {
                        if state.active.is_empty() {
                            ui.label(RichText::new("Nada todavía.").color(palette::DIM));
                        }
                        let mut forget = None;
                        let mut confirm_forget = state.confirm_forget.clone();
                        for memory in &state.active {
                            ui.group(|ui| {
                                ui.label(
                                    RichText::new(&memory.stable_key)
                                        .size(12.0)
                                        .color(palette::TEXT_STRONG),
                                );
                                ui.label(&memory.content);
                                ui.horizontal(|ui| {
                                    ui.label(
                                        RichText::new(format!(
                                            "{} · {} · rev {}",
                                            memory.scope_kind.as_str(),
                                            memory.trust_class.as_str(),
                                            memory.current_revision
                                        ))
                                        .size(10.0)
                                        .color(palette::DIM),
                                    );
                                    if confirm_forget.as_deref() == Some(memory.id.as_str()) {
                                        if ui.small_button("Confirm forget").clicked() {
                                            forget = Some(memory.id.clone());
                                            confirm_forget = None;
                                        }
                                        if ui.small_button("Cancel").clicked() {
                                            confirm_forget = None;
                                        }
                                    } else if ui.small_button("Forget…").clicked() {
                                        confirm_forget = Some(memory.id.clone());
                                    }
                                });
                            });
                        }
                        state.confirm_forget = confirm_forget;
                        if let Some(id) = forget {
                            state.submit(MemoryJob::Forget(id));
                        }
                    });

                ui.add_space(8.0);
                ui.label(
                    RichText::new("Pending")
                        .size(13.0)
                        .color(palette::TEXT_STRONG),
                );
                ScrollArea::vertical()
                    .id_salt("memory-pending")
                    .max_height(120.0)
                    .show(ui, |ui| {
                        if state.pending.is_empty() {
                            ui.label(RichText::new("Sin candidatas.").color(palette::DIM));
                        }
                        let mut approve = None;
                        let mut forget = None;
                        for memory in &state.pending {
                            ui.group(|ui| {
                                ui.label(
                                    RichText::new(&memory.stable_key)
                                        .size(12.0)
                                        .color(palette::TEXT_STRONG),
                                );
                                ui.label(&memory.content);
                                ui.horizontal(|ui| {
                                    ui.label(
                                        RichText::new(format!(
                                            "{} · {} · rev {}",
                                            memory.scope_kind.as_str(),
                                            memory.trust_class.as_str(),
                                            memory.current_revision
                                        ))
                                        .size(10.0)
                                        .color(palette::DIM),
                                    );
                                    if ui.small_button("Approve").clicked() {
                                        approve = Some(memory.id.clone());
                                    }
                                    if ui.small_button("Reject").clicked() {
                                        forget = Some(memory.id.clone());
                                    }
                                });
                            });
                        }
                        if let Some(id) = approve {
                            state.submit(MemoryJob::Approve(id));
                        }
                        if let Some(id) = forget {
                            state.submit(MemoryJob::Reject(id));
                        }
                    });

                ui.add_space(8.0);
                if ui.button("Close").clicked() {
                    close = true;
                }
            });

        if close {
            self.memory_ui = None;
        }
    }
}

//! Pestaña "Tasks" del sidebar (P2.13, T2): PRs e issues del repo vía `gh`.
//!
//! Solo dibuja: los datos los trae el worker de [`crate::orchestration::GhClient`]
//! y las acciones vuelven como [`SidebarResponse`].

use egui::{RichText, Ui};
use std::path::PathBuf;

use super::{SidebarResponse, TEXT_MUTED, TEXT_PRIMARY};
use crate::orchestration::{GhAvailability, GhSnapshot, LinearAvailability, LinearSnapshot};

/// Estado de la pestaña que vive en el app (no en el sidebar).
#[derive(Debug, Clone, Default)]
pub struct TasksState {
    /// Repo al que pertenecen `availability` y `snapshot`. Evita mostrar las
    /// tareas del workspace anterior mientras cambia la carpeta activa.
    pub repo_root: Option<PathBuf>,
    pub availability: Option<GhAvailability>,
    pub snapshot: GhSnapshot,
    pub loading: bool,
    /// Fuente extra de issues: Linear (P3.17).
    pub linear_availability: Option<LinearAvailability>,
    pub linear_snapshot: LinearSnapshot,
}

/// Color del estado de un PR/issue.
fn state_color(state: &str) -> egui::Color32 {
    match state.to_ascii_uppercase().as_str() {
        "OPEN" => egui::Color32::from_rgb(110, 190, 130),
        "MERGED" => egui::Color32::from_rgb(160, 130, 220),
        "CLOSED" => egui::Color32::from_rgb(200, 110, 110),
        _ => TEXT_MUTED,
    }
}

fn draw_message(ui: &mut Ui, message: &str) {
    ui.horizontal(|ui| {
        ui.add_space(12.0);
        ui.label(RichText::new(message).size(11.5).color(TEXT_MUTED));
    });
}

fn task_button(ui: &mut Ui, title: &str) -> egui::Response {
    ui.add_sized(
        egui::vec2(ui.available_width().max(48.0), 24.0),
        egui::Button::new(RichText::new(title).size(12.0).color(TEXT_PRIMARY))
            .frame(false)
            .truncate(),
    )
}

fn task_metadata(ui: &mut Ui, text: &str) {
    ui.add_sized(
        egui::vec2(ui.available_width().max(48.0), 20.0),
        egui::Label::new(RichText::new(text).size(10.5).color(TEXT_MUTED).monospace()).truncate(),
    );
}

pub fn draw_tasks(ui: &mut Ui, state: &TasksState) -> Vec<SidebarResponse> {
    let mut responses = Vec::new();

    ui.horizontal(|ui| {
        ui.add_space(12.0);
        ui.label(
            RichText::new("GitHub")
                .size(12.0)
                .color(TEXT_PRIMARY)
                .strong(),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(8.0);
            if ui
                .add_sized(
                    egui::vec2(64.0, 28.0),
                    egui::Button::new(RichText::new("Refresh").size(11.0)),
                )
                .clicked()
            {
                responses.push(SidebarResponse::RefreshTasks);
            }
            if state.loading {
                ui.label(RichText::new("Loading…").size(10.5).color(TEXT_MUTED));
            }
        });
    });
    ui.add_space(6.0);

    match state.availability.as_ref() {
        None => {
            draw_message(ui, "Sin consultar todavía");
            return responses;
        }
        Some(GhAvailability::Unavailable(reason)) => {
            draw_message(ui, reason);
            return responses;
        }
        Some(GhAvailability::Ready) => {}
    }

    if state.snapshot.pull_requests.is_empty() && state.snapshot.issues.is_empty() {
        draw_message(ui, "Sin PRs ni issues abiertos");
        return responses;
    }

    if !state.snapshot.pull_requests.is_empty() {
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            ui.label(RichText::new("Pull requests").size(11.0).color(TEXT_MUTED));
        });
        for pr in &state.snapshot.pull_requests {
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                ui.label(
                    RichText::new(format!("#{}", pr.number))
                        .size(11.5)
                        .color(state_color(&pr.state))
                        .monospace(),
                );
                if task_button(ui, &pr.title).clicked() {
                    responses.push(SidebarResponse::OpenTask(pr.number));
                }
            });
            ui.horizontal(|ui| {
                ui.add_space(36.0);
                task_metadata(ui, &pr.head_ref_name);
            });
            ui.add_space(2.0);
        }
    }

    if !state.snapshot.issues.is_empty() {
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            ui.label(RichText::new("Issues").size(11.0).color(TEXT_MUTED));
        });
        for issue in &state.snapshot.issues {
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                ui.label(
                    RichText::new(format!("#{}", issue.number))
                        .size(11.5)
                        .color(state_color(&issue.state))
                        .monospace(),
                );
                if task_button(ui, &issue.title).clicked() {
                    responses.push(SidebarResponse::OpenTask(issue.number));
                }
            });
            ui.horizontal(|ui| {
                ui.add_space(36.0);
                if ui
                    .add_sized(
                        egui::vec2(76.0, 24.0),
                        egui::Button::new(RichText::new("Start work").size(10.5)),
                    )
                    .clicked()
                {
                    responses.push(SidebarResponse::StartWorkOnIssue(issue.number));
                }
            });
            ui.add_space(2.0);
        }
    }

    draw_linear_section(ui, state, &mut responses);

    responses
}

/// Issues de Linear como fuente extra de la pestaña (P3.17, T2).
fn draw_linear_section(ui: &mut Ui, state: &TasksState, responses: &mut Vec<SidebarResponse>) {
    // La integración es opt-in: sin token configurado no se muestra nada, para
    // no llenar el panel de ruido a quien no la usa.
    let Some(availability) = state.linear_availability.as_ref() else {
        return;
    };
    ui.add_space(10.0);
    ui.horizontal(|ui| {
        ui.add_space(12.0);
        ui.label(
            RichText::new("Linear")
                .size(12.0)
                .color(TEXT_PRIMARY)
                .strong(),
        );
    });
    if let LinearAvailability::Unavailable(reason) = availability {
        draw_message(ui, reason);
        return;
    }
    if state.linear_snapshot.issues.is_empty() {
        draw_message(ui, "Sin issues asignados");
        return;
    }
    for issue in &state.linear_snapshot.issues {
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            ui.label(
                RichText::new(&issue.identifier)
                    .size(11.5)
                    .color(state_color(&issue.state))
                    .monospace(),
            );
            ui.add_sized(
                egui::vec2(ui.available_width().max(48.0), 24.0),
                egui::Label::new(RichText::new(&issue.title).size(12.0).color(TEXT_PRIMARY))
                    .truncate(),
            );
        });
        ui.horizontal(|ui| {
            ui.add_space(36.0);
            if ui
                .add_sized(
                    egui::vec2(76.0, 24.0),
                    egui::Button::new(RichText::new("Start work").size(10.5)),
                )
                .clicked()
            {
                responses.push(SidebarResponse::StartWorkOnLinearIssue(
                    issue.identifier.clone(),
                ));
            }
        });
        ui.add_space(2.0);
    }
}

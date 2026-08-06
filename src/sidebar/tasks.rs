//! Pestaña "Tasks" del sidebar (P2.13, T2): PRs e issues del repo vía `gh`.
//!
//! Solo dibuja: los datos los trae el worker de [`crate::orchestration::GhClient`]
//! y las acciones vuelven como [`SidebarResponse`].

use egui::{RichText, Ui};

use super::{SidebarResponse, TEXT_MUTED, TEXT_PRIMARY};
use crate::orchestration::{GhAvailability, GhSnapshot};

/// Estado de la pestaña que vive en el app (no en el sidebar).
#[derive(Debug, Clone, Default)]
pub struct TasksState {
    pub availability: Option<GhAvailability>,
    pub snapshot: GhSnapshot,
    pub loading: bool,
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

pub fn draw_tasks(ui: &mut Ui, state: &TasksState) -> Vec<SidebarResponse> {
    let mut responses = Vec::new();

    ui.horizontal(|ui| {
        ui.add_space(12.0);
        ui.label(
            RichText::new("GitHub")
                .size(11.0)
                .color(TEXT_MUTED)
                .strong(),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(10.0);
            if ui.small_button("Refresh").clicked() {
                responses.push(SidebarResponse::RefreshTasks);
            }
            if state.loading {
                ui.label(RichText::new("…").size(11.0).color(TEXT_MUTED));
            }
        });
    });
    ui.add_space(4.0);

    match state.availability.as_ref() {
        None => {
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                ui.label(
                    RichText::new("Sin consultar todavía")
                        .size(11.0)
                        .color(TEXT_MUTED),
                );
            });
            return responses;
        }
        Some(GhAvailability::Unavailable(reason)) => {
            ui.horizontal_wrapped(|ui| {
                ui.add_space(12.0);
                ui.label(RichText::new(reason).size(11.0).color(TEXT_MUTED));
            });
            return responses;
        }
        Some(GhAvailability::Ready) => {}
    }

    if state.snapshot.pull_requests.is_empty() && state.snapshot.issues.is_empty() {
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            ui.label(
                RichText::new("Sin PRs ni issues abiertos")
                    .size(11.0)
                    .color(TEXT_MUTED),
            );
        });
        return responses;
    }

    if !state.snapshot.pull_requests.is_empty() {
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            ui.label(RichText::new("Pull requests").size(10.5).color(TEXT_MUTED));
        });
        for pr in &state.snapshot.pull_requests {
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                ui.label(
                    RichText::new(format!("#{}", pr.number))
                        .size(11.0)
                        .color(state_color(&pr.state))
                        .monospace(),
                );
                let label = ui.selectable_label(
                    false,
                    RichText::new(&pr.title).size(11.5).color(TEXT_PRIMARY),
                );
                if label.clicked() {
                    responses.push(SidebarResponse::OpenTask(pr.number));
                }
            });
            ui.horizontal(|ui| {
                ui.add_space(30.0);
                ui.label(
                    RichText::new(&pr.head_ref_name)
                        .size(10.0)
                        .color(TEXT_MUTED)
                        .monospace(),
                );
            });
        }
    }

    if !state.snapshot.issues.is_empty() {
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            ui.label(RichText::new("Issues").size(10.5).color(TEXT_MUTED));
        });
        for issue in &state.snapshot.issues {
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                ui.label(
                    RichText::new(format!("#{}", issue.number))
                        .size(11.0)
                        .color(state_color(&issue.state))
                        .monospace(),
                );
                let label = ui.selectable_label(
                    false,
                    RichText::new(&issue.title).size(11.5).color(TEXT_PRIMARY),
                );
                if label.clicked() {
                    responses.push(SidebarResponse::OpenTask(issue.number));
                }
            });
            ui.horizontal(|ui| {
                ui.add_space(30.0);
                if ui.small_button("Start work").clicked() {
                    responses.push(SidebarResponse::StartWorkOnIssue(issue.number));
                }
            });
        }
    }

    responses
}

//! Broadcast: manda el mismo comando/prompt a varios terminales de una vez.
//!
//! Es la pieza que hace usable el workflow multi-agente: en vez de repetir
//! "git pull" o la misma instrucción en ocho paneles, se escribe una vez y se
//! elige a quién va. La selección de destinos es lógica pura y está testeada;
//! la UI sólo la maneja.

use egui::{vec2, Align2, RichText, ScrollArea};
use uuid::Uuid;

use crate::orchestration::portable_context::{ContextOrigin, TerminalSelectionContext};
use crate::orchestration::AgentProvider;
use crate::panel::WorkspacePanel;
use crate::theme::colors as palette;

use super::TerminalApp;

/// Un destino posible del broadcast, tal como se lista en el diálogo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct BroadcastTarget {
    pub(super) panel_id: Uuid,
    pub(super) leaf_id: Uuid,
    runtime_session_id: Option<Uuid>,
    provider_hint: Option<AgentProvider>,
    pub(super) title: String,
    /// Un panel sin terminal vivo no puede recibir nada: se lista en gris y
    /// no se puede tildar.
    pub(super) alive: bool,
    pub(super) selected: bool,
}

pub(super) struct BroadcastState {
    workspace_id: Uuid,
    request_focus: bool,
    pub(super) text: String,
    pub(super) targets: Vec<BroadcastTarget>,
    context: Option<TerminalSelectionContext>,
    context_error: Option<String>,
    include_context: bool,
}

impl BroadcastState {
    fn eligible(&self, target: &BroadcastTarget) -> bool {
        target.alive
            && (!self.include_context
                || (target
                    .provider_hint
                    .is_some_and(|provider| provider != AgentProvider::Unknown)
                    && self.context.as_ref().is_some_and(|context| {
                        context.origin.panel_id != target.panel_id
                            || context.origin.leaf_id != target.leaf_id
                    })))
    }

    /// Panels vivos y tildados. Los muertos se filtran incluso si quedaron
    /// tildados de antes (el panel pudo morir con el diálogo abierto).
    fn selected_ids(&self) -> Vec<Uuid> {
        self.targets
            .iter()
            .filter(|target| self.eligible(target) && target.selected)
            .map(|target| target.panel_id)
            .collect()
    }

    fn selected_count(&self) -> usize {
        self.selected_ids().len()
    }

    fn alive_count(&self) -> usize {
        self.targets
            .iter()
            .filter(|target| self.eligible(target))
            .count()
    }

    /// ¿Se puede enviar? Hace falta texto y al menos un destino vivo tildado.
    fn can_send(&self) -> bool {
        !self.text.trim().is_empty() && self.selected_count() > 0
    }

    fn set_all_alive(&mut self, selected: bool) {
        for index in 0..self.targets.len() {
            let eligible = self.eligible(&self.targets[index]);
            self.targets[index].selected = selected && eligible;
        }
    }

    fn toggle_context(&mut self, enabled: bool) {
        self.include_context = enabled && self.context.is_some();
        // Attaching an excerpt must not silently broadcast it to all agents.
        // Both mode transitions require an explicit destination selection.
        self.set_all_alive(false);
    }

    fn prepare_prompt(&self) -> std::io::Result<String> {
        if self.include_context {
            self.context
                .as_ref()
                .ok_or_else(|| std::io::Error::other("El contexto seleccionado ya no existe."))?
                .prepare_prompt(&self.text)
        } else {
            Ok(self.text.trim().to_owned())
        }
    }
}

impl BroadcastTarget {
    fn matches_panel(&self, panel: &WorkspacePanel) -> bool {
        panel.is_alive()
            && panel.focused_leaf_id() == self.leaf_id
            && panel.focused_runtime_session_id() == self.runtime_session_id
            && panel.provider_hint() == self.provider_hint
    }
}

impl TerminalApp {
    pub(super) fn open_broadcast(&mut self) {
        // Freeze the selection once. Rendering, retries, and workspace changes
        // cannot silently replace the excerpt the user reviewed.
        let candidate = self.ws().focused_panel().and_then(|panel| {
            let selection = panel.selected_text()?;
            if selection.trim().is_empty() {
                return None;
            }
            Some(TerminalSelectionContext::new(
                ContextOrigin {
                    workspace_id: self.ws().id,
                    panel_id: panel.id(),
                    leaf_id: panel.focused_leaf_id(),
                    runtime_session_id: panel.focused_runtime_session_id(),
                    memory_task_id: panel.focused_memory_task_id(),
                    title: panel.title().to_owned(),
                    provider_hint: panel.provider_hint(),
                    cwd: panel.current_cwd(),
                },
                selection,
            ))
        });
        let (context, context_error) = match candidate {
            Some(Ok(context)) => (Some(context), None),
            Some(Err(error)) => (None, Some(error.to_string())),
            None => (None, None),
        };
        let targets: Vec<BroadcastTarget> = self
            .ws()
            .panels
            .iter()
            .map(|panel| {
                // Con splits el título dice a qué hoja va (P2.11, T5): sin
                // esto, cuatro hojas del mismo panel se ven idénticas.
                let title = match panel.leaf_count() {
                    count if count > 1 => {
                        format!(
                            "{} · hoja {}/{}",
                            panel.title(),
                            panel.focused_leaf_index() + 1,
                            count
                        )
                    }
                    _ => panel.title().to_owned(),
                };
                BroadcastTarget {
                    panel_id: panel.id(),
                    leaf_id: panel.focused_leaf_id(),
                    runtime_session_id: panel.focused_runtime_session_id(),
                    provider_hint: panel.provider_hint(),
                    title,
                    alive: panel.is_alive(),
                    // Por defecto todos los vivos: el caso común es "a todos".
                    selected: panel.is_alive(),
                }
            })
            .collect();

        if targets.iter().all(|target| !target.alive) {
            self.toast_error("No hay terminales vivos a los que enviar");
            return;
        }
        self.broadcast = Some(BroadcastState {
            workspace_id: self.ws().id,
            request_focus: true,
            text: String::new(),
            targets,
            context,
            context_error,
            include_context: false,
        });
    }

    pub(super) fn show_broadcast(&mut self, ctx: &egui::Context) {
        if self.broadcast.is_none() {
            return;
        }
        if ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
            self.broadcast = None;
            return;
        }

        self.refresh_broadcast_targets();
        let mut send = false;
        let mut cancel = false;
        let screen = ctx.content_rect();

        egui::Area::new(egui::Id::new("broadcast-backdrop"))
            .order(egui::Order::Middle)
            .fixed_pos(screen.min)
            .show(ctx, |ui| {
                ui.painter().rect_filled(
                    screen,
                    0.0,
                    egui::Color32::from_rgba_premultiplied(0, 0, 0, 150),
                );
                ui.allocate_space(screen.size());
            });

        egui::Area::new(egui::Id::new("broadcast-dialog"))
            .order(egui::Order::Foreground)
            .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
            .show(ctx, |ui| {
                egui::Frame::NONE
                    .fill(palette::SURFACE)
                    .stroke(egui::Stroke::new(1.0_f32, palette::LINE))
                    .corner_radius(12.0)
                    .inner_margin(18.0)
                    .show(ui, |ui| {
                        ui.set_width(520.0);
                        let Some(state) = self.broadcast.as_mut() else {
                            return;
                        };

                        ui.label(
                            RichText::new("Broadcast a terminales")
                                .size(15.0)
                                .color(palette::TEXT_STRONG),
                        );
                        ui.label(
                            RichText::new(
                                "El texto se envía como prompt (con Enter) a cada terminal tildado.",
                            )
                            .size(11.0)
                            .color(palette::DIM),
                        );
                        ui.add_space(10.0);

                        if let Some(context) = state.context.as_ref() {
                            let label = format!(
                                "Incluir selección de {} · {}",
                                context.origin.title,
                                context.origin.provider_hint.unwrap_or_default().label()
                            );
                            let mut enabled = state.include_context;
                            if ui.checkbox(&mut enabled, label).changed() {
                                state.toggle_context(enabled);
                            }
                            if state.include_context {
                                let context = state.context.as_ref().expect("captured context");
                                ui.label(
                                    RichText::new("Elegí los agentes de destino. Se comparte sólo el fragmento seleccionado y su origen.")
                                        .size(11.0)
                                        .color(palette::DIM),
                                );
                                ui.collapsing("Vista previa del contexto", |ui| {
                                    if let Some(cwd) = &context.origin.cwd {
                                        ui.label(format!("Carpeta de origen: {cwd}"));
                                    }
                                    ui.label(format!("Captura: {} · hoja {}", context.origin.panel_id, context.origin.leaf_id));
                                    ScrollArea::vertical()
                                        .id_salt("broadcast-context-preview")
                                        .max_height(140.0)
                                        .show(ui, |ui| {
                                            ui.label(RichText::new(&context.selection).monospace());
                                        });
                                });
                            }
                        } else {
                            ui.label(
                                RichText::new(state.context_error.as_deref().unwrap_or(
                                    "Para compartir contexto, seleccioná texto en el chat de origen antes de abrir este diálogo.",
                                ))
                                .size(11.0)
                                .color(palette::DIM),
                            );
                        }
                        ui.add_space(6.0);

                        let field = ui.add(
                            egui::TextEdit::multiline(&mut state.text)
                                .desired_rows(3)
                                .desired_width(f32::INFINITY)
                                .hint_text(if state.include_context {
                                    "Revisá esta propuesta y continuá desde acá…"
                                } else {
                                    "Mensaje o comando para los destinos elegidos"
                                }),
                        );
                        if std::mem::take(&mut state.request_focus) {
                            field.request_focus();
                        }
                        ui.add_space(10.0);

                        let (alive, selected) = (state.alive_count(), state.selected_count());
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(format!("Destinos ({selected}/{alive})"))
                                    .size(12.0)
                                    .color(palette::TEXT),
                            );
                            if ui.small_button("Todos").clicked() {
                                state.set_all_alive(true);
                            }
                            if ui.small_button("Ninguno").clicked() {
                                state.set_all_alive(false);
                            }
                        });
                        ui.add_space(4.0);

                        ScrollArea::vertical()
                            .id_salt("broadcast-targets")
                            .max_height(200.0)
                            .show(ui, |ui| {
                                for index in 0..state.targets.len() {
                                    let eligible = state.eligible(&state.targets[index]);
                                    let target = &mut state.targets[index];
                                    let label = format!("{} · {}", target.title, target.provider_hint.unwrap_or_default().label());
                                    if eligible {
                                        ui.checkbox(&mut target.selected, label);
                                    } else {
                                        // Sin terminal vivo: visible pero no
                                        // seleccionable, para que se entienda
                                        // por qué no recibe nada.
                                        ui.add_enabled_ui(false, |ui| {
                                            let mut off = false;
                                            ui.checkbox(
                                                &mut off,
                                                format!("{label} (no disponible para este envío)"),
                                            );
                                        });
                                    }
                                }
                            });

                        ui.add_space(14.0);
                        let can_send = state.can_send();
                        ui.horizontal(|ui| {
                            if ui
                                .add_enabled(can_send, egui::Button::new("Enviar"))
                                .clicked()
                            {
                                send = true;
                            }
                            if ui.button("Cancelar").clicked() {
                                cancel = true;
                            }
                            if !can_send {
                                ui.label(
                                    RichText::new("Escribí un comando y elegí al menos un destino")
                                        .size(10.0)
                                        .color(palette::DIM),
                                );
                            }
                        });
                    });
            });

        // Ctrl+Enter envía sin tener que ir al botón.
        if ctx.input(|input| input.modifiers.command && input.key_pressed(egui::Key::Enter)) {
            send = true;
        }

        if cancel {
            self.broadcast = None;
            return;
        }
        if send {
            self.dispatch_broadcast();
        }
    }

    fn refresh_broadcast_targets(&mut self) {
        let Some(state) = self.broadcast.as_mut() else {
            return;
        };
        let workspace = self
            .workspaces
            .iter()
            .find(|workspace| workspace.id == state.workspace_id);
        for target in &mut state.targets {
            target.alive = workspace
                .and_then(|workspace| workspace.panel(target.panel_id))
                .is_some_and(|panel| target.matches_panel(panel));
        }
    }

    fn dispatch_broadcast(&mut self) {
        self.refresh_broadcast_targets();
        let Some(state) = self.broadcast.as_ref() else {
            return;
        };
        if !state.can_send() {
            return;
        }
        let text = match state.prepare_prompt() {
            Ok(text) => text,
            Err(error) => {
                self.toast_error(error.to_string());
                return;
            }
        };
        let ids = state.selected_ids();
        let workspace_id = state.workspace_id;
        let mut sent = 0usize;
        let mut failed = Vec::new();
        for panel_id in ids {
            let result = self
                .workspaces
                .iter_mut()
                .find(|workspace| workspace.id == workspace_id)
                .ok_or_else(|| {
                    std::io::Error::new(std::io::ErrorKind::NotFound, "El workspace ya no existe.")
                })
                .and_then(|workspace| workspace.submit_prompt_to_ready_panel(panel_id, &text));
            match result {
                Ok(()) => sent += 1,
                Err(error) => failed.push((panel_id, error.to_string())),
            }
        }
        if failed.is_empty() {
            self.broadcast = None;
        } else if let Some(state) = self.broadcast.as_mut() {
            for target in &mut state.targets {
                target.selected = failed.iter().any(|(id, _)| *id == target.panel_id);
            }
        }
        if sent > 0 {
            self.toast_success(format!("Texto encolado en {sent} terminal(es)"));
        }
        if let Some((_, reason)) = failed.first() {
            self.toast_error(format!(
                "{} envío(s) fallaron. Se conservó el borrador: {reason}",
                failed.len()
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use uuid::Uuid;

    use super::{BroadcastState, BroadcastTarget};
    use crate::app::TerminalApp;
    use crate::orchestration::AgentProvider;
    use crate::terminal::{panel::TerminalPanel, pty::PtyHandle};

    fn target(title: &str, alive: bool, selected: bool) -> BroadcastTarget {
        BroadcastTarget {
            panel_id: Uuid::new_v4(),
            leaf_id: Uuid::new_v4(),
            runtime_session_id: Some(Uuid::new_v4()),
            provider_hint: None,
            title: title.to_owned(),
            alive,
            selected,
        }
    }

    fn state(text: &str, targets: Vec<BroadcastTarget>) -> BroadcastState {
        BroadcastState {
            workspace_id: Uuid::new_v4(),
            request_focus: true,
            text: text.to_owned(),
            targets,
            context: None,
            context_error: None,
            include_context: false,
        }
    }

    type Destination = (Uuid, Arc<Mutex<PtyHandle>>);

    fn context_fixture() -> (TerminalApp, Vec<Destination>) {
        let mut app = crate::app::preferences_ui_tests::detached_app();
        let manager = app.workspaces[0].pty_manager();
        manager
            .lock()
            .unwrap()
            .set_remote_spawner(Box::new(|_, _, _, _, wanted| {
                Ok((
                    wanted.unwrap_or_else(Uuid::new_v4),
                    PtyHandle::for_persistence_tests(b""),
                ))
            }));
        let mut destinations = Vec::new();
        for title in [
            "Claude source",
            "Codex target",
            "Gemini target",
            "Plain shell",
        ] {
            let mut panel = TerminalPanel::new(
                egui::Pos2::ZERO,
                egui::vec2(400.0, 240.0),
                egui::Color32::WHITE,
                0,
            );
            panel.rename_title(title.to_owned());
            panel.attach_session_with_spec(
                Arc::clone(&manager),
                None,
                crate::runtime::SessionSpec::default(),
            );
            let handle = manager
                .lock()
                .unwrap()
                .handle(panel.runtime_session_id().unwrap())
                .unwrap();
            destinations.push((panel.id, handle));
            app.workspaces[0].add_restored_terminal(panel);
        }
        app.workspaces[0].bring_to_front(destinations[0].0);
        let source = destinations[0].1.lock().unwrap();
        source.feed_output_for_persistence_tests(b"original selected answer");
        let mut term = source.term.lock().unwrap();
        use alacritty_terminal::index::{Column, Line, Point, Side};
        use alacritty_terminal::selection::{Selection, SelectionType};
        let mut selection = Selection::new(
            SelectionType::Simple,
            Point::new(Line(0), Column(0)),
            Side::Left,
        );
        selection.update(Point::new(Line(0), Column(23)), Side::Right);
        term.selection = Some(selection);
        drop(term);
        drop(source);
        app.open_broadcast();
        (app, destinations)
    }

    #[test]
    fn portable_context_broadcast_requires_explicit_destinations_and_excludes_source_shell() {
        let (mut app, destinations) = context_fixture();
        let state = app.broadcast.as_mut().unwrap();
        assert!(!state.include_context, "selection is opt-in");
        let context = state.context.clone().unwrap();
        assert_eq!(context.selection, "original selected answer");
        assert_eq!(
            context.origin.provider_hint,
            Some(AgentProvider::ClaudeCode)
        );
        assert_eq!(context.origin.panel_id, destinations[0].0);
        state.text = "  Revisá esto con otro proveedor.  ".to_owned();
        state.toggle_context(true);
        assert_eq!(state.selected_count(), 0);
        assert!(!state.can_send());
        state.set_all_alive(true);
        assert_eq!(
            state.selected_ids(),
            vec![destinations[1].0, destinations[2].0]
        );
        let prompt = state.prepare_prompt().unwrap();
        let transfer: serde_json::Value =
            serde_json::from_str(prompt.split_once("\n\n").unwrap().1).unwrap();
        assert_eq!(transfer["request"], state.text);
        assert_eq!(transfer["context"]["selection"], context.selection);
        state.toggle_context(false);
        assert_eq!(state.selected_count(), 0);
        state.set_all_alive(true);
        assert_eq!(
            state.selected_count(),
            4,
            "ordinary broadcast still includes shells"
        );
    }

    #[test]
    fn portable_context_broadcast_retry_keeps_frozen_selection_and_failed_targets() {
        let (mut app, destinations) = context_fixture();
        destinations[2]
            .1
            .lock()
            .unwrap()
            .close_input_for_prompt_tests();
        let state = app.broadcast.as_mut().unwrap();
        state.text = "  Request with whitespace.  ".to_owned();
        state.toggle_context(true);
        state.set_all_alive(true);
        let context = state.context.clone().unwrap();
        let source_revision = destinations[0].1.lock().unwrap().activity_revision();
        // New output and another active workspace must not change the captured
        // context or redirect dispatch to the currently visible workspace.
        destinations[0]
            .1
            .lock()
            .unwrap()
            .feed_output_for_persistence_tests(b"new output");
        app.workspaces
            .push(crate::state::Workspace::new("Other workspace", None));
        app.active_ws = 1;
        app.dispatch_broadcast();
        let state = app.broadcast.as_ref().unwrap();
        assert_eq!(state.context.as_ref(), Some(&context));
        assert_eq!(state.text, "  Request with whitespace.  ");
        assert_eq!(state.selected_ids(), vec![destinations[2].0]);
        assert_eq!(destinations[1].1.lock().unwrap().activity_revision(), 1);
        assert_eq!(destinations[2].1.lock().unwrap().activity_revision(), 0);
        assert_eq!(destinations[3].1.lock().unwrap().activity_revision(), 0);
        assert_eq!(
            destinations[0].1.lock().unwrap().activity_revision(),
            source_revision + 1
        );
        app.dispatch_broadcast();
        assert_eq!(destinations[1].1.lock().unwrap().activity_revision(), 1);
        assert_eq!(
            app.broadcast.as_ref().unwrap().context.as_ref(),
            Some(&context)
        );
    }

    #[test]
    fn portable_context_broadcast_rejects_large_payload_and_retains_draft() {
        let (mut app, destinations) = context_fixture();
        let state = app.broadcast.as_mut().unwrap();
        state.text = "x".repeat(256 * 1024);
        state.toggle_context(true);
        state.set_all_alive(true);
        let context = state.context.clone().unwrap();
        app.dispatch_broadcast();
        let state = app.broadcast.as_ref().unwrap();
        assert_eq!(state.text.len(), 256 * 1024);
        assert_eq!(state.context.as_ref(), Some(&context));
        assert_eq!(state.selected_count(), 2);
        assert_eq!(destinations[1].1.lock().unwrap().activity_revision(), 0);
        assert_eq!(destinations[2].1.lock().unwrap().activity_revision(), 0);
    }

    #[test]
    fn portable_context_broadcast_rejects_replaced_session_with_same_leaf() {
        let (mut app, _destinations) = context_fixture();
        let state = app.broadcast.as_mut().unwrap();
        state.text = "continue".to_owned();
        state.toggle_context(true);
        state.targets[1].selected = true;
        let old_leaf = state.targets[1].leaf_id;
        let old_runtime = state.targets[1].runtime_session_id;
        let manager = app.workspaces[0].pty_manager();
        let workspace_id = app.workspaces[0].id;
        let crate::panel::WorkspacePanel::Terminal(panel) = &mut app.workspaces[0].panels[1];
        assert!(panel.replace_focused_session(
            Arc::clone(&manager),
            None,
            workspace_id,
            "codex".to_owned(),
            "codex".to_owned()
        ));
        assert_eq!(panel.focused_leaf_id(), old_leaf);
        let new_runtime = panel.focused_runtime_session_id();
        assert_ne!(new_runtime, old_runtime);
        let new_handle = manager
            .lock()
            .unwrap()
            .handle(new_runtime.unwrap())
            .unwrap();
        app.dispatch_broadcast();
        assert!(!app.broadcast.as_ref().unwrap().targets[1].alive);
        assert_eq!(new_handle.lock().unwrap().activity_revision(), 0);
        assert!(app.broadcast.as_ref().unwrap().context.is_some());
    }

    #[test]
    fn portable_context_broadcast_rejects_changed_provider_hint() {
        let (mut app, destinations) = context_fixture();
        let state = app.broadcast.as_mut().unwrap();
        state.text = "continue".to_owned();
        state.toggle_context(true);
        state.targets[1].selected = true;
        app.workspaces[0].panels[1].set_title("Gemini replacement".to_owned());
        app.dispatch_broadcast();
        assert!(!app.broadcast.as_ref().unwrap().targets[1].alive);
        assert_eq!(destinations[1].1.lock().unwrap().activity_revision(), 0);
        assert!(app.broadcast.as_ref().unwrap().context.is_some());
    }

    fn broadcast_frame(
        app: &mut TerminalApp,
        ctx: &egui::Context,
        events: Vec<egui::Event>,
    ) -> Vec<(String, egui::Pos2)> {
        let mut output = ctx.run_ui(
            egui::RawInput {
                focused: true,
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1024.0, 768.0),
                )),
                events,
                ..Default::default()
            },
            |ui| app.show_broadcast(ui.ctx()),
        );
        let labels = output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(text) => Some((
                    text.galley.text().to_owned(),
                    text.pos + text.galley.rect.center().to_vec2(),
                )),
                _ => None,
            })
            .collect();
        output.textures_delta.clear();
        output.drop_without_applying_deltas();
        labels
    }

    fn click_events(position: egui::Pos2) -> Vec<egui::Event> {
        vec![
            egui::Event::PointerMoved(position),
            egui::Event::PointerButton {
                pos: position,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
            egui::Event::PointerButton {
                pos: position,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            },
        ]
    }

    #[test]
    fn portable_context_broadcast_ui_selects_context_destination_and_queues_on_send() {
        let (mut app, destinations) = context_fixture();
        app.broadcast.as_mut().unwrap().text = "Review with Codex".to_owned();
        let ctx = egui::Context::default();
        broadcast_frame(&mut app, &ctx, Vec::new());
        let labels = broadcast_frame(&mut app, &ctx, Vec::new());
        let include = labels
            .iter()
            .find(|(text, _)| text.starts_with("Incluir selección"))
            .unwrap()
            .1;
        broadcast_frame(&mut app, &ctx, click_events(include));
        assert!(app.broadcast.as_ref().unwrap().include_context);
        assert_eq!(app.broadcast.as_ref().unwrap().selected_count(), 0);
        let labels = broadcast_frame(&mut app, &ctx, Vec::new());
        let codex = labels
            .iter()
            .find(|(text, _)| text == "Codex target · Codex")
            .unwrap()
            .1;
        broadcast_frame(&mut app, &ctx, click_events(codex));
        assert_eq!(
            app.broadcast.as_ref().unwrap().selected_ids(),
            vec![destinations[1].0]
        );
        let labels = broadcast_frame(&mut app, &ctx, Vec::new());
        let send = labels.iter().find(|(text, _)| text == "Enviar").unwrap().1;
        broadcast_frame(&mut app, &ctx, click_events(send));
        assert!(app.broadcast.is_none());
        assert_eq!(destinations[1].1.lock().unwrap().activity_revision(), 1);
        assert_eq!(destinations[2].1.lock().unwrap().activity_revision(), 0);
        assert_eq!(destinations[3].1.lock().unwrap().activity_revision(), 0);
    }

    #[test]
    fn only_alive_and_selected_panels_receive_the_broadcast() {
        let state = state(
            "ls",
            vec![
                target("alive+on", true, true),
                target("alive+off", true, false),
                target("dead+on", false, true),
                target("dead+off", false, false),
            ],
        );
        let ids = state.selected_ids();
        assert_eq!(ids.len(), 1);
        assert_eq!(ids[0], state.targets[0].panel_id);
    }

    #[test]
    fn dead_panels_do_not_count_even_if_they_stayed_ticked() {
        // Un panel puede morir con el diálogo abierto; su tilde vieja no debe
        // inflar la cuenta ni habilitar el envío.
        let state = state("ls", vec![target("dead", false, true)]);
        assert_eq!(state.selected_count(), 0);
        assert!(!state.can_send());
    }

    #[test]
    fn empty_or_blank_text_cannot_be_sent() {
        let targets = vec![target("alive", true, true)];
        assert!(!state("", targets.clone()).can_send());
        assert!(!state("   \n\t ", targets.clone()).can_send());
        assert!(state("ls", targets).can_send());
    }

    #[test]
    fn sending_requires_at_least_one_target() {
        assert!(!state("ls", vec![target("alive", true, false)]).can_send());
        assert!(!state("ls", Vec::new()).can_send());
    }

    #[test]
    fn select_all_skips_dead_panels() {
        let mut state = state(
            "ls",
            vec![target("alive", true, false), target("dead", false, false)],
        );
        state.set_all_alive(true);
        assert!(state.targets[0].selected);
        assert!(
            !state.targets[1].selected,
            "a dead panel must never be selected"
        );
        assert_eq!(state.selected_count(), 1);
    }

    #[test]
    fn select_none_clears_every_tick() {
        let mut state = state("ls", vec![target("a", true, true), target("b", true, true)]);
        state.set_all_alive(false);
        assert_eq!(state.selected_count(), 0);
        assert!(!state.can_send());
    }

    #[test]
    fn alive_count_ignores_dead_panels() {
        let state = state(
            "ls",
            vec![
                target("a", true, true),
                target("b", true, false),
                target("c", false, false),
            ],
        );
        assert_eq!(state.alive_count(), 2);
        assert_eq!(state.selected_count(), 1);
    }

    #[test]
    fn prompt_delivery_broadcast_retry_preserves_only_failed_destinations() {
        use crate::terminal::{panel::TerminalPanel, pty::PtyHandle};
        use std::sync::Arc;

        let mut app = crate::app::preferences_ui_tests::detached_app();
        let manager = app.workspaces[0].pty_manager();
        manager
            .lock()
            .unwrap()
            .set_remote_spawner(Box::new(|_, _, _, _, wanted| {
                Ok((
                    wanted.unwrap_or_else(Uuid::new_v4),
                    PtyHandle::for_persistence_tests(b""),
                ))
            }));
        let mut destinations = Vec::new();
        for _ in 0..2 {
            let mut panel = TerminalPanel::new(
                egui::Pos2::ZERO,
                egui::vec2(400.0, 240.0),
                egui::Color32::WHITE,
                0,
            );
            panel.attach_session_with_spec(
                Arc::clone(&manager),
                None,
                crate::runtime::SessionSpec::default(),
            );
            let handle = manager
                .lock()
                .unwrap()
                .handle(panel.runtime_session_id().unwrap())
                .unwrap();
            destinations.push((panel.id, handle));
            app.workspaces[0].add_restored_terminal(panel);
        }
        destinations[1]
            .1
            .lock()
            .unwrap()
            .close_input_for_prompt_tests();
        app.open_broadcast();
        app.broadcast.as_mut().unwrap().text = "broadcast draft".to_owned();
        app.dispatch_broadcast();

        let state = app.broadcast.as_ref().unwrap();
        assert_eq!(state.text, "broadcast draft");
        assert_eq!(state.selected_ids(), vec![destinations[1].0]);
        assert_eq!(destinations[0].1.lock().unwrap().activity_revision(), 1);
        assert_eq!(destinations[1].1.lock().unwrap().activity_revision(), 0);

        app.dispatch_broadcast();
        assert_eq!(
            destinations[0].1.lock().unwrap().activity_revision(),
            1,
            "retry must not duplicate accepted input"
        );
        assert_eq!(app.broadcast.as_ref().unwrap().text, "broadcast draft");
    }
}

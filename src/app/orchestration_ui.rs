//! Glue de orquestación de la app: reconciliación de sesiones de agentes con
//! los paneles vivos, refresh periódico de observaciones y el flujo de
//! lanzamiento de agentes (con worktree asíncrono).

use std::collections::HashSet;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

use uuid::Uuid;

use crate::orchestration::{
    AgentLaunchPlan, AgentLaunchRequest, AgentProvider, LaunchOutcome, LaunchPreparation,
    PanelRuntimeObservation, WorktreeMode,
};
use crate::state::{TerminalSpawnRequest, Workspace};

use super::TerminalApp;

pub(super) const ORCHESTRATION_REFRESH_INTERVAL: Duration = Duration::from_millis(750);

enum LaunchSource {
    Manual(Uuid),
    GithubIssue(u64),
    LinearIssue(String),
}

struct MemoryLaunchJob {
    request: AgentLaunchRequest,
    source: LaunchSource,
}

struct MemoryLaunchCompletion {
    request: AgentLaunchRequest,
    repo_root: Option<std::path::PathBuf>,
    source: LaunchSource,
}

pub(super) struct LaunchMemoryWorker {
    jobs: Sender<MemoryLaunchJob>,
    completions: Receiver<MemoryLaunchCompletion>,
    in_flight: usize,
}

impl LaunchMemoryWorker {
    pub(super) fn new() -> Self {
        let (jobs_tx, jobs_rx) = mpsc::channel::<MemoryLaunchJob>();
        let (completion_tx, completion_rx) = mpsc::channel();
        thread::Builder::new()
            .name("launch-memory-worker".to_owned())
            .spawn(move || {
                while let Ok(mut job) = jobs_rx.recv() {
                    job.request.brief = crate::memory::launch_brief_with_memory_scoped(
                        &job.request.brief,
                        job.request.base_cwd.as_deref(),
                        crate::memory::process_store().as_ref(),
                        job.request.task_id,
                        Some(job.request.workspace_id),
                    );
                    let repo_root =
                        crate::orchestration::Orchestrator::resolve_launch_repo(&job.request);
                    if completion_tx
                        .send(MemoryLaunchCompletion {
                            repo_root,
                            request: job.request,
                            source: job.source,
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .expect("launch memory worker");
        Self {
            jobs: jobs_tx,
            completions: completion_rx,
            in_flight: 0,
        }
    }

    fn submit(&mut self, job: MemoryLaunchJob) -> bool {
        if self.jobs.send(job).is_err() {
            return false;
        }
        self.in_flight += 1;
        true
    }

    fn poll(&mut self) -> Vec<MemoryLaunchCompletion> {
        let completions = self.completions.try_iter().collect::<Vec<_>>();
        self.in_flight = self.in_flight.saturating_sub(completions.len());
        completions
    }

    fn in_flight(&self) -> bool {
        self.in_flight > 0
    }
}

#[derive(Clone)]
pub(super) struct LaunchAgentDraft {
    pub(super) workspace_id: Uuid,
    pub(super) provider: AgentProvider,
    pub(super) task_title: String,
    pub(super) brief: String,
    pub(super) worktree_mode: WorktreeMode,
    pub(super) error: Option<String>,
    // El worktree del agente se crea en un worker; mientras tanto el diálogo
    // queda abierto mostrando progreso.
    pub(super) pending_session: Option<Uuid>,
    // El contexto de memoria se resuelve fuera del frame antes de preparar el
    // launch. El id evita que una respuesta vieja lance un diálogo cancelado.
    pub(super) pending_memory: Option<Uuid>,
}

impl TerminalApp {
    pub(super) fn reconcile_orchestration(&mut self) {
        let mut live_panel_ids = HashSet::new();
        for workspace in &self.workspaces {
            for panel in &workspace.panels {
                live_panel_ids.insert(panel.id());
                self.orchestrator.ensure_panel_session(
                    workspace.id,
                    workspace.cwd.clone(),
                    panel.id(),
                    panel.focused_runtime_session_id(),
                    panel.title(),
                );
            }
        }
        self.orchestrator.prune_missing_panels(&live_panel_ids);
        self.prune_panel_keyed_state(&live_panel_ids);
    }

    /// Descarta el estado por panel de los paneles que ya no existen.
    ///
    /// Estos dos mapas se llenaban al abrir un panel y no se vaciaban nunca:
    /// en una sesión larga, cada terminal abierto y cerrado dejaba su entrada
    /// para siempre. Es poca memoria por entrada, pero crecimiento sin cota al
    /// fin y al cabo.
    fn prune_panel_keyed_state(&mut self, live_panel_ids: &HashSet<Uuid>) {
        self.scrollback_restored
            .retain(|panel_id| live_panel_ids.contains(panel_id));
        // `agent_status_seen` va por sesión de runtime, no por panel.
        let live_sessions: HashSet<Uuid> = self
            .workspaces
            .iter()
            .flat_map(|workspace| workspace.panels.iter())
            .flat_map(|panel| panel.all_runtime_session_ids())
            .collect();
        retain_seen_sessions(&mut self.agent_status_seen, &live_sessions);
    }

    pub(super) fn collect_observations(&self) -> Vec<PanelRuntimeObservation> {
        self.workspaces
            .iter()
            .flat_map(Workspace::orchestration_observations)
            .collect()
    }

    pub(super) fn refresh_orchestration(&mut self) {
        let observations = self.collect_observations();
        self.orchestrator.apply_observations(observations);
        self.notify_agent_attention_transitions();
        self.last_orchestration_refresh = Instant::now();
    }

    /// Notifica al SO cuando una sesión de agente pasa a un estado de atención
    /// (esperando aprobación, input, o falló). Solo en la transición, no se
    /// repite mientras siga en el mismo estado.
    fn notify_agent_attention_transitions(&mut self) {
        let sessions: Vec<(
            Uuid,
            crate::orchestration::AgentStatus,
            &'static str,
            String,
        )> = self
            .orchestrator
            .sessions()
            .iter()
            .map(|session| {
                (
                    session.session_id,
                    session.status,
                    session.provider.label(),
                    session.label.clone(),
                )
            })
            .collect();
        let transitions = attention_transitions(&mut self.agent_status_seen, &sessions);
        if transitions.is_empty() {
            return;
        }

        // Marca unread en los paneles que pasan a atención sin estar
        // enfocados (o con la ventana sin foco), y descarta paneles muertos
        // (timers stale) antes de despachar al SO.
        let mut to_notify: Vec<(String, String)> = Vec::new();
        for (session_id, title, body) in transitions {
            let panel_state = self
                .workspaces
                .iter()
                .flat_map(|workspace| workspace.panels.iter())
                .find(|panel| panel.all_runtime_session_ids().contains(&session_id))
                .map(|panel| (panel.focused(), panel.is_alive()));
            let Some((focused, alive)) = panel_state else {
                continue;
            };
            if !alive {
                continue; // panel muerto: no notificar
            }
            if !focused || !self.window_focused {
                for workspace in &mut self.workspaces {
                    for panel in &mut workspace.panels {
                        if panel.all_runtime_session_ids().contains(&session_id) {
                            panel.set_unread(true);
                        }
                    }
                }
            }
            to_notify.push((title, body));
        }

        if !crate::config::runtime_config().agent_notifications {
            return;
        }
        let workspace_id = self.ws().id;
        for (title, body) in to_notify {
            if self
                .notification_gate
                .allow(workspace_id, std::time::Instant::now())
            {
                crate::utils::platform::notify(&title, &body);
            }
        }
    }

    /// Drena los eventos de hooks de agentes y los aplica al orquestador
    /// (P2.12). El estado del hook gana sobre OSC 9999 y la heurística.
    pub(super) fn poll_hook_events(&mut self) {
        let Some(server) = self.hook_server.as_ref() else {
            return;
        };
        let events = server.poll();
        if events.is_empty() {
            return;
        }
        let now = chrono::Utc::now();
        for event in events {
            // El hook dice de qué panel viene; si ese panel ya no existe, se
            // descarta (hook viejo de una sesión cerrada).
            let Some(panel_id) = event.panel_id else {
                continue;
            };
            let Some(alive) = self
                .workspaces
                .iter()
                .flat_map(|workspace| workspace.panels.iter())
                .find(|panel| panel.id() == panel_id)
                .map(|panel| panel.is_alive())
            else {
                continue;
            };
            // El id de sesión que trae el hook se guarda en el panel: con eso
            // el próximo arranque reanuda la conversación exacta (P2.12, T3).
            if let Some(session_id) = event.session_id.clone() {
                for workspace in &mut self.workspaces {
                    for panel in &mut workspace.panels {
                        if panel.id() == panel_id {
                            // Hooks previos a la identidad multi-hoja no
                            // mandaban `leaf`; su metadata pertenecía a la
                            // sesión raíz del panel.
                            let leaf_id = event.leaf_id.unwrap_or_else(|| panel.root_leaf_id());
                            panel.set_agent_session_id_for_leaf(leaf_id, Some(session_id.clone()));
                        }
                    }
                }
            }
            self.orchestrator.apply_hook_event(&event, alive, now);
        }
        self.repaint_policy.note_runtime_event();
    }

    // ----- GitHub in-app vía `gh` (P2.13) -----

    /// Pide (o refresca) PRs e issues del repo del workspace activo.
    pub(super) fn refresh_github_tasks(&mut self, force: bool) {
        let Some(repo_root) = self.ws().cwd.clone() else {
            self.tasks_state.repo_root = None;
            self.tasks_state.availability = None;
            self.tasks_state.snapshot = Default::default();
            self.tasks_state.loading = false;
            return;
        };
        let repo_changed = self.tasks_state.repo_root.as_deref() != Some(repo_root.as_path());
        if repo_changed {
            self.tasks_state.repo_root = Some(repo_root.clone());
            self.tasks_state.availability = None;
            self.tasks_state.snapshot = Default::default();
            self.tasks_state.loading = false;
        }
        self.gh_client
            .request(repo_root.clone(), force || repo_changed);
        self.tasks_state.loading = self.gh_client.is_loading_for(&repo_root);
        // Linear es opt-in: sin token en config.toml el worker ni se toca, y
        // la sección no se dibuja.
        let linear_token = crate::config::runtime_config().linear_token;
        if linear_token.is_some() {
            self.linear_client.request(linear_token, force);
        }
    }

    /// Drena los resultados de los workers de `gh` y Linear hacia la pestaña.
    pub(super) fn poll_gh_client(&mut self) {
        for result in self.gh_client.poll() {
            if self.tasks_state.repo_root.as_deref() != Some(result.repo_root.as_path()) {
                continue;
            }
            self.tasks_state.loading = false;
            self.tasks_state.availability = Some(result.availability);
            self.tasks_state.snapshot = result.snapshot;
        }
        for result in self.linear_client.poll() {
            self.tasks_state.linear_availability = Some(result.availability);
            self.tasks_state.linear_snapshot = result.snapshot;
        }
    }

    /// "Start work" sobre un issue de Linear (P3.17, T2): mismo camino que el
    /// de GitHub, con el identificador legible como nombre de worktree.
    pub(super) fn start_work_on_linear_issue(&mut self, identifier: &str) {
        let Some(issue) = self
            .tasks_state
            .linear_snapshot
            .issues
            .iter()
            .find(|issue| issue.identifier == identifier)
            .cloned()
        else {
            return;
        };
        let request = AgentLaunchRequest {
            workspace_id: self.ws().id,
            task_id: None,
            base_cwd: self.ws().cwd.clone(),
            provider: AgentProvider::ClaudeCode,
            task_title: crate::orchestration::linear_branch_name(&issue.identifier, &issue.title),
            brief: crate::orchestration::linear_prompt(
                &issue.identifier,
                &issue.title,
                &issue.description,
            ),
            worktree_mode: WorktreeMode::Auto,
        };
        self.queue_launch_with_memory(request, LaunchSource::LinearIssue(identifier.to_owned()));
    }

    /// Abre el PR/issue en el navegador con `gh browse`, que resuelve la URL
    /// del remoto sin que tengamos que armarla a mano.
    pub(super) fn open_github_task(&mut self, number: u64) {
        let Some(repo_root) = self.ws().cwd.clone() else {
            return;
        };
        let spawned = std::process::Command::new("gh")
            .current_dir(&repo_root)
            .args(["browse", &number.to_string()])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        if spawned.is_err() {
            self.toast_error("No se pudo abrir con gh");
        }
    }

    /// "Start work" sobre un issue: worktree `issue-<n>-<slug>` y agente
    /// arrancado con el prompt del issue.
    pub(super) fn start_work_on_issue(&mut self, number: u64) {
        let Some(issue) = self
            .tasks_state
            .snapshot
            .issues
            .iter()
            .find(|issue| issue.number == number)
            .cloned()
        else {
            return;
        };
        let workspace_id = self.ws().id;
        let request = AgentLaunchRequest {
            workspace_id,
            task_id: None,
            base_cwd: self.ws().cwd.clone(),
            provider: AgentProvider::ClaudeCode,
            task_title: crate::orchestration::issue_branch_name(number, &issue.title),
            brief: crate::orchestration::issue_prompt(number, &issue.title, &issue.body),
            worktree_mode: WorktreeMode::Auto,
        };
        self.queue_launch_with_memory(request, LaunchSource::GithubIssue(number));
    }

    /// Asocia el issue al panel recién creado, para el badge `#N`.
    pub(super) fn link_issue_to_panel(&mut self, session_id: Uuid, issue: u64) {
        let panel_id = self
            .orchestrator
            .sessions()
            .iter()
            .find(|session| session.session_id == session_id)
            .and_then(|session| session.panel_id);
        let Some(panel_id) = panel_id else {
            return;
        };
        for workspace in &mut self.workspaces {
            for panel in &mut workspace.panels {
                if panel.id() == panel_id {
                    panel.set_linked_issue(Some(issue));
                }
            }
        }
    }

    /// Drena las capturas de Design Mode (P3.18, T3) y las manda al agente
    /// enfocado con el formato determinístico.
    pub(super) fn poll_design_captures(&mut self) {
        let Some(server) = self.hook_server.as_ref() else {
            return;
        };
        let captures = server.poll_design();
        if captures.is_empty() {
            return;
        }
        let Some(panel_id) = self
            .ws()
            .panels
            .iter()
            .find(|panel| panel.focused() && panel.is_alive())
            .map(|panel| panel.id())
        else {
            self.toast_error("Elemento capturado, pero no hay agente enfocado");
            return;
        };
        let mut delivered = 0usize;
        for capture in captures {
            let prompt = crate::orchestration::format_design_capture(&capture);
            for workspace in &mut self.workspaces {
                if workspace.send_prompt_to_panel(panel_id, &prompt) {
                    delivered += 1;
                    break;
                }
            }
        }
        if delivered > 0 {
            self.toast_success(if delivered == 1 {
                "Elemento capturado".to_owned()
            } else {
                format!("{delivered} elementos capturados")
            });
        }
    }

    pub(super) fn maybe_refresh_orchestration(&mut self) {
        if self.panel_gesture.is_some() {
            return;
        }
        if self.last_orchestration_refresh.elapsed() >= ORCHESTRATION_REFRESH_INTERVAL {
            self.reconcile_orchestration();
            let started_at = Instant::now();
            self.refresh_orchestration();
            self.last_orchestration_scan_duration = started_at.elapsed();
        }
    }
    pub(super) fn open_launch_agent_dialog(&mut self) {
        let workspace_id = self.ws().id;
        self.launch_agent = Some(LaunchAgentDraft {
            workspace_id,
            provider: AgentProvider::ClaudeCode,
            task_title: "".to_owned(),
            brief: "".to_owned(),
            worktree_mode: WorktreeMode::Auto,
            error: None,
            pending_session: None,
            pending_memory: None,
        });
    }
    pub(super) fn submit_launch_agent(&mut self, ctx: &egui::Context) {
        let Some(draft) = self.launch_agent.clone() else {
            return;
        };
        let request = AgentLaunchRequest {
            workspace_id: draft.workspace_id,
            task_id: None,
            base_cwd: self
                .workspace_index_by_id(draft.workspace_id)
                .and_then(|index| self.workspaces.get(index))
                .and_then(|workspace| workspace.cwd.clone()),
            provider: draft.provider,
            task_title: draft.task_title.clone(),
            brief: draft.brief.clone(),
            worktree_mode: draft.worktree_mode,
        };
        let request_id = Uuid::new_v4();
        if let Some(current) = self.launch_agent.as_mut() {
            current.error = None;
            current.pending_memory = Some(request_id);
        }
        if !self.queue_launch_with_memory(request, LaunchSource::Manual(request_id)) {
            if let Some(current) = self.launch_agent.as_mut() {
                current.pending_memory = None;
                current.error = Some("No se pudo iniciar el worker de memoria".to_owned());
            }
        } else {
            ctx.request_repaint_after(Duration::from_millis(16));
        }
    }

    pub(super) fn poll_pending_launches(&mut self, ctx: &egui::Context) {
        self.poll_memory_launches(ctx);
        if self.orchestrator.has_pending_launches() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        for outcome in self.orchestrator.poll_ready_launches() {
            match outcome {
                LaunchOutcome::Ready(plan) => {
                    let spawned = self.spawn_agent_panel(ctx, &plan);
                    // Si este launch venía de un "Start work" sobre un issue,
                    // recién ahora existe el panel al que pegarle el badge.
                    if spawned {
                        if let Some(issue) = self.pending_issue_links.remove(&plan.session_id) {
                            self.link_issue_to_panel(plan.session_id, issue);
                        }
                    }
                    let matches_draft = self
                        .launch_agent
                        .as_ref()
                        .is_some_and(|draft| draft.pending_session == Some(plan.session_id));
                    if matches_draft {
                        if spawned {
                            self.launch_agent = None;
                        } else if let Some(draft) = self.launch_agent.as_mut() {
                            draft.pending_session = None;
                        }
                    }
                }
                LaunchOutcome::Failed { session_id, error } => {
                    log::warn!("agent worktree creation failed: {error}");
                    if let Some(draft) = self.launch_agent.as_mut() {
                        if draft.pending_session == Some(session_id) {
                            draft.pending_session = None;
                            draft.error = Some(format!("No se pudo crear el worktree: {error}"));
                        }
                    }
                }
            }
        }
    }

    fn queue_launch_with_memory(
        &mut self,
        request: AgentLaunchRequest,
        source: LaunchSource,
    ) -> bool {
        self.launch_memory_worker
            .submit(MemoryLaunchJob { request, source })
    }

    fn poll_memory_launches(&mut self, ctx: &egui::Context) {
        for completion in self.launch_memory_worker.poll() {
            if self
                .workspace_index_by_id(completion.request.workspace_id)
                .is_none()
            {
                continue;
            }
            if let LaunchSource::Manual(request_id) = &completion.source {
                let current = self
                    .launch_agent
                    .as_ref()
                    .and_then(|draft| draft.pending_memory);
                if current != Some(*request_id) {
                    continue;
                }
                if let Some(draft) = self.launch_agent.as_mut() {
                    draft.pending_memory = None;
                }
            }

            let preparation = self
                .orchestrator
                .prepare_launch_with_repo(completion.request, completion.repo_root);
            match (completion.source, preparation) {
                (LaunchSource::Manual(_), Ok(LaunchPreparation::Ready(plan))) => {
                    if self.spawn_agent_panel(ctx, &plan) {
                        self.launch_agent = None;
                    }
                }
                (
                    LaunchSource::Manual(_),
                    Ok(LaunchPreparation::PendingWorktree { session_id }),
                ) => {
                    if let Some(draft) = self.launch_agent.as_mut() {
                        draft.error = None;
                        draft.pending_session = Some(session_id);
                    }
                }
                (LaunchSource::Manual(_), Err(err)) => {
                    if let Some(draft) = self.launch_agent.as_mut() {
                        draft.error = Some(err.to_string());
                    }
                }
                (LaunchSource::GithubIssue(number), Ok(LaunchPreparation::Ready(plan))) => {
                    if self.spawn_agent_panel(ctx, &plan) {
                        self.link_issue_to_panel(plan.session_id, number);
                    }
                }
                (
                    LaunchSource::GithubIssue(number),
                    Ok(LaunchPreparation::PendingWorktree { session_id }),
                ) => {
                    self.pending_issue_links.insert(session_id, number);
                }
                (LaunchSource::GithubIssue(number), Err(err)) => {
                    self.toast_error(format!("No se pudo arrancar el issue #{number}: {err}"));
                }
                (LaunchSource::LinearIssue(_), Ok(LaunchPreparation::Ready(plan))) => {
                    self.spawn_agent_panel(ctx, &plan);
                }
                (LaunchSource::LinearIssue(_), Ok(LaunchPreparation::PendingWorktree { .. })) => {}
                (LaunchSource::LinearIssue(identifier), Err(err)) => {
                    self.toast_error(format!("No se pudo arrancar {identifier}: {err}"));
                }
            }
        }
        if self.launch_memory_worker.in_flight() {
            ctx.request_repaint_after(Duration::from_millis(16));
        }
    }

    /// Crea el panel de terminal para un plan de agente listo. Devuelve false
    /// si el workspace destino ya no existe.
    pub(super) fn spawn_agent_panel(
        &mut self,
        ctx: &egui::Context,
        plan: &AgentLaunchPlan,
    ) -> bool {
        let Some(workspace_index) = self.workspace_index_by_id(plan.workspace_id) else {
            if let Some(current) = self.launch_agent.as_mut() {
                current.error = Some("Workspace not found".to_owned());
            }
            return false;
        };
        let spawned = {
            let workspace = &mut self.workspaces[workspace_index];
            workspace.spawn_terminal_with_request(
                ctx,
                TerminalSpawnRequest {
                    title: Some(plan.panel_title.clone()),
                    cwd: plan.cwd.clone(),
                    startup_command: plan.startup_command.clone(),
                    startup_input: plan.startup_input.clone(),
                },
            )
        };
        self.orchestrator.bind_launch_to_panel(
            plan.session_id,
            spawned.panel_id,
            spawned.runtime_session_id,
        );
        self.switch_workspace(workspace_index);
        self.ws_mut().bring_to_front(spawned.panel_id);
        self.reconcile_orchestration();
        self.refresh_orchestration();
        true
    }
}

/// Dada la historia de estados vistos y los estados actuales, devuelve
/// `(title, body)` para cada sesión que TRANSICIONÓ a un estado de atención.
/// Actualiza `seen` como efecto lateral. Pura y testeable (sin notificar).
/// Descarta el estado de sesiones que ya no existen.
///
/// `attention_transitions` inserta una entrada por cada sesión que observa y
/// nunca borra: sin esta poda el mapa crece durante toda la vida del proceso.
fn retain_seen_sessions(
    seen: &mut std::collections::HashMap<Uuid, crate::orchestration::AgentStatus>,
    live_sessions: &HashSet<Uuid>,
) {
    seen.retain(|session_id, _| live_sessions.contains(session_id));
}

fn attention_transitions(
    seen: &mut std::collections::HashMap<Uuid, crate::orchestration::AgentStatus>,
    sessions: &[(
        Uuid,
        crate::orchestration::AgentStatus,
        &'static str,
        String,
    )],
) -> Vec<(Uuid, String, String)> {
    use crate::orchestration::AgentStatus;
    let mut out = Vec::new();
    for (session_id, status, provider_label, label) in sessions {
        let previous = seen.get(session_id).copied();
        seen.insert(*session_id, *status);
        if previous == Some(*status) {
            continue;
        }
        let attention = matches!(
            status,
            AgentStatus::WaitingApproval | AgentStatus::NeedsInput | AgentStatus::Failed
        );
        if !attention {
            continue;
        }
        let title = format!("Agente: {provider_label}");
        let body = if label.trim().is_empty() {
            status.label().to_owned()
        } else {
            format!("{} — {}", label, status.label())
        };
        out.push((*session_id, title, body));
    }
    out
}

#[cfg(test)]
mod attention_tests {
    use std::collections::HashMap;

    use uuid::Uuid;

    use super::attention_transitions;
    use crate::orchestration::AgentStatus;

    fn session(
        id: Uuid,
        status: AgentStatus,
        label: &str,
    ) -> (Uuid, AgentStatus, &'static str, String) {
        (id, status, "Claude Code", label.to_owned())
    }

    #[test]
    fn notifies_on_transition_into_attention() {
        let mut seen = HashMap::new();
        let id = Uuid::new_v4();
        let notifications = attention_transitions(
            &mut seen,
            &[session(id, AgentStatus::WaitingApproval, "Fix bug")],
        );
        assert_eq!(notifications.len(), 1);
        assert_eq!(notifications[0].1, "Agente: Claude Code");
        assert!(notifications[0].2.contains("Fix bug"));
    }

    #[test]
    fn does_not_repeat_while_in_same_attention_state() {
        let mut seen = HashMap::new();
        let id = Uuid::new_v4();
        let first =
            attention_transitions(&mut seen, &[session(id, AgentStatus::NeedsInput, "Task")]);
        assert_eq!(first.len(), 1);
        let second =
            attention_transitions(&mut seen, &[session(id, AgentStatus::NeedsInput, "Task")]);
        assert!(second.is_empty(), "no debe repetir en el mismo estado");
    }

    #[test]
    fn ignores_non_attention_states() {
        let mut seen = HashMap::new();
        let id = Uuid::new_v4();
        let notifications =
            attention_transitions(&mut seen, &[session(id, AgentStatus::Running, "Task")]);
        assert!(notifications.is_empty());
    }

    #[test]
    fn notifies_again_on_transition_to_different_attention_state() {
        let mut seen = HashMap::new();
        let id = Uuid::new_v4();
        let _ = attention_transitions(&mut seen, &[session(id, AgentStatus::WaitingApproval, "T")]);
        let again = attention_transitions(&mut seen, &[session(id, AgentStatus::Failed, "T")]);
        assert_eq!(
            again.len(),
            1,
            "transición a otro estado de atención notifica"
        );
    }
    #[test]
    fn seen_state_stays_bounded_when_sessions_come_and_go() {
        // Regresión de fuga: sin poda, cada sesión observada dejaba su entrada
        // para siempre. Simulamos 500 sesiones efímeras con una sola viva.
        use std::collections::HashSet;

        let survivor = Uuid::new_v4();
        let mut seen = HashMap::new();

        for _ in 0..500 {
            let ephemeral = Uuid::new_v4();
            let batch = vec![
                session(survivor, AgentStatus::Running, "vive"),
                session(ephemeral, AgentStatus::Running, "efimera"),
            ];
            attention_transitions(&mut seen, &batch);

            // La sesión efímera se cierra; sólo sobrevive la otra.
            let live: HashSet<Uuid> = [survivor].into_iter().collect();
            super::retain_seen_sessions(&mut seen, &live);
        }

        assert_eq!(
            seen.len(),
            1,
            "the map must not grow with sessions that already died"
        );
        assert!(seen.contains_key(&survivor));
    }

    #[test]
    fn pruning_keeps_every_live_session() {
        use std::collections::HashSet;

        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let mut seen = HashMap::new();
        attention_transitions(
            &mut seen,
            &[
                session(a, AgentStatus::Running, "a"),
                session(b, AgentStatus::Idle, "b"),
            ],
        );

        let live: HashSet<Uuid> = [a, b].into_iter().collect();
        super::retain_seen_sessions(&mut seen, &live);
        assert_eq!(seen.len(), 2, "live sessions must never be dropped");
    }
}

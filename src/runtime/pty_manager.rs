use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use uuid::Uuid;

use crate::terminal::input::{agent_prompt_bytes, paste_bytes, sanitize_agent_prompt};
use crate::terminal::pty::PtyHandle;

use super::SessionSpec;

pub type SharedPtyHandle = Arc<Mutex<PtyHandle>>;
pub type SharedRuntimeScheduler = Arc<Mutex<RuntimeScheduler>>;
const DEFAULT_UI_BATCH_LIMIT: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RuntimeSessionUpdate {
    pub session_id: Uuid,
    pub output: bool,
    pub title_changed: bool,
    pub bell: bool,
    pub exited: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UiUpdateBatch {
    pub session_updates: Vec<RuntimeSessionUpdate>,
    pub repaint_requested: bool,
}

#[derive(Default)]
pub struct RuntimeScheduler {
    pending: HashMap<Uuid, RuntimeSessionUpdate>,
    repaint_queued: bool,
    max_batch_size: usize,
}

#[derive(Default)]
pub struct PtyManager {
    sessions: HashMap<Uuid, ManagedSession>,
    scheduler: SharedRuntimeScheduler,
}

struct ManagedSession {
    spec: SessionSpec,
    handle: Option<SharedPtyHandle>,
    detached_alive: bool,
    pending_startup_input: Option<PendingStartupInput>,
    /// Prompt interactivo (feedback del code review) diferido hasta que el
    /// TUI renderice algo; solo se usa cuando el panel se acaba de spawnear.
    pending_prompt: Option<PendingStartupInput>,
}

struct PendingStartupInput {
    input: String,
    baseline_render_revision: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SchedulerUpdateKind {
    Output,
    TitleChanged,
    Bell,
    Exited,
    Render,
}

impl ManagedSession {
    fn detached(spec: SessionSpec) -> Self {
        Self {
            spec,
            handle: None,
            detached_alive: true,
            pending_startup_input: None,
            pending_prompt: None,
        }
    }

    fn is_alive(&self) -> bool {
        if let Some(handle) = &self.handle {
            handle
                .lock()
                .ok()
                .map(|handle| handle.alive())
                .unwrap_or(false)
        } else {
            self.detached_alive
        }
    }

    fn title_snapshot(&self) -> Option<String> {
        if let Some(handle) = &self.handle {
            if let Ok(handle) = handle.lock() {
                if let Some(title) = handle.title_snapshot() {
                    return Some(title);
                }
            }
        }

        Some(self.spec.title.clone())
    }

    fn is_attached(&self) -> bool {
        self.handle.is_some()
    }
}

impl PendingStartupInput {
    fn is_ready(&self, current_render_revision: u64) -> bool {
        current_render_revision > self.baseline_render_revision
    }
}

impl RuntimeScheduler {
    pub fn new() -> Self {
        Self {
            pending: HashMap::new(),
            repaint_queued: false,
            max_batch_size: DEFAULT_UI_BATCH_LIMIT,
        }
    }

    #[allow(dead_code)]
    pub fn new_for_tests() -> Self {
        Self::new()
    }

    #[allow(dead_code)]
    pub fn with_batch_limit_for_tests(max_batch_size: usize) -> Self {
        Self {
            max_batch_size: max_batch_size.max(1),
            ..Self::new()
        }
    }

    #[allow(dead_code)]
    pub fn enqueue_output_batch(&mut self, sessions: usize, updates_per_session: usize) {
        for session_index in 0..sessions {
            let session_id = Uuid::from_u128((session_index + 1) as u128);
            for _ in 0..updates_per_session {
                self.record_output(session_id);
            }
        }
    }

    pub fn record_output(&mut self, session_id: Uuid) {
        self.mark_session(session_id, SchedulerUpdateKind::Output);
    }

    pub fn record_title_changed(&mut self, session_id: Uuid) {
        self.mark_session(session_id, SchedulerUpdateKind::TitleChanged);
    }

    pub fn record_bell(&mut self, session_id: Uuid) {
        self.mark_session(session_id, SchedulerUpdateKind::Bell);
    }

    pub fn record_exit(&mut self, session_id: Uuid) {
        self.mark_session(session_id, SchedulerUpdateKind::Exited);
    }

    pub fn record_render(&mut self, session_id: Uuid) {
        self.mark_session(session_id, SchedulerUpdateKind::Render);
    }

    pub fn drain_ui_updates(&mut self) -> UiUpdateBatch {
        let repaint_requested = self.repaint_queued || !self.pending.is_empty();
        let keys = self
            .pending
            .keys()
            .copied()
            .take(self.max_batch_size)
            .collect::<Vec<_>>();
        let mut session_updates = keys
            .into_iter()
            .filter_map(|session_id| self.pending.remove(&session_id))
            .collect::<Vec<_>>();
        session_updates.sort_by_key(|update| update.session_id.as_u128());
        self.repaint_queued = !self.pending.is_empty();
        UiUpdateBatch {
            session_updates,
            repaint_requested: repaint_requested || self.repaint_queued,
        }
    }

    fn mark_session(&mut self, session_id: Uuid, kind: SchedulerUpdateKind) {
        let update = self
            .pending
            .entry(session_id)
            .or_insert_with(|| RuntimeSessionUpdate {
                session_id,
                ..Default::default()
            });
        match kind {
            SchedulerUpdateKind::Output | SchedulerUpdateKind::Render => {
                update.output = true;
            }
            SchedulerUpdateKind::TitleChanged => {
                update.title_changed = true;
            }
            SchedulerUpdateKind::Bell => {
                update.bell = true;
            }
            SchedulerUpdateKind::Exited => {
                update.exited = true;
            }
        }
        self.request_repaint_once();
    }

    fn request_repaint_once(&mut self) {
        if self.repaint_queued {
            return;
        }
        self.repaint_queued = true;
    }
}

impl PtyManager {
    pub fn new() -> Self {
        Self {
            sessions: HashMap::new(),
            scheduler: Arc::new(Mutex::new(RuntimeScheduler::new())),
        }
    }

    #[allow(dead_code)]
    pub fn new_for_tests() -> Self {
        Self::new()
    }

    pub fn create_detached(&mut self, spec: SessionSpec) -> Uuid {
        let session_id = Uuid::new_v4();
        self.sessions
            .insert(session_id, ManagedSession::detached(spec));
        session_id
    }

    pub fn spawn(
        &mut self,
        spec: SessionSpec,
        cwd: Option<&Path>,
        cols: u16,
        rows: u16,
    ) -> anyhow::Result<Uuid> {
        let session_id = Uuid::new_v4();
        let detached_spec = SessionSpec {
            title: spec.title.clone(),
            cwd: spec.cwd.clone().or_else(|| cwd.map(Path::to_path_buf)),
            startup_command: spec.startup_command.clone(),
            startup_input: spec.startup_input.clone(),
        };
        self.sessions
            .insert(session_id, ManagedSession::detached(detached_spec));
        if let Err(err) = self.attach_detached(session_id, cols, rows) {
            self.sessions.remove(&session_id);
            return Err(err);
        }
        Ok(session_id)
    }

    pub fn attach_detached(
        &mut self,
        session_id: Uuid,
        cols: u16,
        rows: u16,
    ) -> anyhow::Result<()> {
        let Some(session) = self.sessions.get_mut(&session_id) else {
            anyhow::bail!("Runtime session not found");
        };
        if session.is_attached() {
            return Ok(());
        }

        let spec = session.spec.clone();
        let handle = PtyHandle::spawn(
            spec.cwd.as_deref(),
            cols,
            rows,
            session_id,
            Arc::clone(&self.scheduler),
        )?;
        if let Some(command) = spec
            .startup_command
            .as_deref()
            .map(str::trim)
            .filter(|command| !command.is_empty())
        {
            handle.write_all(format!("{command}\n").as_bytes());
        }
        let shared_handle = Arc::new(Mutex::new(handle));
        if let Some(input) = spec
            .startup_input
            .as_deref()
            .map(str::trim)
            .filter(|input| !input.is_empty())
            .map(str::to_owned)
        {
            let baseline_render_revision = shared_handle
                .lock()
                .ok()
                .map(|handle| handle.render_revision())
                .unwrap_or(0);
            if spec
                .startup_command
                .as_deref()
                .map(str::trim)
                .filter(|command| !command.is_empty())
                .is_some()
            {
                session.pending_startup_input = Some(PendingStartupInput {
                    input,
                    baseline_render_revision,
                });
            } else if let Ok(handle) = shared_handle.lock() {
                write_startup_input(&handle, &input);
            }
        }
        session.handle = Some(shared_handle);
        session.detached_alive = false;
        Ok(())
    }

    pub fn handle(&self, session_id: Uuid) -> Option<SharedPtyHandle> {
        self.sessions
            .get(&session_id)
            .and_then(|session| session.handle.as_ref().map(Arc::clone))
    }

    pub fn session_title(&self, session_id: Uuid) -> Option<String> {
        self.sessions.get(&session_id)?.title_snapshot()
    }

    pub fn update_spec_title(&mut self, session_id: Uuid, title: String) {
        if let Some(session) = self.sessions.get_mut(&session_id) {
            session.spec.title = title;
        }
    }

    pub fn is_alive(&self, session_id: Uuid) -> bool {
        self.sessions
            .get(&session_id)
            .map(ManagedSession::is_alive)
            .unwrap_or(false)
    }

    pub fn is_attached(&self, session_id: Uuid) -> bool {
        self.sessions
            .get(&session_id)
            .map(ManagedSession::is_attached)
            .unwrap_or(false)
    }

    pub fn attached_session_count(&self) -> usize {
        self.sessions
            .values()
            .filter(|session| session.is_attached())
            .count()
    }

    pub fn detached_session_count(&self) -> usize {
        self.sessions
            .values()
            .filter(|session| !session.is_attached() && session.is_alive())
            .count()
    }

    pub fn drain_ui_updates(&mut self) -> UiUpdateBatch {
        self.flush_pending_startup_inputs();
        self.scheduler
            .lock()
            .ok()
            .map(|mut scheduler| scheduler.drain_ui_updates())
            .unwrap_or_default()
    }

    pub fn close(&mut self, session_id: Uuid) -> bool {
        self.sessions.remove(&session_id).is_some()
    }

    /// Encola un prompt interactivo diferido: se escribe recién cuando el
    /// render revision avanza (el TUI renderizó algo), para no inyectar en un
    /// agente que todavía está arrancando.
    pub fn queue_prompt(&mut self, session_id: Uuid, text: &str) {
        let Some(session) = self.sessions.get_mut(&session_id) else {
            return;
        };
        let Some(handle) = session.handle.as_ref() else {
            return;
        };
        let baseline = handle
            .lock()
            .ok()
            .map(|handle| handle.render_revision())
            .unwrap_or(0);
        session.pending_prompt = Some(PendingStartupInput {
            input: text.to_owned(),
            baseline_render_revision: baseline,
        });
    }

    fn flush_pending_startup_inputs(&mut self) {
        for session in self.sessions.values_mut() {
            Self::flush_one_pending(session, true);
            Self::flush_one_pending(session, false);
        }
    }

    /// Flushea `pending_startup_input` (is_startup=true) o `pending_prompt`
    /// (false) si el handle está listo. Devuelve el input escrito, si hubo.
    fn flush_one_pending(session: &mut ManagedSession, is_startup: bool) {
        let pending_ref = if is_startup {
            session.pending_startup_input.as_ref()
        } else {
            session.pending_prompt.as_ref()
        };
        let Some(pending) = pending_ref else {
            return;
        };
        let Some(handle) = session.handle.as_ref() else {
            return;
        };
        // Hold a single lock across the readiness check and the write so
        // the handle cannot change state between the two.
        let Ok(handle) = handle.lock() else {
            return;
        };
        if !pending.is_ready(handle.render_revision()) {
            return;
        }
        let taken = if is_startup {
            session.pending_startup_input.take()
        } else {
            session.pending_prompt.take()
        };
        let Some(pending) = taken else {
            return;
        };
        if is_startup {
            write_startup_input(&handle, &pending.input);
        } else {
            write_prompt_input(&handle, &pending.input);
        }
    }
}

/// Inyecta el prompt inicial en el agente (idea de orca): neutraliza bytes
/// de escape para que el brief no pueda emitir secuencias de control, y si
/// el TUI ya activó bracketed paste lo envía como paste atómico para que un
/// brief multi-línea no se ejecute línea por línea.
fn write_startup_input(handle: &PtyHandle, input: &str) {
    let sanitized = sanitize_agent_prompt(input);
    let mode = handle.input_mode();
    let mut bytes = paste_bytes(&sanitized, &mode);
    bytes.push(b'\n');
    handle.write_all(&bytes);
}

/// Prompt interactivo (feedback): igual que el startup pero submit con `\r`
/// (la tecla Enter real), consistente con `agent_prompt_bytes`.
fn write_prompt_input(handle: &PtyHandle, input: &str) {
    let bytes = agent_prompt_bytes(input, &handle.input_mode());
    handle.write_all(&bytes);
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::{PendingStartupInput, PtyManager};
    use crate::runtime::SessionSpec;

    #[test]
    fn pending_startup_input_waits_for_render_revision_to_advance() {
        let pending = PendingStartupInput {
            input: "prompt".to_owned(),
            baseline_render_revision: 4,
        };

        assert!(!pending.is_ready(4));
        assert!(pending.is_ready(5));
    }

    #[test]
    fn queue_prompt_on_missing_session_is_noop() {
        let mut manager = PtyManager::new_for_tests();
        manager.queue_prompt(Uuid::new_v4(), "hello");
        assert_eq!(manager.attached_session_count(), 0);
    }

    #[test]
    fn queue_prompt_on_detached_session_without_handle_is_noop() {
        let mut manager = PtyManager::new_for_tests();
        let session_id = manager.create_detached(SessionSpec::default());
        // Sin handle todavía: no se puede diferir (necesita el render
        // revision del PTY), no debe paniquear ni adjuntar.
        manager.queue_prompt(session_id, "hello");
        assert!(!manager.is_attached(session_id));
    }
}

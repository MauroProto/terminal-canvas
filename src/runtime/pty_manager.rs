use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use uuid::Uuid;

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
        }
    }

    fn attached(spec: SessionSpec, handle: SharedPtyHandle) -> Self {
        Self {
            spec,
            handle: Some(handle),
            detached_alive: false,
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

impl RuntimeScheduler {
    pub fn new() -> Self {
        Self {
            pending: HashMap::new(),
            repaint_queued: false,
            max_batch_size: DEFAULT_UI_BATCH_LIMIT,
        }
    }

    pub fn new_for_tests() -> Self {
        Self::new()
    }

    pub fn with_batch_limit_for_tests(max_batch_size: usize) -> Self {
        Self {
            max_batch_size: max_batch_size.max(1),
            ..Self::new()
        }
    }

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
        ctx: &egui::Context,
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
        if let Err(err) = self.attach_detached(ctx, session_id, cols, rows) {
            self.sessions.remove(&session_id);
            return Err(err);
        }
        Ok(session_id)
    }

    pub fn attach_detached(
        &mut self,
        ctx: &egui::Context,
        session_id: Uuid,
        cols: u16,
        rows: u16,
    ) -> anyhow::Result<()> {
        let _ = ctx;
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
            let shared_handle_clone = Arc::clone(&shared_handle);
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(600));
                if let Ok(handle) = shared_handle_clone.lock() {
                    handle.write_all(format!("{input}\n").as_bytes());
                }
            });
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

    pub fn drain_ui_updates(&self) -> UiUpdateBatch {
        self.scheduler
            .lock()
            .ok()
            .map(|mut scheduler| scheduler.drain_ui_updates())
            .unwrap_or_default()
    }

    pub fn close(&mut self, session_id: Uuid) -> bool {
        self.sessions.remove(&session_id).is_some()
    }
}

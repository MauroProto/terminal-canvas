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
const BACKGROUND_READER_DELAY: std::time::Duration = std::time::Duration::from_millis(2);

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
    /// Sesión del panel enfocado (P3.16, T2): tiene carril propio y se drena
    /// antes que el resto, para que tipear nunca se sienta atrás de un panel
    /// de fondo escupiendo salida.
    priority_session: Option<Uuid>,
}

/// Orden de drenado del frame: primero la sesión prioritaria (si tiene algo
/// pendiente), después el resto hasta completar el presupuesto. Puro para
/// poder testear la política sin montar PTYs.
pub fn drain_order(pending: &[Uuid], priority: Option<Uuid>, max_batch: usize) -> Vec<Uuid> {
    if max_batch == 0 {
        return Vec::new();
    }
    let mut out: Vec<Uuid> = Vec::with_capacity(max_batch.min(pending.len()));
    if let Some(priority) = priority {
        if pending.contains(&priority) {
            out.push(priority);
        }
    }
    let mut rest: Vec<Uuid> = pending
        .iter()
        .copied()
        .filter(|id| Some(*id) != priority)
        .collect();
    // Orden estable: sin esto el batch cambia de composición entre frames por
    // el orden aleatorio del HashMap.
    rest.sort_by_key(Uuid::as_u128);
    for id in rest {
        if out.len() >= max_batch {
            break;
        }
        out.push(id);
    }
    out
}

#[derive(Default)]
pub struct PtyManager {
    sessions: HashMap<Uuid, ManagedSession>,
    scheduler: SharedRuntimeScheduler,
    /// Crea la sesión en el daemon en vez de in-process (P3.15, T3).
    ///
    /// Se inyecta desde afuera a propósito: así el runtime no depende del
    /// módulo del daemon (que es unix-only y va detrás de un feature), y los
    /// harness de tests que montan este archivo por `#[path]` no necesitan
    /// arrastrar todo el árbol del daemon.
    remote_spawner: Option<RemoteSpawner>,
}

/// Crea una sesión fuera del proceso y devuelve su id y su handle.
pub type RemoteSpawner = Box<
    dyn Fn(
            &SessionSpec,
            u16,
            u16,
            SharedRuntimeScheduler,
            Option<Uuid>,
        ) -> anyhow::Result<(Uuid, PtyHandle)>
        + Send
        + Sync,
>;

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
            priority_session: None,
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

    /// Declara qué sesión tiene el foco, para darle carril propio (P3.16).
    pub fn set_priority_session(&mut self, session_id: Option<Uuid>) {
        self.priority_session = session_id;
    }

    /// Presupuesto de I/O del lector, no sólo orden de notificaciones. Cuando
    /// hay una sesión enfocada, los lectores de fondo ceden brevemente CPU y
    /// dejan que el kernel les aplique contrapresión; el lector interactivo no
    /// se retrasa.
    pub fn reader_delay(&self, session_id: Uuid) -> std::time::Duration {
        if self
            .priority_session
            .is_some_and(|priority| priority != session_id)
        {
            BACKGROUND_READER_DELAY
        } else {
            std::time::Duration::ZERO
        }
    }

    pub fn drain_ui_updates(&mut self) -> UiUpdateBatch {
        let repaint_requested = self.repaint_queued || !self.pending.is_empty();
        let pending_ids = self.pending.keys().copied().collect::<Vec<_>>();
        let keys = drain_order(&pending_ids, self.priority_session, self.max_batch_size);
        let session_updates = keys
            .into_iter()
            .filter_map(|session_id| self.pending.remove(&session_id))
            .collect::<Vec<_>>();
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
            remote_spawner: None,
        }
    }

    #[allow(dead_code)]
    pub fn new_for_tests() -> Self {
        Self::new()
    }

    pub fn create_detached(&mut self, spec: SessionSpec) -> Uuid {
        self.create_detached_with_id(spec, None)
    }

    /// Crea una sesión detached con un id preexistente si se da (P3.15, T4):
    /// así un panel restaurado puede pedirle al daemon **su** sesión de la
    /// corrida anterior en vez de una nueva.
    pub fn create_detached_with_id(&mut self, spec: SessionSpec, existing: Option<Uuid>) -> Uuid {
        let session_id = existing
            .filter(|id| !self.sessions.contains_key(id))
            .unwrap_or_else(Uuid::new_v4);
        self.sessions
            .insert(session_id, ManagedSession::detached(spec));
        session_id
    }

    /// Instala el creador de sesiones fuera del proceso (P3.15, T3).
    pub fn set_remote_spawner(&mut self, spawner: RemoteSpawner) {
        self.remote_spawner = Some(spawner);
    }

    /// ¿Las sesiones nuevas se crean fuera del proceso?
    pub fn hosts_out_of_process(&self) -> bool {
        self.remote_spawner.is_some()
    }

    pub fn spawn(
        &mut self,
        spec: SessionSpec,
        cwd: Option<&Path>,
        cols: u16,
        rows: u16,
    ) -> anyhow::Result<Uuid> {
        // Con el daemon adoptado el PTY se crea allá: cerrar la app no lo mata.
        if let Some(spawner) = self.remote_spawner.as_ref() {
            let mut spec_with_cwd = spec.clone();
            if spec_with_cwd.cwd.is_none() {
                spec_with_cwd.cwd = cwd.map(Path::to_path_buf);
            }
            match spawner(
                &spec_with_cwd,
                cols,
                rows,
                Arc::clone(&self.scheduler),
                None,
            ) {
                Ok((session_id, handle)) => {
                    let mut managed = ManagedSession::detached(spec_with_cwd);
                    let shared_handle = Arc::new(Mutex::new(handle));
                    Self::configure_startup_input(&mut managed, &shared_handle);
                    managed.handle = Some(shared_handle);
                    managed.detached_alive = false;
                    self.sessions.insert(session_id, managed);
                    return Ok(session_id);
                }
                Err(err) => {
                    // Fallback in-process: mejor un terminal que muere con la
                    // app que ningún terminal.
                    log::warn!("el daemon no pudo hostear la sesión ({err}); se usa in-process");
                }
            }
        }
        let session_id = Uuid::new_v4();
        let detached_spec = SessionSpec {
            title: spec.title.clone(),
            cwd: spec.cwd.clone().or_else(|| cwd.map(Path::to_path_buf)),
            startup_command: spec.startup_command.clone(),
            startup_input: spec.startup_input.clone(),
            // El fallback local sigue siendo la misma sesión lógica. Perder
            // su identidad rompe hooks, diagnóstico y aislamiento por
            // proyecto precisamente cuando el daemon degrada.
            panel_id: spec.panel_id,
            workspace_id: spec.workspace_id,
            leaf_id: spec.leaf_id,
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
            if session.is_alive() {
                return Ok(());
            }
            let was_remote = session
                .handle
                .as_ref()
                .and_then(|handle| handle.lock().ok())
                .is_some_and(|handle| handle.is_remote());
            if !was_remote {
                // Un proceso local que terminó no se relanza a espaldas del
                // usuario. La recuperación automática es sólo del transporte
                // remoto, donde el PTY puede seguir vivo en el daemon.
                return Ok(());
            }
            session.handle = None;
        }

        let spec = session.spec.clone();
        // Con daemon adoptado, una sesión detached se engancha **allá** con su
        // mismo id (P3.15, T3): es el camino que toman los paneles restaurados,
        // y sin esto se atacheaban in-process aunque el daemon estuviera vivo.
        let remote = self.remote_spawner.as_ref().and_then(|spawner| {
            match spawner(
                &spec,
                cols,
                rows,
                Arc::clone(&self.scheduler),
                Some(session_id),
            ) {
                Ok((id, handle)) if id == session_id => Some(handle),
                Ok((id, _)) => {
                    // El daemon dio otro id: no se puede mapear panel ↔ sesión,
                    // así que se cae a in-process en vez de perder el panel.
                    log::warn!("el daemon devolvió {id} en vez de {session_id}");
                    None
                }
                Err(err) => {
                    log::warn!("el daemon no pudo enganchar {session_id} ({err})");
                    None
                }
            }
        });

        let (handle, was_remote) = match remote {
            Some(handle) => (handle, true),
            None => (
                PtyHandle::spawn(
                    spec.cwd.as_deref(),
                    cols,
                    rows,
                    session_id,
                    Arc::clone(&self.scheduler),
                    crate::terminal::pty::HookIdentity {
                        panel_id: spec.panel_id,
                        workspace_id: spec.workspace_id,
                        leaf_id: spec.leaf_id,
                    },
                )?,
                false,
            ),
        };
        // El daemon ya corrió el startup_command al crear la sesión: repetirlo
        // acá lanzaría el agente dos veces.
        if !was_remote {
            if let Some(command) = spec
                .startup_command
                .as_deref()
                .map(str::trim)
                .filter(|command| !command.is_empty())
            {
                handle.write_all(format!("{command}\n").as_bytes());
            }
        }
        let shared_handle = Arc::new(Mutex::new(handle));
        Self::configure_startup_input(session, &shared_handle);
        session.handle = Some(shared_handle);
        session.detached_alive = false;
        Ok(())
    }

    fn configure_startup_input(session: &mut ManagedSession, handle: &SharedPtyHandle) {
        let spec = &session.spec;
        if let Some(input) = spec
            .startup_input
            .as_deref()
            .map(str::trim)
            .filter(|input| !input.is_empty())
            .map(str::to_owned)
        {
            let baseline_render_revision = handle
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
            } else if let Ok(handle) = handle.lock() {
                write_startup_input(&handle, &input);
            }
        }
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

    #[cfg(test)]
    pub fn startup_command_for_tests(&self, session_id: Uuid) -> Option<&str> {
        self.sessions
            .get(&session_id)?
            .spec
            .startup_command
            .as_deref()
    }

    /// Declara la sesión del panel enfocado para el carril interactivo.
    pub fn set_priority_session(&mut self, session_id: Option<Uuid>) {
        if let Ok(mut scheduler) = self.scheduler.lock() {
            scheduler.set_priority_session(session_id);
        }
    }

    pub fn drain_ui_updates(&mut self) -> UiUpdateBatch {
        self.flush_pending_startup_inputs();
        self.scheduler
            .lock()
            .ok()
            .map(|mut scheduler| scheduler.drain_ui_updates())
            .unwrap_or_default()
    }

    /// Suelta la sesión de este proceso. Si vive en el daemon **no** la mata:
    /// este es el camino que corre también al cerrar la app, y matar acá sería
    /// exactamente lo contrario de lo que el daemon promete (P3.15).
    pub fn close(&mut self, session_id: Uuid) -> bool {
        self.sessions.remove(&session_id).is_some()
    }

    /// Cierra la sesión **para siempre**: la suelta y, si vive en el daemon,
    /// la mata. Es el camino del usuario cerrando un panel.
    pub fn close_and_kill_remote(&mut self, session_id: Uuid) -> bool {
        let Some(session) = self.sessions.remove(&session_id) else {
            return false;
        };
        #[cfg(not(all(unix, feature = "daemon")))]
        let _ = session;
        #[cfg(all(unix, feature = "daemon"))]
        if let Some(handle) = session.handle.as_ref() {
            if let Ok(pty) = handle.lock() {
                pty.kill_remote_session();
            }
        }
        true
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

    use super::{PendingStartupInput, PtyManager, RuntimeScheduler};
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

    #[test]
    fn focused_session_gets_the_unthrottled_io_lane() {
        let focused = Uuid::new_v4();
        let background = Uuid::new_v4();
        let mut scheduler = RuntimeScheduler::new_for_tests();
        scheduler.set_priority_session(Some(focused));

        assert!(scheduler.reader_delay(focused).is_zero());
        assert!(!scheduler.reader_delay(background).is_zero());
        scheduler.set_priority_session(None);
        assert!(scheduler.reader_delay(background).is_zero());
    }
}

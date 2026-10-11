use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use uuid::Uuid;

use crate::terminal::input::{agent_prompt_bytes, paste_bytes, sanitize_agent_prompt};
use crate::terminal::pty::PtyHandle;

use super::DeferredPrompt;
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
    remote_async: Option<RemoteQueue>,
    local_backend: bool,
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

/// A protected recovery refusal always carries the identity that must survive.
#[derive(Clone)]
pub struct RemoteRecoveryError {
    pub session_id: Uuid,
    pub safe_local_fallback: bool,
    pub message: String,
    pub closer: Option<RemoteCloser>,
    pub retry_intent: RemoteSpawnIntent,
    pub retryable: bool,
    pub may_close: bool,
}

impl std::fmt::Debug for RemoteRecoveryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RemoteRecoveryError")
            .field("session_id", &self.session_id)
            .field("safe_local_fallback", &self.safe_local_fallback)
            .field("message", &self.message)
            .finish_non_exhaustive()
    }
}

impl RemoteRecoveryError {
    pub fn protected(session_id: Uuid, message: impl Into<String>) -> Self {
        Self {
            session_id,
            safe_local_fallback: false,
            message: message.into(),
            closer: None,
            retry_intent: RemoteSpawnIntent::AdoptExisting,
            retryable: true,
            may_close: true,
        }
    }

    pub fn birth_pending(session_id: Uuid, message: impl Into<String>, idempotent: bool) -> Self {
        let mut error = Self::protected(session_id, message);
        error.retry_intent = RemoteSpawnIntent::NewBirth;
        error.retryable = idempotent;
        error.may_close = idempotent;
        error
    }

    pub fn birth_rejected(session_id: Uuid, message: impl Into<String>) -> Self {
        let mut error = Self::birth_pending(session_id, message, false);
        error.retryable = false;
        error.may_close = false;
        error
    }

    pub fn with_closer(mut self, closer: RemoteCloser) -> Self {
        self.closer = Some(closer);
        self
    }

    pub fn before_creation(session_id: Uuid, message: impl Into<String>) -> Self {
        Self {
            session_id,
            safe_local_fallback: true,
            message: message.into(),
            closer: None,
            retry_intent: RemoteSpawnIntent::NewBirth,
            retryable: true,
            may_close: false,
        }
    }
}

impl std::fmt::Display for RemoteRecoveryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for RemoteRecoveryError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteSpawnIntent {
    NewBirth,
    AdoptExisting,
}

pub struct RemoteSpawnResult {
    pub session_id: Uuid,
    pub handle: PtyHandle,
    pub output_ready: bool,
    pub owned_birth: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteCloseIntent {
    OwnedBirth,
    AdoptExisting,
}

pub type AsyncRemoteSpawner = Arc<
    dyn Fn(
            &SessionSpec,
            u16,
            u16,
            SharedRuntimeScheduler,
            Uuid,
            RemoteSpawnIntent,
        ) -> anyhow::Result<RemoteSpawnResult>
        + Send
        + Sync,
>;
pub type RemoteCloser = Arc<dyn Fn(Uuid, RemoteCloseIntent) -> anyhow::Result<()> + Send + Sync>;

const REMOTE_QUEUE_CAPACITY: usize = 32;
const REMOTE_QUEUE_BYTES: usize = 8 * 1024 * 1024;
const REMOTE_RETRY_DELAY: std::time::Duration = std::time::Duration::from_secs(2);
const REMOTE_CLOSE_RETRY: std::time::Duration = std::time::Duration::from_secs(1);
const REMOTE_DETACHED: u8 = 1;
const REMOTE_CLOSED: u8 = 2;

struct RemoteLease {
    id: Uuid,
    generation: Uuid,
    retired: std::sync::atomic::AtomicU8,
    pending: std::sync::atomic::AtomicBool,
    local_only: std::sync::atomic::AtomicBool,
    local_retiring: Mutex<Option<SharedPtyHandle>>,
    may_close: std::sync::atomic::AtomicBool,
    attempted: std::sync::atomic::AtomicBool,
    may_have_spawned: std::sync::atomic::AtomicBool,
    local_birth_admission: std::sync::atomic::AtomicU8,
    birth: std::sync::atomic::AtomicBool,
    dimensions: Mutex<(u16, u16)>,
    applied_dimensions: Mutex<Option<(u16, u16)>>,
    completion: Mutex<Option<RemoteCompletion>>,
    cleanup_after: Mutex<std::time::Instant>,
}

impl RemoteLease {
    fn new(id: Uuid, cols: u16, rows: u16) -> Arc<Self> {
        Arc::new(Self {
            id,
            generation: Uuid::new_v4(),
            retired: std::sync::atomic::AtomicU8::new(0),
            pending: std::sync::atomic::AtomicBool::new(false),
            local_only: std::sync::atomic::AtomicBool::new(false),
            local_retiring: Mutex::new(None),
            may_close: std::sync::atomic::AtomicBool::new(true),
            attempted: std::sync::atomic::AtomicBool::new(false),
            may_have_spawned: std::sync::atomic::AtomicBool::new(false),
            local_birth_admission: std::sync::atomic::AtomicU8::new(0),
            birth: std::sync::atomic::AtomicBool::new(false),
            dimensions: Mutex::new((cols.max(1), rows.max(1))),
            applied_dimensions: Mutex::new(None),
            completion: Mutex::new(None),
            cleanup_after: Mutex::new(std::time::Instant::now()),
        })
    }

    fn close_local_birth_admissions(&self) {
        // bit0 = already-admitted factory; bit1 = permanently closed.
        self.local_birth_admission
            .fetch_or(2, std::sync::atomic::Ordering::AcqRel);
    }

    fn admit_local_birth(&self) -> bool {
        if self.retirement() != 0 {
            return false;
        }
        self.local_birth_admission
            .compare_exchange(
                0,
                1,
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
            )
            .is_ok()
    }

    fn finish_local_birth(&self) {
        self.local_birth_admission
            .fetch_and(!1, std::sync::atomic::Ordering::AcqRel);
    }

    fn close_intent(&self) -> RemoteCloseIntent {
        if self.birth.load(std::sync::atomic::Ordering::Acquire) {
            RemoteCloseIntent::OwnedBirth
        } else {
            RemoteCloseIntent::AdoptExisting
        }
    }

    fn cleanup_eligible(&self) -> bool {
        self.may_close.load(std::sync::atomic::Ordering::Acquire)
            || !self.attempted.load(std::sync::atomic::Ordering::Acquire)
            || self.local_only.load(std::sync::atomic::Ordering::Acquire)
    }

    fn retirement(&self) -> u8 {
        self.retired.load(std::sync::atomic::Ordering::Acquire)
    }
}

struct RemoteCompletion {
    result: anyhow::Result<RemoteSpawnResult>,
    dimensions: (u16, u16),
}

type CleanupBarrier = Vec<(Uuid, std::sync::Weak<RemoteLease>)>;
static REMOTE_CLEANUP_BARRIER: std::sync::OnceLock<Mutex<CleanupBarrier>> =
    std::sync::OnceLock::new();

fn cleanup_pending(backend: Uuid, id: Uuid) -> bool {
    let mut barrier = REMOTE_CLEANUP_BARRIER
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    barrier.retain(|(_, lease)| lease.strong_count() > 0);
    barrier.iter().any(|(owner, lease)| {
        *owner == backend
            && lease
                .upgrade()
                .is_some_and(|lease| lease.id == id && lease.retirement() == REMOTE_CLOSED)
    })
}

fn remote_spec_bytes(spec: &SessionSpec) -> Option<usize> {
    let wire = spec
        .title
        .len()
        .checked_add(spec.cwd.as_ref().map_or(0, |path| path.as_os_str().len()))?
        .checked_add(spec.startup_command.as_ref().map_or(0, String::len))?;
    let input = spec.startup_input.as_ref().map_or(0, String::len);
    // Wire text can expand sixfold in JSON; prompts are not part of WireSpec.
    if wire > 512 * 1024 || input > crate::terminal::pty::MAX_INPUT_BYTES {
        return None;
    }
    wire.checked_add(input)
}

struct RemoteJob {
    lease: Arc<RemoteLease>,
    spec: SessionSpec,
    scheduler: SharedRuntimeScheduler,
    intent: RemoteSpawnIntent,
    prepared: Option<RemoteSpawnResult>,
    attached_resize: Option<SharedPtyHandle>,
}

#[derive(Default)]
struct RemoteQueueState {
    ready: std::collections::VecDeque<RemoteJob>,
    ready_bytes: usize,
    leases: Vec<Arc<RemoteLease>>,
    stopping: bool,
}

type SharedRemoteQueue = Arc<(Mutex<RemoteQueueState>, std::sync::Condvar)>;

struct RemoteQueue {
    state: SharedRemoteQueue,
    backend_id: Uuid,
    spawn: AsyncRemoteSpawner,
    close: RemoteCloser,
    started: bool,
}

impl RemoteQueue {
    fn new(backend_id: Uuid, spawn: AsyncRemoteSpawner, close: RemoteCloser) -> Self {
        Self {
            state: Arc::new((
                Mutex::new(RemoteQueueState::default()),
                std::sync::Condvar::new(),
            )),
            backend_id,
            spawn,
            close,
            started: false,
        }
    }

    fn register(&mut self, lease: Arc<RemoteLease>) {
        let mut state = self
            .state
            .0
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if !state.leases.iter().any(|old| Arc::ptr_eq(old, &lease)) {
            state.leases.push(lease);
        }
    }

    fn ensure_started(&mut self) -> anyhow::Result<()> {
        if !self.started {
            let state = Arc::clone(&self.state);
            let spawn = Arc::clone(&self.spawn);
            let close = Arc::clone(&self.close);
            std::thread::Builder::new()
                .name("remote-recovery".to_owned())
                .spawn(move || remote_worker(state, spawn, close))?;
            self.started = true;
        }
        Ok(())
    }

    fn submit(&mut self, job: RemoteJob) -> anyhow::Result<()> {
        self.ensure_started()?;
        let mut state = self
            .state
            .0
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let bytes = remote_spec_bytes(&job.spec).ok_or_else(|| {
            RemoteRecoveryError::birth_rejected(
                job.lease.id,
                "La configuración de la sesión supera el límite permitido.",
            )
        })?;
        if cleanup_pending(self.backend_id, job.lease.id) {
            anyhow::bail!(RemoteRecoveryError::protected(
                job.lease.id,
                "Esperando la confirmación del cierre anterior."
            ));
        }
        if state.stopping
            || state.ready.len() >= REMOTE_QUEUE_CAPACITY
            || state.ready_bytes.saturating_add(bytes) > REMOTE_QUEUE_BYTES
        {
            anyhow::bail!(RemoteRecoveryError::protected(
                job.lease.id,
                "La recuperación está ocupada; se reintentará."
            ));
        }
        if job.lease.retirement() != 0
            || job
                .lease
                .pending
                .swap(true, std::sync::atomic::Ordering::AcqRel)
        {
            anyhow::bail!(RemoteRecoveryError::protected(
                job.lease.id,
                "La recuperación de esta sesión ya está pendiente."
            ));
        }
        state.ready_bytes += bytes;
        state.ready.push_back(job);
        self.state.1.notify_one();
        Ok(())
    }

    fn has_cleanup(&self) -> bool {
        let state = self
            .state
            .0
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        state
            .leases
            .iter()
            .any(|lease| lease.retirement() == REMOTE_CLOSED && lease.cleanup_eligible())
    }

    fn retire(&mut self, lease: &Arc<RemoteLease>, kill: bool) {
        // Publication and retirement share the completion boundary. Drop any
        // ready handle before a potentially long cleanup retry can retain it.
        let completion = {
            let mut completion = lease
                .completion
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            lease.retired.store(
                if kill { REMOTE_CLOSED } else { REMOTE_DETACHED },
                std::sync::atomic::Ordering::Release,
            );
            lease.close_local_birth_admissions();
            completion.take()
        };
        if let Some(completion) = completion {
            retain_local_completion(lease, completion);
        }
        if kill {
            REMOTE_CLEANUP_BARRIER
                .get_or_init(|| Mutex::new(Vec::new()))
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push((self.backend_id, Arc::downgrade(lease)));
        }
        if kill {
            let _ = self.ensure_started();
        }
        self.state.1.notify_one();
    }
}

impl Drop for RemoteQueue {
    fn drop(&mut self) {
        let mut state = self
            .state
            .0
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        state.stopping = true;
        for lease in &state.leases {
            lease.close_local_birth_admissions();
            let _ = lease.retired.compare_exchange(
                0,
                REMOTE_DETACHED,
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
            );
        }
        self.state.1.notify_one();
    }
}

fn remote_worker(shared: SharedRemoteQueue, spawn: AsyncRemoteSpawner, close: RemoteCloser) {
    enum Work {
        Attach(Box<RemoteJob>),
        Close(Arc<RemoteLease>),
    }
    let mut prefer_attach = false;
    loop {
        let work = {
            let mut state = shared.0.lock().unwrap_or_else(|error| error.into_inner());
            loop {
                state.leases.retain(|lease| {
                    lease.retirement() != REMOTE_DETACHED
                        || lease.pending.load(std::sync::atomic::Ordering::Acquire)
                });
                let now = std::time::Instant::now();
                let closing = state
                    .leases
                    .iter()
                    .find(|lease| {
                        lease.retirement() == REMOTE_CLOSED
                            && lease.cleanup_eligible()
                            && !lease.pending.load(std::sync::atomic::Ordering::Acquire)
                            && *lease
                                .cleanup_after
                                .lock()
                                .unwrap_or_else(|error| error.into_inner())
                                <= now
                    })
                    .cloned();
                if !state.ready.is_empty() && (prefer_attach || closing.is_none()) {
                    let job = state.ready.pop_front().expect("ready queue was checked");
                    state.ready_bytes = state
                        .ready_bytes
                        .saturating_sub(remote_spec_bytes(&job.spec).unwrap_or(0));
                    prefer_attach = false;
                    break Work::Attach(Box::new(job));
                }
                if let Some(lease) = closing {
                    prefer_attach = true;
                    break Work::Close(lease);
                }
                let delay = state
                    .leases
                    .iter()
                    .filter(|lease| {
                        lease.retirement() == REMOTE_CLOSED
                            && lease.cleanup_eligible()
                            && !lease.pending.load(std::sync::atomic::Ordering::Acquire)
                    })
                    .filter_map(|lease| {
                        lease
                            .cleanup_after
                            .lock()
                            .ok()
                            .map(|due| due.saturating_duration_since(now))
                    })
                    .min();
                // Retired cleanups outlive a dropped workspace/backend. They
                // continue with bounded retry I/O while this process is alive.
                if state.stopping && delay.is_none() {
                    return;
                }
                state = if let Some(delay) = delay {
                    shared
                        .1
                        .wait_timeout(state, delay)
                        .unwrap_or_else(|error| error.into_inner())
                        .0
                } else {
                    shared
                        .1
                        .wait(state)
                        .unwrap_or_else(|error| error.into_inner())
                };
            }
        };
        match work {
            Work::Close(lease) => {
                let local_only = lease.local_only.load(std::sync::atomic::Ordering::Acquire)
                    || !lease.attempted.load(std::sync::atomic::Ordering::Acquire);
                let result = if local_only {
                    handoff_local_retirement(&lease)
                } else {
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        close(lease.id, lease.close_intent())
                    }))
                    .unwrap_or_else(|_| Err(anyhow::anyhow!("La limpieza remota falló.")))
                };
                if result.is_ok() {
                    lease
                        .retired
                        .store(REMOTE_DETACHED, std::sync::atomic::Ordering::Release);
                    let mut state = shared.0.lock().unwrap_or_else(|error| error.into_inner());
                    state.leases.retain(|old| !Arc::ptr_eq(old, &lease));
                } else {
                    *lease
                        .cleanup_after
                        .lock()
                        .unwrap_or_else(|error| error.into_inner()) =
                        std::time::Instant::now() + REMOTE_CLOSE_RETRY;
                }
            }
            Work::Attach(job) => perform_remote_job(*job, &spawn),
        }
    }
}

fn normalize_birth_uncertainty(
    lease: &RemoteLease,
    result: &mut anyhow::Result<RemoteSpawnResult>,
) {
    let already_uncertain = lease
        .may_have_spawned
        .load(std::sync::atomic::Ordering::Acquire);
    if let Err(error) = result {
        let mut typed = error
            .downcast_ref::<RemoteRecoveryError>()
            .cloned()
            .unwrap_or_else(|| {
                RemoteRecoveryError::birth_pending(lease.id, error.to_string(), true)
            });
        if already_uncertain {
            typed.safe_local_fallback = false;
            typed.retry_intent = RemoteSpawnIntent::NewBirth;
            typed.may_close |= lease.may_close.load(std::sync::atomic::Ordering::Acquire);
        }
        if typed.retry_intent == RemoteSpawnIntent::NewBirth
            && !typed.safe_local_fallback
            && (typed.retryable || typed.may_close)
        {
            lease
                .may_have_spawned
                .store(true, std::sync::atomic::Ordering::Release);
        }
        *error = typed.into();
    } else if result.as_ref().is_ok_and(|value| value.owned_birth) {
        lease
            .may_have_spawned
            .store(true, std::sync::atomic::Ordering::Release);
    }
}

fn perform_remote_job(job: RemoteJob, spawn: &AsyncRemoteSpawner) {
    perform_remote_job_with_fallback(job, spawn, &spawn_local_handle)
}

fn perform_remote_job_with_fallback(
    mut job: RemoteJob,
    spawn: &AsyncRemoteSpawner,
    local_factory: &impl Fn(
        &SessionSpec,
        u16,
        u16,
        Uuid,
        SharedRuntimeScheduler,
    ) -> anyhow::Result<PtyHandle>,
) {
    if job.lease.retirement() != 0 {
        job.lease
            .pending
            .store(false, std::sync::atomic::Ordering::Release);
        return;
    }
    let (cols, rows) = *job
        .lease
        .dimensions
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if let Some(handle) = job.attached_resize.take() {
        if job.lease.retirement() == 0 {
            if let Ok(mut handle) = handle.lock() {
                handle.resize(cols, rows);
                *job.lease
                    .applied_dimensions
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()) = Some((cols, rows));
            }
        }
        job.lease
            .pending
            .store(false, std::sync::atomic::Ordering::Release);
        if let Ok(mut scheduler) = job.scheduler.lock() {
            scheduler.record_render(job.lease.id);
        }
        return;
    }
    let mut result = if let Some(prepared) = job.prepared.take() {
        Ok(prepared)
    } else {
        job.lease
            .attempted
            .store(true, std::sync::atomic::Ordering::Release);
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            spawn(
                &job.spec,
                cols,
                rows,
                Arc::clone(&job.scheduler),
                job.lease.id,
                job.intent,
            )
        }))
        .unwrap_or_else(|_| {
            Err(RemoteRecoveryError::birth_pending(
                job.lease.id,
                "La recuperación remota falló después de una admisión potencial.",
                true,
            )
            .into())
        })
    };
    normalize_birth_uncertainty(&job.lease, &mut result);
    if result
        .as_ref()
        .err()
        .and_then(|error| error.downcast_ref::<RemoteRecoveryError>())
        .is_some_and(|error| job.intent == RemoteSpawnIntent::NewBirth && error.safe_local_fallback)
    {
        let mut local_started = false;
        if job.lease.admit_local_birth() {
            // Close closes this admission atomically; a factory admitted first
            // stays pending until it returns and its result is dropped/published.
            if job.lease.retirement() == 0 {
                local_started = true;
                job.lease
                    .local_only
                    .store(true, std::sync::atomic::Ordering::Release);
                result = local_factory(
                    &job.spec,
                    cols,
                    rows,
                    job.lease.id,
                    Arc::clone(&job.scheduler),
                )
                .map(|handle| RemoteSpawnResult {
                    session_id: job.lease.id,
                    output_ready: handle.activity_revision() > 0,
                    owned_birth: false,
                    handle,
                });
            }
            job.lease.finish_local_birth();
        }
        if !local_started {
            // A canceled KnownNotCreated attempt needs no remote close ACK.
            job.lease
                .attempted
                .store(false, std::sync::atomic::Ordering::Release);
        }
    }
    if result
        .as_ref()
        .is_ok_and(|result| result.session_id != job.lease.id)
    {
        result = Err(RemoteRecoveryError::birth_pending(
            job.lease.id,
            "El daemon devolvió una identidad distinta tras una admisión potencial.",
            true,
        )
        .into());
        normalize_birth_uncertainty(&job.lease, &mut result);
    }
    if let Some(error) = result
        .as_ref()
        .err()
        .and_then(|error| error.downcast_ref::<RemoteRecoveryError>())
    {
        job.lease
            .may_close
            .store(error.may_close, std::sync::atomic::Ordering::Release);
        if error.retry_intent == RemoteSpawnIntent::NewBirth {
            job.lease
                .birth
                .store(true, std::sync::atomic::Ordering::Release);
        }
    }
    let dimensions = *job
        .lease
        .dimensions
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if let Ok(result) = &mut result {
        job.lease
            .birth
            .store(result.owned_birth, std::sync::atomic::Ordering::Release);
        job.lease.local_only.store(
            !result.handle.is_remote(),
            std::sync::atomic::Ordering::Release,
        );
        job.lease
            .may_close
            .store(true, std::sync::atomic::Ordering::Release);
        result.handle.resize(dimensions.0, dimensions.1);
        *job.lease
            .applied_dimensions
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(dimensions);
    }
    {
        let mut completion = job
            .lease
            .completion
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if job.lease.retirement() == 0 {
            *completion = Some(RemoteCompletion { result, dimensions });
            if let Ok(mut scheduler) = job.scheduler.lock() {
                scheduler.record_render(job.lease.id);
            }
        } else {
            retain_local_completion(&job.lease, RemoteCompletion { result, dimensions });
        }
    }
    job.lease
        .pending
        .store(false, std::sync::atomic::Ordering::Release);
}

fn retain_local_completion(lease: &RemoteLease, completion: RemoteCompletion) {
    if let Ok(result) = completion.result {
        if !result.handle.is_remote() {
            lease
                .local_only
                .store(true, std::sync::atomic::Ordering::Release);
            *lease
                .local_retiring
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(Mutex::new(result.handle)));
            lease
                .retired
                .store(REMOTE_CLOSED, std::sync::atomic::Ordering::Release);
        }
    }
}
fn handoff_local_retirement(lease: &RemoteLease) -> anyhow::Result<()> {
    let mut retained = lease
        .local_retiring
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if let Some(handle) = retained.as_ref() {
        crate::hook_runtime::process_runtime()
            .ok_or_else(|| anyhow::anyhow!("local authority unavailable"))?
            .request_local_close(Arc::clone(handle))?;
        retained.take(); // worker now owns the handle until aggregate final ACK.
    }
    Ok(())
}

fn spawn_local_handle(
    spec: &SessionSpec,
    cols: u16,
    rows: u16,
    id: Uuid,
    scheduler: SharedRuntimeScheduler,
) -> anyhow::Result<PtyHandle> {
    let handle = PtyHandle::spawn(
        spec.cwd.as_deref(),
        cols,
        rows,
        id,
        scheduler,
        crate::terminal::pty::HookIdentity {
            panel_id: spec.panel_id,
            workspace_id: spec.workspace_id,
            leaf_id: spec.leaf_id,
            memory_task_id: spec.memory_task_id,
            hook_generation: None,
            hook_owner: None,
        },
    )?;
    if let Some(command) = spec
        .startup_command
        .as_deref()
        .map(str::trim)
        .filter(|command| !command.is_empty())
    {
        if let Err(error) = handle.try_write_all(format!("{command}\n").as_bytes()) {
            if let Some(runtime) = crate::hook_runtime::process_runtime() {
                runtime.request_local_close_from_factory(Arc::new(Mutex::new(handle)));
            }
            return Err(error.into());
        }
    }
    Ok(handle)
}

struct RemoteRecoveryState {
    lease: Arc<RemoteLease>,
    next_retry: std::time::Instant,
    fresh: bool,
    retryable: bool,
    startup_delivered: bool,
    error: Option<String>,
}

struct ManagedSession {
    spec: SessionSpec,
    handle: Option<SharedPtyHandle>,
    detached_alive: bool,
    pending_startup_input: Option<PendingStartupInput>,
    /// Prompt interactivo (feedback del code review) diferido hasta que el
    /// TUI renderice algo; solo se usa cuando el panel se acaba de spawnear.
    pending_prompt: Option<PendingStartupInput>,
    remote_recovery: Option<RemoteRecoveryState>,
    legacy_closer: Option<RemoteCloser>,
}

struct PendingStartupInput {
    defer_until_remote_publication: bool,
    input: String,
    /// None means readiness was already observed; capacity retries need no output.
    baseline_render_revision: Option<u64>,
    completion: Option<(
        std::sync::mpsc::Sender<std::io::Result<()>>,
        Arc<std::sync::atomic::AtomicBool>,
    )>,
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
            remote_recovery: None,
            legacy_closer: None,
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
        self.baseline_render_revision
            .is_none_or(|baseline| current_render_revision > baseline)
    }

    fn try_send(
        &mut self,
        current_render_revision: u64,
        send: impl FnOnce(&str) -> std::io::Result<()>,
    ) -> std::io::Result<bool> {
        if !self.is_ready(current_render_revision) {
            return Ok(false);
        }
        self.baseline_render_revision = None;
        match send(&self.input) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(false),
            Err(error) => Err(error),
        }
    }
}

/// Count the sanitized bytes and maximum paste framing before allocating a copy.
fn prompt_fits_input_budget(input: &str) -> bool {
    let limit = crate::terminal::pty::MAX_INPUT_BYTES;
    input.len() <= limit
        && input.len() + input.bytes().filter(|byte| *byte == 0x1b).count() * 4 + 13 <= limit
}

const OVERSIZED_PROMPT_ERROR: &str =
    "No se envió el prompt: el texto preparado supera el límite de 2 MiB. Dividilo en partes más pequeñas.";

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
        let mut manager = Self {
            sessions: HashMap::new(),
            scheduler: Arc::new(Mutex::new(RuntimeScheduler::new())),
            remote_spawner: None,
            remote_async: None,
            local_backend: false,
        };
        if let Some(runtime) = crate::hook_runtime::process_runtime() {
            manager.set_remote_backend(
                runtime.owner,
                Arc::new(|spec, cols, rows, scheduler, id, _intent| {
                    spawn_local_handle(spec, cols, rows, id, scheduler).map(|handle| {
                        RemoteSpawnResult {
                            session_id: id,
                            output_ready: handle.activity_revision() > 0,
                            owned_birth: false,
                            handle,
                        }
                    })
                }),
                Arc::new(|_id, _intent| {
                    if crate::hook_runtime::process_runtime()
                        .is_some_and(|runtime| runtime.local_close_completed(_id))
                    {
                        Ok(())
                    } else {
                        Err(anyhow::anyhow!("Local aggregate closure still pending."))
                    }
                }),
            );
            manager.local_backend = true;
        }
        manager
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
        let mut session = ManagedSession::detached(spec);
        if let Some(queue) = self.remote_async.as_mut() {
            let lease = RemoteLease::new(session_id, 80, 24);
            queue.register(Arc::clone(&lease));
            session.remote_recovery = Some(RemoteRecoveryState {
                lease,
                next_retry: std::time::Instant::now(),
                fresh: false,
                retryable: true,
                startup_delivered: true,
                error: None,
            });
        }
        self.sessions.insert(session_id, session);
        session_id
    }

    /// Instala el creador de sesiones fuera del proceso (P3.15, T3).
    pub fn set_remote_spawner(&mut self, spawner: RemoteSpawner) {
        self.remote_async = None;
        for session in self.sessions.values_mut() {
            session.remote_recovery = None;
        }
        self.remote_spawner = Some(spawner);
    }

    /// Production recovery runs outside the UI; the synchronous setter stays
    /// available for existing callers and in-memory harnesses.
    pub fn set_remote_backend(
        &mut self,
        backend_id: Uuid,
        spawner: AsyncRemoteSpawner,
        closer: RemoteCloser,
    ) {
        // Bootstrap installs this override before the first worker is started.
        if self.local_backend && self.remote_async.as_ref().is_some_and(|q| !q.started) {
            self.remote_async = None;
            self.local_backend = false;
        }
        if let Some(queue) = self.remote_async.as_ref() {
            if queue.backend_id != backend_id {
                log::warn!("El backend remoto activo conserva su propietario original.");
            }
            return;
        }
        self.remote_spawner = None;
        let mut queue = RemoteQueue::new(backend_id, spawner, closer);
        for (id, session) in &mut self.sessions {
            let local_only = session
                .handle
                .as_ref()
                .and_then(|handle| handle.lock().ok())
                .is_some_and(|handle| !handle.is_remote());
            if !local_only {
                let lease = RemoteLease::new(*id, 80, 24);
                queue.register(Arc::clone(&lease));
                session.remote_recovery = Some(RemoteRecoveryState {
                    lease,
                    next_retry: std::time::Instant::now(),
                    fresh: false,
                    retryable: true,
                    startup_delivered: true,
                    error: None,
                });
            }
        }
        self.remote_async = Some(queue);
    }

    /// Preserve the proposed identity before any asynchronous Spawn can run.
    pub fn spawn_pending(
        &mut self,
        mut spec: SessionSpec,
        cwd: Option<&Path>,
        cols: u16,
        rows: u16,
    ) -> anyhow::Result<Uuid> {
        if self.remote_async.is_none() {
            return self.spawn(spec, cwd, cols, rows);
        }
        spec.cwd = spec.cwd.or_else(|| cwd.map(Path::to_path_buf));
        let id = self.create_detached(spec);
        if let Some(recovery) = self
            .sessions
            .get_mut(&id)
            .and_then(|session| session.remote_recovery.as_mut())
        {
            recovery.fresh = true;
            recovery
                .lease
                .birth
                .store(true, std::sync::atomic::Ordering::Release);
            recovery
                .lease
                .may_close
                .store(false, std::sync::atomic::Ordering::Release);
            recovery.startup_delivered = false;
        }
        let _ = self.queue_remote_recovery(id, cols, rows, std::time::Instant::now());
        Ok(id)
    }

    pub fn set_desired_grid_size(&mut self, id: Uuid, cols: u16, rows: u16) {
        if let Some(recovery) = self
            .sessions
            .get(&id)
            .and_then(|session| session.remote_recovery.as_ref())
        {
            *recovery
                .lease
                .dimensions
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = (cols.max(1), rows.max(1));
        }
        self.queue_attached_resize(id);
    }

    fn queue_attached_resize(&mut self, id: Uuid) {
        let Some(session) = self.sessions.get(&id) else {
            return;
        };
        let (Some(recovery), Some(handle)) = (&session.remote_recovery, &session.handle) else {
            return;
        };
        if recovery.lease.retirement() != 0
            || recovery
                .lease
                .pending
                .load(std::sync::atomic::Ordering::Acquire)
        {
            return;
        }
        let desired = *recovery
            .lease
            .dimensions
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let applied = *recovery
            .lease
            .applied_dimensions
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if applied == Some(desired) {
            return;
        }
        let job = RemoteJob {
            lease: Arc::clone(&recovery.lease),
            spec: session.spec.clone(),
            scheduler: Arc::clone(&self.scheduler),
            intent: RemoteSpawnIntent::AdoptExisting,
            prepared: None,
            attached_resize: Some(Arc::clone(handle)),
        };
        if let Some(queue) = self.remote_async.as_mut() {
            let _ = queue.submit(job);
        }
    }

    pub fn recovery_error(&self, id: Uuid) -> Option<&str> {
        self.sessions
            .get(&id)?
            .remote_recovery
            .as_ref()?
            .error
            .as_deref()
    }

    fn queue_remote_recovery(
        &mut self,
        id: Uuid,
        cols: u16,
        rows: u16,
        now: std::time::Instant,
    ) -> anyhow::Result<()> {
        let session = self
            .sessions
            .get_mut(&id)
            .ok_or_else(|| anyhow::anyhow!("Runtime session not found"))?;
        let recovery = session
            .remote_recovery
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("Remote recovery is unavailable"))?;
        *recovery
            .lease
            .dimensions
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = (cols.max(1), rows.max(1));
        if recovery.lease.retirement() != 0 {
            anyhow::bail!("La sesión se está cerrando.");
        }
        if !recovery.retryable {
            anyhow::bail!(RemoteRecoveryError::birth_rejected(
                id,
                recovery
                    .error
                    .as_deref()
                    .unwrap_or("La recuperación requiere actualizar el daemon.")
            ));
        }
        if remote_spec_bytes(&session.spec).is_none() {
            recovery.retryable = false;
            recovery.error =
                Some("La configuración de la sesión supera el límite permitido.".to_owned());
            anyhow::bail!(RemoteRecoveryError::birth_rejected(
                id,
                recovery.error.as_deref().unwrap()
            ));
        }
        if recovery
            .lease
            .pending
            .load(std::sync::atomic::Ordering::Acquire)
            || recovery
                .lease
                .completion
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .is_some()
            || now < recovery.next_retry
        {
            return Ok(());
        }
        let job = RemoteJob {
            lease: Arc::clone(&recovery.lease),
            spec: session.spec.clone(),
            scheduler: Arc::clone(&self.scheduler),
            intent: if recovery.fresh {
                RemoteSpawnIntent::NewBirth
            } else {
                RemoteSpawnIntent::AdoptExisting
            },
            prepared: None,
            attached_resize: None,
        };
        let queue = self
            .remote_async
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("Remote worker is unavailable"))?;
        match queue.submit(job) {
            Ok(()) => {
                recovery.error = Some("Recuperando la sesión remota…".to_owned());
                Ok(())
            }
            Err(error) => {
                recovery.error = Some(error.to_string());
                recovery.next_retry = now + REMOTE_RETRY_DELAY;
                Err(error)
            }
        }
    }

    fn collect_remote_completions(&mut self) {
        let now = std::time::Instant::now();
        let mut completions = Vec::new();
        for (id, session) in &self.sessions {
            if let Some(recovery) = &session.remote_recovery {
                if let Ok(mut completion) = recovery.lease.completion.try_lock() {
                    if let Some(completion) = completion.take() {
                        completions.push((*id, recovery.lease.generation, completion));
                    }
                }
            }
        }
        for (id, generation, completion) in completions {
            let Some(session) = self.sessions.get_mut(&id) else {
                continue;
            };
            let Some(recovery) = session.remote_recovery.as_mut() else {
                continue;
            };
            if recovery.lease.generation != generation || recovery.lease.retirement() != 0 {
                continue;
            }
            match completion.result {
                Ok(result) if result.session_id == id => {
                    recovery.fresh = false;
                    let desired = *recovery
                        .lease
                        .dimensions
                        .lock()
                        .unwrap_or_else(|error| error.into_inner());
                    if desired != completion.dimensions {
                        // Re-apply the latest dimensions on the worker before
                        // exposing a handle. No socket resize runs under UI lock.
                        let job = RemoteJob {
                            lease: Arc::clone(&recovery.lease),
                            spec: session.spec.clone(),
                            scheduler: Arc::clone(&self.scheduler),
                            intent: RemoteSpawnIntent::AdoptExisting,
                            prepared: Some(result),
                            attached_resize: None,
                        };
                        if let Some(queue) = self.remote_async.as_mut() {
                            if let Err(error) = queue.submit(job) {
                                recovery.error = Some(error.to_string());
                                recovery.next_retry = now + REMOTE_RETRY_DELAY;
                            }
                        }
                        continue;
                    }
                    let startup = !recovery.startup_delivered;
                    recovery.startup_delivered = true;
                    recovery.error = None;
                    let ready = result.output_ready;
                    let handle = Arc::new(Mutex::new(result.handle));
                    if let Some(runtime) = crate::hook_runtime::process_runtime() {
                        let _ = runtime.observe_local(&handle);
                    }
                    if startup {
                        Self::configure_startup_input(session, &handle);
                        if ready {
                            if let Some(pending) = session.pending_startup_input.as_mut() {
                                pending.baseline_render_revision = None;
                            }
                        }
                    }
                    for pending in [
                        &mut session.pending_startup_input,
                        &mut session.pending_prompt,
                    ]
                    .into_iter()
                    .filter_map(Option::as_mut)
                    {
                        pending.defer_until_remote_publication = false;
                        if ready {
                            pending.baseline_render_revision = None;
                        } else if pending.baseline_render_revision.is_some() {
                            pending.baseline_render_revision =
                                handle.lock().ok().map(|handle| handle.render_revision());
                        }
                    }
                    session.handle = Some(handle);
                    session.detached_alive = false;
                }
                Ok(_) => {
                    recovery.error =
                        Some("La sesión remota devolvió una identidad distinta.".to_owned());
                    recovery.retryable = false;
                }
                Err(error) => {
                    if let Some(typed) = error.downcast_ref::<RemoteRecoveryError>() {
                        recovery.fresh = typed.retry_intent == RemoteSpawnIntent::NewBirth;
                        recovery.retryable = typed.retryable;
                    } else {
                        recovery.retryable = false;
                    }
                    recovery.error = Some(error.to_string());
                    recovery.next_retry = now + REMOTE_RETRY_DELAY;
                }
            }
        }
    }

    /// ¿Las sesiones nuevas se crean fuera del proceso?
    pub fn hosts_out_of_process(&self) -> bool {
        self.remote_spawner.is_some() || self.remote_async.is_some()
    }

    pub fn spawn(
        &mut self,
        spec: SessionSpec,
        cwd: Option<&Path>,
        cols: u16,
        rows: u16,
    ) -> anyhow::Result<Uuid> {
        if self.remote_async.is_some() {
            return self.spawn_pending(spec, cwd, cols, rows);
        }
        let proposed_id = Uuid::new_v4();
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
                    let typed = err.downcast_ref::<RemoteRecoveryError>();
                    if !typed.is_some_and(|error| error.safe_local_fallback) {
                        let session_id = typed.map_or(proposed_id, |error| error.session_id);
                        let mut managed = ManagedSession::detached(spec_with_cwd);
                        managed.legacy_closer = typed.and_then(|error| error.closer.clone());
                        let lease = RemoteLease::new(session_id, cols, rows);
                        lease
                            .attempted
                            .store(true, std::sync::atomic::Ordering::Release);
                        lease.birth.store(
                            typed.is_some_and(|error| {
                                error.retry_intent == RemoteSpawnIntent::NewBirth
                            }),
                            std::sync::atomic::Ordering::Release,
                        );
                        lease.may_close.store(
                            typed.is_some_and(|error| error.may_close),
                            std::sync::atomic::Ordering::Release,
                        );
                        managed.remote_recovery = Some(RemoteRecoveryState {
                            lease,
                            next_retry: std::time::Instant::now() + REMOTE_RETRY_DELAY,
                            fresh: typed.is_some_and(|error| {
                                error.retry_intent == RemoteSpawnIntent::NewBirth
                            }),
                            retryable: typed.is_some_and(|error| error.retryable),
                            startup_delivered: false,
                            error: Some(err.to_string()),
                        });
                        self.sessions.insert(session_id, managed);
                        return Ok(session_id);
                    }
                    log::warn!("el daemon no pudo hostear la sesión ({err}); se usa in-process");
                }
            }
        }
        let session_id = proposed_id;
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
            memory_task_id: spec.memory_task_id,
            hook_legacy_root: false,
        };
        // This is the sole local fallback; it must not invoke the remote callback again.
        let handle = spawn_local_handle(
            &detached_spec,
            cols,
            rows,
            session_id,
            Arc::clone(&self.scheduler),
        )?;
        let mut managed = ManagedSession::detached(detached_spec);
        let shared = Arc::new(Mutex::new(handle));
        Self::configure_startup_input(&mut managed, &shared);
        managed.handle = Some(shared);
        managed.detached_alive = false;
        self.sessions.insert(session_id, managed);
        Ok(session_id)
    }

    pub fn attach_detached(
        &mut self,
        session_id: Uuid,
        cols: u16,
        rows: u16,
    ) -> anyhow::Result<()> {
        self.collect_remote_completions();
        if self.remote_async.is_some() {
            let session = self
                .sessions
                .get_mut(&session_id)
                .ok_or_else(|| anyhow::anyhow!("Runtime session not found"))?;
            if let Some(handle) = session.handle.as_ref() {
                if let Ok(pty) = handle.lock() {
                    if pty.alive() || !pty.is_remote() || pty.remote_session_exited() {
                        return Ok(());
                    }
                } else {
                    anyhow::bail!("PTY handle lock poisoned");
                }
                session.handle = None;
            }
            self.queue_remote_recovery(session_id, cols, rows, std::time::Instant::now())?;
            anyhow::bail!(RemoteRecoveryError::protected(
                session_id,
                self.recovery_error(session_id)
                    .unwrap_or("La sesión remota sigue pendiente.")
            ));
        }
        let Some(session) = self.sessions.get_mut(&session_id) else {
            anyhow::bail!("Runtime session not found");
        };
        if session.is_attached() {
            if session.is_alive() {
                return Ok(());
            }
            let reconnect_transport = session
                .handle
                .as_ref()
                .and_then(|handle| handle.lock().ok())
                .is_some_and(|handle| handle.is_remote() && !handle.remote_session_exited());
            if !reconnect_transport {
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
        let handle = if let Some(spawner) = self.remote_spawner.as_ref() {
            // A known identity never degrades to a second local PTY.
            if let Some(recovery) = session.remote_recovery.as_ref() {
                if recovery.fresh || !recovery.retryable {
                    anyhow::bail!(RemoteRecoveryError::birth_rejected(session_id,
                        "La recuperación de este nacimiento requiere un backend con intent explícito."));
                }
                if std::time::Instant::now() < recovery.next_retry {
                    anyhow::bail!(RemoteRecoveryError::protected(
                        session_id,
                        recovery
                            .error
                            .as_deref()
                            .unwrap_or("La sesión sigue pendiente.")
                    ));
                }
            }
            match spawner(
                &spec,
                cols,
                rows,
                Arc::clone(&self.scheduler),
                Some(session_id),
            ) {
                Ok((id, handle)) if id == session_id => handle,
                Ok(_) => anyhow::bail!(RemoteRecoveryError::protected(
                    session_id,
                    "El daemon devolvió una identidad distinta."
                )),
                Err(error) => {
                    if let Some(protected) = error.downcast_ref::<RemoteRecoveryError>() {
                        session.legacy_closer = protected.closer.clone();
                    }
                    let startup_delivered = session
                        .remote_recovery
                        .as_ref()
                        .is_none_or(|recovery| recovery.startup_delivered);
                    session.remote_recovery = Some(RemoteRecoveryState {
                        lease: RemoteLease::new(session_id, cols, rows),
                        next_retry: std::time::Instant::now() + REMOTE_RETRY_DELAY,
                        fresh: false,
                        retryable: true,
                        startup_delivered,
                        error: Some(error.to_string()),
                    });
                    return Err(error);
                }
            }
        } else {
            spawn_local_handle(&spec, cols, rows, session_id, Arc::clone(&self.scheduler))?
        };
        let startup = session
            .remote_recovery
            .as_ref()
            .is_none_or(|recovery| !recovery.startup_delivered);
        let shared_handle = Arc::new(Mutex::new(handle));
        if startup {
            Self::configure_startup_input(session, &shared_handle);
        }
        if let Some(recovery) = session.remote_recovery.as_mut() {
            recovery.startup_delivered = true;
            recovery.error = None;
        }
        session.handle = Some(shared_handle);
        session.detached_alive = false;
        Ok(())
    }

    fn configure_startup_input(session: &mut ManagedSession, handle: &SharedPtyHandle) {
        if session
            .spec
            .startup_input
            .as_deref()
            .is_some_and(|input| !prompt_fits_input_budget(input.trim()))
        {
            session.spec.startup_input = None;
            session.pending_startup_input = None;
            if let Ok(handle) = handle.lock() {
                handle.record_input_error(OVERSIZED_PROMPT_ERROR.to_owned());
            }
            return;
        }
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
                    baseline_render_revision: Some(baseline_render_revision),
                    completion: None,
                    defer_until_remote_publication: false,
                });
            } else if let Ok(handle) = handle.lock() {
                if let Err(error) = write_startup_input(&handle, &input) {
                    if error.kind() == std::io::ErrorKind::WouldBlock {
                        session.pending_startup_input = Some(PendingStartupInput {
                            input,
                            baseline_render_revision: None,
                            completion: None,
                            defer_until_remote_publication: false,
                        });
                    } else {
                        handle.record_input_error(format!(
                            "No se pudo enviar el prompt inicial: {error}"
                        ));
                        log::warn!("No se pudo enviar el prompt inicial: {error}");
                    }
                }
            }
        }
    }

    pub(crate) fn spec(&self, id: Uuid) -> Option<&SessionSpec> {
        Some(&self.sessions.get(&id)?.spec)
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
        self.flush_pending_inputs();
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
        if let Some(runtime) = crate::hook_runtime::process_runtime() {
            let remote = self
                .sessions
                .get(&session_id)
                .and_then(|s| s.handle.as_ref())
                .and_then(|h| h.lock().ok())
                .map(|h| h.is_remote());
            if remote == Some(true) || (remote.is_none() && !self.local_backend) {
                runtime.release_endpoint(session_id);
            } else {
                runtime.begin_close(session_id, true);
            }
        }
        let Some(session) = self.sessions.remove(&session_id) else {
            return false;
        };
        if let (Some(queue), Some(recovery)) =
            (self.remote_async.as_mut(), session.remote_recovery.as_ref())
        {
            let local = session
                .handle
                .as_ref()
                .is_some_and(|h| h.lock().ok().is_some_and(|p| !p.is_remote()));
            if local {
                *recovery
                    .lease
                    .local_retiring
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) = session.handle.as_ref().cloned();
                recovery
                    .lease
                    .local_only
                    .store(true, std::sync::atomic::Ordering::Release);
            }
            queue.retire(&recovery.lease, local || self.local_backend);
        }
        true
    }

    /// Cierra la sesión **para siempre**: la suelta y, si vive en el daemon,
    /// la mata. Es el camino del usuario cerrando un panel.
    pub fn close_and_kill_remote(&mut self, session_id: Uuid) -> bool {
        if let Some(runtime) = crate::hook_runtime::process_runtime() {
            runtime.begin_close(session_id, true);
        }
        let Some(session) = self.sessions.remove(&session_id) else {
            return false;
        };
        if let (Some(queue), Some(recovery)) =
            (self.remote_async.as_mut(), session.remote_recovery.as_ref())
        {
            let local_only = session
                .handle
                .as_ref()
                .and_then(|handle| handle.lock().ok())
                .is_some_and(|handle| !handle.is_remote());
            if !local_only
                && !recovery.retryable
                && !recovery
                    .lease
                    .may_close
                    .load(std::sync::atomic::Ordering::Acquire)
            {
                log::warn!("No se confirmó el cierre de {session_id}: el daemon legacy no puede resolver un nacimiento ambiguo.");
                self.sessions.insert(session_id, session);
                return false;
            }
            if local_only {
                *recovery
                    .lease
                    .local_retiring
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) = session.handle.as_ref().cloned();
                recovery
                    .lease
                    .local_only
                    .store(true, std::sync::atomic::Ordering::Release);
            }
            queue.retire(&recovery.lease, true);
            return true;
        }
        if let Some(closer) = session.legacy_closer.as_ref() {
            // This compatibility API is synchronous; production uses RemoteQueue.
            let intent = session
                .remote_recovery
                .as_ref()
                .map_or(RemoteCloseIntent::AdoptExisting, |recovery| {
                    recovery.lease.close_intent()
                });
            if let Err(error) = closer(session_id, intent) {
                log::warn!("No se confirmó el cierre de {session_id}: {error}");
                self.sessions.insert(session_id, session);
                return false;
            }
            return true;
        }
        if session.handle.is_none() && self.remote_spawner.is_some() {
            log::warn!(
                "No se confirmó el cierre de {session_id}: el creador legacy no ofrece un closer."
            );
            self.sessions.insert(session_id, session);
            return false;
        }
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
        match self.queue_prompt_tracked(session_id, text) {
            Ok(receipt) => receipt.detach(),
            Err(error) => log::warn!("No se pudo encolar el prompt: {error}"),
        }
    }

    /// Enqueue into a live terminal without overtaking startup or deferred input.
    pub fn submit_prompt(&mut self, session_id: Uuid, text: &str) -> std::io::Result<()> {
        use std::io::{Error, ErrorKind};
        if text.trim().is_empty() {
            return Err(Error::new(ErrorKind::InvalidInput, "El prompt está vacío."));
        }
        if !prompt_fits_input_budget(text.trim()) {
            return Err(Error::new(ErrorKind::InvalidInput, OVERSIZED_PROMPT_ERROR));
        }
        let session = self.sessions.get(&session_id).ok_or_else(|| {
            Error::new(ErrorKind::NotFound, "La sesión del terminal ya no existe.")
        })?;
        if session.pending_startup_input.is_some() || session.pending_prompt.is_some() {
            return Err(Error::new(
                ErrorKind::WouldBlock,
                "El terminal todavía tiene un prompt pendiente; esperá antes de enviar otro.",
            ));
        }
        let handle = session
            .handle
            .as_ref()
            .ok_or_else(|| Error::new(ErrorKind::NotConnected, "El terminal no está conectado."))?;
        let pty = handle
            .lock()
            .map_err(|_| Error::other("No se pudo acceder al terminal."))?;
        if !pty.alive() {
            return Err(Error::new(ErrorKind::BrokenPipe, "El terminal terminó."));
        }
        write_prompt_input(&pty, text)
    }

    /// A bounded startup slot, with observable acceptance and no replacement.
    /// This is volatile transport deferral, not a persistent agent-work queue.
    pub fn queue_prompt_tracked(
        &mut self,
        session_id: Uuid,
        text: &str,
    ) -> std::io::Result<DeferredPrompt> {
        use std::io::{Error, ErrorKind};
        let session = self.sessions.get_mut(&session_id).ok_or_else(|| {
            Error::new(ErrorKind::NotFound, "La sesión del terminal ya no existe.")
        })?;
        let handle = session.handle.as_ref();
        let recoverable = session.remote_recovery.as_ref().is_some_and(|recovery| {
            recovery.lease.retirement() == 0
                && (recovery.retryable
                    || recovery
                        .lease
                        .pending
                        .load(std::sync::atomic::Ordering::Acquire))
        });
        if handle.is_none() && !recoverable {
            return Err(Error::new(
                ErrorKind::NotConnected,
                "El terminal no está conectado.",
            ));
        }
        if text.trim().is_empty() {
            return Err(Error::new(ErrorKind::InvalidInput, "El prompt está vacío."));
        }
        if session.pending_prompt.is_some() {
            return Err(Error::new(
                ErrorKind::WouldBlock,
                "Hay otro prompt pendiente; esperá a que el terminal lo acepte.",
            ));
        }
        if !prompt_fits_input_budget(text.trim()) {
            if let Some(handle) = handle.and_then(|handle| handle.lock().ok()) {
                handle.record_input_error(OVERSIZED_PROMPT_ERROR.to_owned());
            }
            return Err(Error::new(ErrorKind::InvalidInput, OVERSIZED_PROMPT_ERROR));
        }
        let baseline = if let Some(handle) = handle {
            let pty = handle
                .lock()
                .map_err(|_| Error::other("No se pudo acceder al estado del terminal."))?;
            if !pty.alive() && !(pty.is_remote() && !pty.remote_session_exited() && recoverable) {
                return Err(Error::new(ErrorKind::BrokenPipe, "El terminal terminó."));
            }
            if pty.alive() {
                pty.check_input_open()?;
            }
            Some(pty.render_revision())
        } else {
            Some(u64::MAX)
        };
        let (completion, receipt) = DeferredPrompt::channel();
        let cancellation = receipt.cancellation_token();
        session.pending_prompt = Some(PendingStartupInput {
            input: text.trim().to_owned(),
            baseline_render_revision: baseline,
            completion: Some((completion, cancellation)),
            defer_until_remote_publication: handle.is_none() && recoverable,
        });
        Ok(receipt)
    }

    /// Progress input without consuming scheduler events for hidden workspaces.
    pub fn flush_pending_inputs(&mut self) {
        self.collect_remote_completions();
        if let Some(queue) = self.remote_async.as_mut() {
            if queue.has_cleanup() {
                let _ = queue.ensure_started();
            }
        }
        if self.remote_async.is_some() {
            let mut retry = Vec::new();
            for (id, session) in &mut self.sessions {
                let disconnected = session
                    .handle
                    .as_ref()
                    .and_then(|handle| handle.try_lock().ok())
                    .is_some_and(|handle| {
                        !handle.alive() && handle.is_remote() && !handle.remote_session_exited()
                    });
                if disconnected {
                    session.handle = None;
                }
                if session.handle.is_none() {
                    if let Some(recovery) = session.remote_recovery.as_ref() {
                        let dimensions = *recovery
                            .lease
                            .dimensions
                            .lock()
                            .unwrap_or_else(|error| error.into_inner());
                        retry.push((*id, dimensions));
                    }
                }
            }
            let now = std::time::Instant::now();
            for (id, (cols, rows)) in retry {
                let _ = self.queue_remote_recovery(id, cols, rows, now);
            }
        }
        let attached: Vec<_> = self.sessions.keys().copied().collect();
        for id in attached {
            self.queue_attached_resize(id);
        }
        for session in self.sessions.values_mut() {
            if session.pending_prompt.as_ref().is_some_and(|pending| {
                pending.completion.as_ref().is_some_and(|(_, cancelled)| {
                    cancelled.load(std::sync::atomic::Ordering::Acquire)
                })
            }) {
                session.pending_prompt = None;
            }
            Self::flush_one_pending(session, true);
            if session.pending_startup_input.is_none() {
                Self::flush_one_pending(session, false);
            }
        }
    }

    pub fn has_pending_inputs(&self) -> bool {
        self.remote_async
            .as_ref()
            .is_some_and(RemoteQueue::has_cleanup)
            || self.sessions.values().any(|session| {
                session.pending_startup_input.is_some()
                    || session.pending_prompt.is_some()
                    || (session
                        .remote_recovery
                        .as_ref()
                        .is_some_and(|recovery| recovery.retryable)
                        && !session.is_attached())
            })
    }

    /// Flushea `pending_startup_input` (is_startup=true) o `pending_prompt`
    /// (false) si el handle está listo. Conserva el prompt si la cola está llena.
    fn flush_one_pending(session: &mut ManagedSession, is_startup: bool) {
        let Some(handle) = session.handle.as_ref() else {
            let waiting_for_first_publication = session
                .pending_prompt
                .as_ref()
                .is_some_and(|pending| pending.defer_until_remote_publication)
                && session.remote_recovery.as_ref().is_some_and(|recovery| {
                    recovery.lease.retirement() == 0
                        && (recovery.retryable
                            || recovery
                                .lease
                                .pending
                                .load(std::sync::atomic::Ordering::Acquire))
                });
            if waiting_for_first_publication {
                return;
            }
            // T3 P2: a lost retained handle resolves tracked feedback before
            // blocked startup can hide it. Untracked startup stays preserved.
            if let Some((completion, _)) = session
                .pending_prompt
                .as_mut()
                .and_then(|pending| pending.completion.take())
            {
                let _ = completion.send(Err(std::io::Error::new(
                    std::io::ErrorKind::NotConnected,
                    "El terminal se desconectó antes de aceptar el prompt pendiente.",
                )));
                session.pending_prompt = None;
            }
            return;
        };
        let pending_ref = if is_startup {
            session.pending_startup_input.as_mut()
        } else {
            session.pending_prompt.as_mut()
        };
        let Some(pending) = pending_ref else {
            return;
        };

        // Hold a single lock across the readiness check and the write so
        // the handle cannot change state between the two.
        let Ok(handle) = handle.lock() else {
            if let Some((completion, _)) = pending.completion.take() {
                let _ = completion.send(Err(std::io::Error::other(
                    "No se pudo acceder al terminal pendiente.",
                )));
            }
            if is_startup {
                session.pending_startup_input = None;
            } else {
                session.pending_prompt = None;
            }
            return;
        };
        let result = if !handle.alive() {
            if handle.is_remote()
                && !handle.remote_session_exited()
                && session
                    .remote_recovery
                    .as_ref()
                    .is_some_and(|recovery| recovery.lease.retirement() == 0)
            {
                return;
            }
            Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "El terminal terminó antes de recibir el prompt.",
            ))
        } else if let Err(error) = handle.check_input_open() {
            Err(error)
        } else {
            pending.try_send(handle.render_revision(), |input| {
                if is_startup {
                    write_startup_input(&handle, input)
                } else {
                    write_prompt_input(&handle, input)
                }
            })
        };
        if matches!(result, Ok(false)) {
            // Enqueue is atomic: no bytes were accepted, so retrying cannot
            // duplicate a partial prompt. UI updates retry after the queue drains.
            return;
        }
        if let Err(error) = &result {
            handle.record_input_error(format!("No se pudo enviar el prompt pendiente: {error}"));
            log::warn!("No se pudo enviar el prompt pendiente: {error}");
        }
        if let Some((completion, _)) = pending.completion.take() {
            let _ = completion.send(result.map(|_| ()));
        }
        if is_startup {
            session.pending_startup_input = None;
        } else {
            session.pending_prompt = None;
        }
    }
}

/// Inyecta el prompt inicial en el agente (idea de orca): neutraliza bytes
/// de escape para que el brief no pueda emitir secuencias de control, y si
/// el TUI ya activó bracketed paste lo envía como paste atómico para que un
/// brief multi-línea no se ejecute línea por línea.
fn write_startup_input(handle: &PtyHandle, input: &str) -> std::io::Result<()> {
    let sanitized = sanitize_agent_prompt(input);
    let mode = handle.input_mode();
    let mut bytes = paste_bytes(&sanitized, &mode);
    bytes.push(b'\n');
    handle.try_write_all(&bytes)
}

/// Prompt interactivo (feedback): igual que el startup pero submit con `\r`
/// (la tecla Enter real), consistente con `agent_prompt_bytes`.
fn write_prompt_input(handle: &PtyHandle, input: &str) -> std::io::Result<()> {
    let bytes = agent_prompt_bytes(input, &handle.input_mode());
    handle.try_write_all(&bytes)
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
            baseline_render_revision: Some(4),
            completion: None,
            defer_until_remote_publication: false,
        };

        assert!(!pending.is_ready(4));
        assert!(pending.is_ready(5));
    }

    #[test]
    fn pending_input_retries_capacity_without_another_render() {
        let mut pending = PendingStartupInput {
            input: "prompt".to_owned(),
            baseline_render_revision: None,
            completion: None,
            defer_until_remote_publication: false,
        };
        assert!(!pending
            .try_send(0, |_| Err(std::io::ErrorKind::WouldBlock.into()))
            .unwrap());
        let mut accepted = String::new();
        assert!(pending
            .try_send(0, |input| {
                accepted.push_str(input);
                Ok(())
            })
            .unwrap());
        assert_eq!(accepted, "prompt");
    }

    #[test]
    fn pending_input_waits_before_attempting_and_returns_permanent_errors() {
        let mut pending = PendingStartupInput {
            input: "prompt".to_owned(),
            baseline_render_revision: Some(4),
            completion: None,
            defer_until_remote_publication: false,
        };
        assert!(!pending.try_send(4, |_| panic!("not ready")).unwrap());
        assert_eq!(
            pending
                .try_send(5, |_| Err(std::io::ErrorKind::InvalidInput.into()))
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::InvalidInput
        );
    }

    #[test]
    fn deferred_prompt_budget_includes_escape_expansion_and_paste_framing() {
        let limit = crate::terminal::pty::MAX_INPUT_BYTES;
        assert!(super::prompt_fits_input_budget(&"x".repeat(limit - 13)));
        assert!(!super::prompt_fits_input_budget(&"x".repeat(limit - 12)));
        assert!(!super::prompt_fits_input_budget(&"\x1b".repeat(limit / 4)));
        assert!(!super::prompt_fits_input_budget(&"x".repeat(limit + 1)));
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

    fn prompt_delivery_fixture() -> (PtyManager, Uuid, super::SharedPtyHandle) {
        let mut manager = PtyManager::new_for_tests();
        let id = manager.create_detached(SessionSpec::default());
        let handle = std::sync::Arc::new(std::sync::Mutex::new(
            crate::terminal::pty::PtyHandle::for_persistence_tests(b""),
        ));
        manager.sessions.get_mut(&id).unwrap().handle = Some(std::sync::Arc::clone(&handle));
        (manager, id, handle)
    }

    #[test]
    fn prompt_delivery_rejects_missing_and_detached_destinations() {
        let mut manager = PtyManager::new_for_tests();
        assert!(
            matches!(manager.queue_prompt_tracked(Uuid::new_v4(), "feedback"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound)
        );
        let id = manager.create_detached(SessionSpec::default());
        assert!(matches!(manager.queue_prompt_tracked(id, "feedback"),
            Err(error) if error.kind() == std::io::ErrorKind::NotConnected));
        assert!(!manager.is_attached(id));
    }

    #[test]
    fn prompt_delivery_does_not_replace_or_overtake_an_existing_prompt() {
        let (mut manager, id, handle) = prompt_delivery_fixture();
        let first = manager
            .queue_prompt_tracked(id, "original feedback")
            .unwrap();
        assert!(matches!(manager.queue_prompt_tracked(id, "replacement"),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock));
        assert_eq!(
            manager
                .submit_prompt(id, "overtaking input")
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::WouldBlock
        );
        assert_eq!(
            manager.sessions[&id].pending_prompt.as_ref().unwrap().input,
            "original feedback"
        );
        manager.flush_pending_inputs();
        assert!(first.poll().is_none());
        handle.lock().unwrap().mark_render_dirty();
        manager.flush_pending_inputs();
        assert!(first.poll().unwrap().is_ok());
        assert!(!manager.has_pending_inputs());
        assert_eq!(handle.lock().unwrap().activity_revision(), 1);
    }

    #[test]
    fn prompt_delivery_closing_input_returns_failure_without_acknowledgement() {
        let (mut manager, id, handle) = prompt_delivery_fixture();
        let receipt = manager.queue_prompt_tracked(id, "feedback").unwrap();
        let pty = handle.lock().unwrap();
        pty.close_input_for_prompt_tests();
        drop(pty);
        manager.flush_pending_inputs();
        assert_eq!(
            receipt.poll().unwrap().unwrap_err().kind(),
            std::io::ErrorKind::BrokenPipe
        );
        assert!(!manager.has_pending_inputs());
        assert_eq!(handle.lock().unwrap().activity_revision(), 0);
        assert!(
            matches!(manager.queue_prompt_tracked(id, "deferred feedback"),
            Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe)
        );
        assert_eq!(
            manager
                .submit_prompt(id, "direct feedback")
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::BrokenPipe
        );
    }

    #[test]
    fn prompt_delivery_dropping_the_receipt_cancels_unaccepted_input() {
        let (mut manager, id, handle) = prompt_delivery_fixture();
        let receipt = manager
            .queue_prompt_tracked(id, "cancelled feedback")
            .unwrap();
        drop(receipt);
        handle.lock().unwrap().mark_render_dirty();
        manager.flush_pending_inputs();
        assert!(!manager.has_pending_inputs());
        assert_eq!(handle.lock().unwrap().activity_revision(), 0);
        assert!(manager.submit_prompt(id, "new feedback").is_ok());
    }

    #[test]
    fn prompt_delivery_does_not_pass_the_startup_readiness_gate() {
        let (mut manager, id, handle) = prompt_delivery_fixture();
        manager.sessions.get_mut(&id).unwrap().pending_startup_input = Some(PendingStartupInput {
            input: "startup brief".to_owned(),
            baseline_render_revision: Some(100),
            completion: None,
            defer_until_remote_publication: false,
        });
        let receipt = manager.queue_prompt_tracked(id, "review feedback").unwrap();
        handle.lock().unwrap().mark_render_dirty();
        manager.flush_pending_inputs();
        assert!(receipt.poll().is_none());
        assert_eq!(handle.lock().unwrap().activity_revision(), 0);
        assert_eq!(
            manager
                .submit_prompt(id, "another prompt")
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::WouldBlock
        );
    }

    #[test]
    fn prompt_delivery_cancellation_frees_the_slot_during_blocked_startup() {
        let (mut manager, id, handle) = prompt_delivery_fixture();
        manager.sessions.get_mut(&id).unwrap().pending_startup_input = Some(PendingStartupInput {
            input: "startup brief".to_owned(),
            baseline_render_revision: Some(100),
            completion: None,
            defer_until_remote_publication: false,
        });
        let receipt = manager
            .queue_prompt_tracked(id, "cancelled feedback")
            .unwrap();
        drop(receipt);
        manager.flush_pending_inputs();
        assert!(manager.sessions[&id].pending_startup_input.is_some());
        assert!(manager.sessions[&id].pending_prompt.is_none());
        let replacement = manager.queue_prompt_tracked(id, "new feedback").unwrap();
        manager.flush_pending_inputs();
        assert!(replacement.poll().is_none());
        assert_eq!(handle.lock().unwrap().activity_revision(), 0);
    }

    #[test]
    fn prompt_delivery_poisoned_handle_resolves_instead_of_waiting_forever() {
        let (mut manager, id, handle) = prompt_delivery_fixture();
        let receipt = manager.queue_prompt_tracked(id, "review feedback").unwrap();
        let _ = std::panic::catch_unwind(|| {
            let _guard = handle.lock().unwrap();
            panic!("synthetic terminal-state failure");
        });
        manager.flush_pending_inputs();
        assert_eq!(
            receipt.poll().unwrap().unwrap_err().kind(),
            std::io::ErrorKind::Other
        );
        assert!(!manager.has_pending_inputs());
    }

    #[test]
    fn prompt_delivery_closing_the_session_resolves_the_pending_receipt() {
        let (mut manager, id, _handle) = prompt_delivery_fixture();
        let receipt = manager.queue_prompt_tracked(id, "feedback").unwrap();
        manager.close(id);
        assert_eq!(
            receipt.poll().unwrap().unwrap_err().kind(),
            std::io::ErrorKind::BrokenPipe
        );
    }

    #[test]
    fn prompt_delivery_rejects_empty_and_oversized_text_without_a_pending_slot() {
        let (mut manager, id, handle) = prompt_delivery_fixture();
        assert!(matches!(manager.queue_prompt_tracked(id, " \n "),
            Err(error) if error.kind() == std::io::ErrorKind::InvalidInput));
        let oversized = "x".repeat(crate::terminal::pty::MAX_INPUT_BYTES + 1);
        assert!(matches!(manager.queue_prompt_tracked(id, &oversized),
            Err(error) if error.kind() == std::io::ErrorKind::InvalidInput));
        assert_eq!(
            manager.submit_prompt(id, &oversized).unwrap_err().kind(),
            std::io::ErrorKind::InvalidInput
        );
        let escaped = "\u{1b}".repeat(crate::terminal::pty::MAX_INPUT_BYTES / 4);
        assert_eq!(
            manager.submit_prompt(id, &escaped).unwrap_err().kind(),
            std::io::ErrorKind::InvalidInput
        );
        assert!(!manager.has_pending_inputs());
        assert_eq!(handle.lock().unwrap().activity_revision(), 0);
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

    #[test]
    fn prompt_delivery_missing_handle_resolves_receipt_without_losing_startup() {
        for blocked_startup in [false, true] {
            let (mut manager, id, handle) = prompt_delivery_fixture();
            if blocked_startup {
                manager.sessions.get_mut(&id).unwrap().pending_startup_input =
                    Some(PendingStartupInput {
                        input: "startup brief".to_owned(),
                        baseline_render_revision: Some(100),
                        completion: None,
                        defer_until_remote_publication: false,
                    });
            }
            let first = manager
                .queue_prompt_tracked(id, "original feedback")
                .unwrap();
            assert!(matches!(manager.queue_prompt_tracked(id, "replacement"),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock));
            assert_eq!(
                manager.sessions[&id].pending_prompt.as_ref().unwrap().input,
                "original feedback"
            );
            manager.sessions.get_mut(&id).unwrap().handle = None;
            manager.flush_pending_inputs();

            assert_eq!(
                first.poll().unwrap().unwrap_err().kind(),
                std::io::ErrorKind::NotConnected
            );
            assert!(manager.sessions.contains_key(&id));
            assert!(!manager.is_attached(id));
            assert!(manager.sessions[&id].pending_prompt.is_none());
            assert_eq!(
                manager.sessions[&id].pending_startup_input.is_some(),
                blocked_startup
            );
            assert_eq!(manager.has_pending_inputs(), blocked_startup);
            assert_eq!(handle.lock().unwrap().activity_revision(), 0);
            assert!(
                matches!(manager.queue_prompt_tracked(id, "still disconnected"),
                Err(error) if error.kind() == std::io::ErrorKind::NotConnected)
            );
            if blocked_startup {
                assert_eq!(
                    manager.sessions[&id]
                        .pending_startup_input
                        .as_ref()
                        .unwrap()
                        .input,
                    "startup brief"
                );
            }

            manager.sessions.get_mut(&id).unwrap().handle = Some(std::sync::Arc::clone(&handle));
            let replacement = manager
                .queue_prompt_tracked(id, "new explicit prompt")
                .unwrap();
            handle.lock().unwrap().mark_render_dirty();
            manager.flush_pending_inputs();
            if blocked_startup {
                assert!(replacement.poll().is_none());
                assert_eq!(handle.lock().unwrap().activity_revision(), 0);
            } else {
                assert!(replacement.poll().unwrap().is_ok());
                assert_eq!(handle.lock().unwrap().activity_revision(), 1);
            }
        }
    }
}

#[cfg(test)]
mod full_attach_prompt_recovery_fixtures {
    use super::*;
    fn pending_manager() -> (PtyManager, Uuid) {
        let mut manager = PtyManager::new_for_tests();
        let id = manager.create_detached(SessionSpec::default());
        manager.sessions.get_mut(&id).unwrap().remote_recovery = Some(RemoteRecoveryState {
            lease: RemoteLease::new(id, 117, 39),
            next_retry: std::time::Instant::now(),
            fresh: true,
            retryable: true,
            startup_delivered: false,
            error: Some("pending".to_owned()),
        });
        (manager, id)
    }
    #[test]
    fn tracked_prompt_keeps_receipt_during_pending_recovery_and_then_accepts() {
        let (mut manager, id) = pending_manager();
        let receipt = manager.queue_prompt_tracked(id, "KEEP ORIGINAL").unwrap();
        manager.flush_pending_inputs();
        assert!(receipt.poll().is_none());
        assert_eq!(
            manager.sessions[&id].pending_prompt.as_ref().unwrap().input,
            "KEEP ORIGINAL"
        );
        let handle = PtyHandle::for_persistence_tests(b"READY");
        let lease = Arc::clone(
            &manager.sessions[&id]
                .remote_recovery
                .as_ref()
                .unwrap()
                .lease,
        );
        *lease.completion.lock().unwrap() = Some(RemoteCompletion {
            result: Ok(RemoteSpawnResult {
                session_id: id,
                handle,
                output_ready: true,
                owned_birth: true,
            }),
            dimensions: (117, 39),
        });
        manager.collect_remote_completions();
        manager.flush_pending_inputs();
        assert!(receipt.poll().unwrap().is_ok());
        assert!(manager.sessions[&id].pending_prompt.is_none());
    }
    #[test]
    fn pending_recovery_preserves_cancellation_and_does_not_replace_a_prompt() {
        let (mut manager, id) = pending_manager();
        let receipt = manager.queue_prompt_tracked(id, "FIRST").unwrap();
        assert_eq!(
            manager
                .queue_prompt_tracked(id, "SECOND")
                .err()
                .unwrap()
                .kind(),
            std::io::ErrorKind::WouldBlock
        );
        drop(receipt);
        manager.flush_pending_inputs();
        assert!(manager.sessions[&id].pending_prompt.is_none());
        let replacement = manager.queue_prompt_tracked(id, "AFTER CANCEL").unwrap();
        assert!(replacement.poll().is_none());
    }
    #[test]
    fn unproven_birth_has_no_safe_fallback_retry_or_close_rights() {
        let error = RemoteRecoveryError::birth_pending(Uuid::from_u128(0x6300), "upgrade", false);
        assert!(!error.safe_local_fallback);
        assert!(!error.retryable);
        assert!(!error.may_close);
        assert_eq!(error.retry_intent, RemoteSpawnIntent::NewBirth);
    }
}

#[cfg(test)]
mod full_attach_worker_fixtures {
    use super::*;
    use std::sync::atomic::Ordering;

    fn job(lease: Arc<RemoteLease>, intent: RemoteSpawnIntent) -> RemoteJob {
        RemoteJob {
            lease,
            spec: SessionSpec::default(),
            scheduler: Arc::new(Mutex::new(RuntimeScheduler::new_for_tests())),
            intent,
            prepared: None,
            attached_resize: None,
        }
    }

    #[test]
    fn retired_before_execution_never_calls_remote_spawn() {
        let lease = RemoteLease::new(Uuid::from_u128(0x6401), 117, 39);
        lease.pending.store(true, Ordering::Release);
        lease.retired.store(REMOTE_CLOSED, Ordering::Release);
        let spawn: AsyncRemoteSpawner = Arc::new(|_, _, _, _, _, _| panic!("retired factory"));
        perform_remote_job(job(Arc::clone(&lease), RemoteSpawnIntent::NewBirth), &spawn);
        assert!(!lease.pending.load(Ordering::Acquire));
        assert!(!lease.attempted.load(Ordering::Acquire));
        assert!(lease.completion.lock().unwrap().is_none());
    }

    #[test]
    fn retirement_during_ambiguous_spawn_keeps_uuid_close_rights_without_publication() {
        let id = Uuid::from_u128(0x6402);
        let lease = RemoteLease::new(id, 117, 39);
        let lease_in_factory = Arc::clone(&lease);
        let spawn: AsyncRemoteSpawner = Arc::new(move |_, cols, rows, _, actual_id, intent| {
            assert_eq!(
                (cols, rows, actual_id, intent),
                (117, 39, id, RemoteSpawnIntent::NewBirth)
            );
            lease_in_factory
                .retired
                .store(REMOTE_CLOSED, Ordering::Release);
            Err(RemoteRecoveryError::birth_pending(id, "lost ACK", true).into())
        });
        perform_remote_job(job(Arc::clone(&lease), RemoteSpawnIntent::NewBirth), &spawn);
        assert!(lease.attempted.load(Ordering::Acquire));
        assert!(lease.cleanup_eligible());
        assert_eq!(lease.close_intent(), RemoteCloseIntent::OwnedBirth);
        assert!(lease.completion.lock().unwrap().is_none());
        assert!(!lease.pending.load(Ordering::Acquire));
    }

    #[test]
    fn queue_capacity_rejection_preserves_the_unscheduled_identity() {
        let spawn: AsyncRemoteSpawner =
            Arc::new(|_, _, _, _, _, _| panic!("fixture starts no worker"));
        let closer: RemoteCloser = Arc::new(|_, _| panic!("fixture starts no cleanup"));
        let mut queue = RemoteQueue::new(Uuid::from_u128(0x6403), spawn, closer);
        queue.started = true; // Exercise actual submit admission with no worker thread.
        for value in 0..REMOTE_QUEUE_CAPACITY {
            let lease = RemoteLease::new(Uuid::from_u128(0x6500 + value as u128), 117, 39);
            queue
                .submit(job(lease, RemoteSpawnIntent::NewBirth))
                .unwrap();
        }
        let id = Uuid::from_u128(0x64ff);
        let lease = RemoteLease::new(id, 117, 39);
        let error = queue
            .submit(job(Arc::clone(&lease), RemoteSpawnIntent::NewBirth))
            .unwrap_err();
        let protected = error.downcast_ref::<RemoteRecoveryError>().unwrap();
        assert_eq!(protected.session_id, id);
        assert!(!protected.safe_local_fallback);
        assert!(!lease.pending.load(Ordering::Acquire));
        assert!(!lease.attempted.load(Ordering::Acquire));
        assert_eq!(*lease.dimensions.lock().unwrap(), (117, 39));
        assert_eq!(
            queue.state.0.lock().unwrap().ready.len(),
            REMOTE_QUEUE_CAPACITY
        );
    }
}

#[cfg(test)]
mod birth_uncertainty_fixtures {
    use super::*;
    use std::sync::atomic::Ordering;
    fn job(lease: Arc<RemoteLease>) -> RemoteJob {
        RemoteJob {
            lease,
            spec: SessionSpec::default(),
            scheduler: Arc::new(Mutex::new(RuntimeScheduler::new_for_tests())),
            intent: RemoteSpawnIntent::NewBirth,
            prepared: None,
            attached_resize: None,
        }
    }
    fn forbidden_local(
        _: &SessionSpec,
        _: u16,
        _: u16,
        _: Uuid,
        _: SharedRuntimeScheduler,
    ) -> anyhow::Result<PtyHandle> {
        panic!("duplicate/cancelled local factory")
    }
    #[test]
    fn lost_spawn_ack_then_connect_failure_never_calls_local_factory_or_loses_close_rights() {
        let id = Uuid::from_u128(0x6701);
        let lease = RemoteLease::new(id, 117, 39);
        let lost: AsyncRemoteSpawner = Arc::new(move |_, _, _, _, actual, _| {
            assert_eq!(actual, id);
            Err(RemoteRecoveryError::birth_pending(id, "lost ACK", true).into())
        });
        perform_remote_job_with_fallback(job(Arc::clone(&lease)), &lost, &forbidden_local);
        lease.completion.lock().unwrap().take();
        let disconnected: AsyncRemoteSpawner = Arc::new(move |_, _, _, _, actual, _| {
            assert_eq!(actual, id);
            Err(RemoteRecoveryError::before_creation(id, "connect refusal on retry").into())
        });
        perform_remote_job_with_fallback(job(Arc::clone(&lease)), &disconnected, &forbidden_local);
        let completion = lease.completion.lock().unwrap().take().unwrap();
        let error = completion.result.err().unwrap();
        let typed = error.downcast_ref::<RemoteRecoveryError>().unwrap();
        assert_eq!(typed.session_id, id);
        assert!(!typed.safe_local_fallback);
        assert!(typed.may_close && lease.cleanup_eligible());
        assert_eq!(typed.retry_intent, RemoteSpawnIntent::NewBirth);
        assert_eq!(lease.close_intent(), RemoteCloseIntent::OwnedBirth);
        assert!(lease.may_have_spawned.load(Ordering::Acquire));
        assert!(!lease.local_only.load(Ordering::Acquire));
    }
    #[test]
    fn callback_panic_after_possible_spawn_keeps_birth_cleanup_and_no_fallback() {
        let id = Uuid::from_u128(0x6702);
        let lease = RemoteLease::new(id, 117, 39);
        let spawn: AsyncRemoteSpawner = Arc::new(move |_, _, _, _, actual, _| {
            assert_eq!(actual, id);
            panic!("panic after admitted Spawn")
        });
        perform_remote_job_with_fallback(job(Arc::clone(&lease)), &spawn, &forbidden_local);
        let completion = lease.completion.lock().unwrap().take().unwrap();
        let error = completion.result.err().unwrap();
        let typed = error.downcast_ref::<RemoteRecoveryError>().unwrap();
        assert!(!typed.safe_local_fallback);
        assert!(typed.retryable && typed.may_close);
        assert!(lease.cleanup_eligible());
        assert_eq!(lease.close_intent(), RemoteCloseIntent::OwnedBirth);
    }
    #[test]
    fn retirement_during_known_uncreated_connect_failure_closes_local_admission() {
        let id = Uuid::from_u128(0x6703);
        let lease = RemoteLease::new(id, 117, 39);
        let closing = Arc::clone(&lease);
        let spawn: AsyncRemoteSpawner = Arc::new(move |_, _, _, _, _, _| {
            closing.retired.store(REMOTE_CLOSED, Ordering::Release);
            closing.close_local_birth_admissions();
            Err(RemoteRecoveryError::before_creation(id, "known uncreated").into())
        });
        perform_remote_job_with_fallback(job(Arc::clone(&lease)), &spawn, &forbidden_local);
        assert!(!lease.admit_local_birth());
        assert!(!lease.local_only.load(Ordering::Acquire));
        assert!(lease.completion.lock().unwrap().is_none());
        assert!(!lease.pending.load(Ordering::Acquire));
    }
}

#[cfg(test)]
mod t3_recovery_admission_fixtures {
    use super::*;
    #[test]
    fn prepublication_deferred_receipt_waits_but_lost_published_handle_resolves_before_startup() {
        let mut manager = PtyManager::new_for_tests();
        let id = manager.create_detached(SessionSpec::default());
        let lease = RemoteLease::new(id, 117, 39);
        manager.sessions.get_mut(&id).unwrap().remote_recovery = Some(RemoteRecoveryState {
            lease: Arc::clone(&lease),
            next_retry: std::time::Instant::now(),
            fresh: true,
            retryable: true,
            startup_delivered: false,
            error: None,
        });
        let receipt = manager
            .queue_prompt_tracked(id, "KEEP UNTIL FIRST PUBLICATION")
            .unwrap();
        manager.flush_pending_inputs();
        assert!(receipt.poll().is_none());
        *lease.completion.lock().unwrap() = Some(RemoteCompletion {
            dimensions: (117, 39),
            result: Ok(RemoteSpawnResult {
                session_id: id,
                handle: PtyHandle::for_persistence_tests(b""),
                output_ready: false,
                owned_birth: true,
            }),
        });
        manager.collect_remote_completions();
        let session = manager.sessions.get_mut(&id).unwrap();
        assert!(
            !session
                .pending_prompt
                .as_ref()
                .unwrap()
                .defer_until_remote_publication
        );
        session.pending_startup_input = Some(PendingStartupInput {
            input: "KEEP STARTUP".to_owned(),
            baseline_render_revision: Some(100),
            completion: None,
            defer_until_remote_publication: false,
        });
        session.handle = None;
        manager.flush_pending_inputs();
        assert_eq!(
            receipt.poll().unwrap().unwrap_err().kind(),
            std::io::ErrorKind::NotConnected
        );
        assert!(manager.sessions[&id].pending_prompt.is_none());
        assert_eq!(
            manager.sessions[&id]
                .pending_startup_input
                .as_ref()
                .unwrap()
                .input,
            "KEEP STARTUP"
        );
    }
}

#[cfg(test)]
mod permanent_first_publication_failure_fixtures {
    use super::*;
    #[test]
    fn permanent_birth_failure_resolves_admitted_feedback_and_rejects_new_receipts() {
        let mut manager = PtyManager::new_for_tests();
        let id = manager.create_detached(SessionSpec::default());
        let lease = RemoteLease::new(id, 117, 39);
        manager.sessions.get_mut(&id).unwrap().remote_recovery = Some(RemoteRecoveryState {
            lease: Arc::clone(&lease),
            next_retry: std::time::Instant::now(),
            fresh: true,
            retryable: true,
            startup_delivered: false,
            error: None,
        });
        let receipt = manager
            .queue_prompt_tracked(id, "ADMITTED BEFORE REFUSAL")
            .unwrap();
        manager.sessions.get_mut(&id).unwrap().pending_startup_input = Some(PendingStartupInput {
            input: "KEEP UNTRACKED STARTUP".to_owned(),
            baseline_render_revision: Some(100),
            completion: None,
            defer_until_remote_publication: false,
        });
        *lease.completion.lock().unwrap() = Some(RemoteCompletion {
            dimensions: (117, 39),
            result: Err(RemoteRecoveryError::birth_rejected(id, "permanent refusal").into()),
        });
        manager.collect_remote_completions();
        assert!(
            !manager.sessions[&id]
                .remote_recovery
                .as_ref()
                .unwrap()
                .retryable
        );
        assert_eq!(
            manager
                .queue_prompt_tracked(id, "NEW RECEIPT")
                .err()
                .unwrap()
                .kind(),
            std::io::ErrorKind::NotConnected
        );
        manager.flush_pending_inputs();
        assert_eq!(
            receipt.poll().unwrap().unwrap_err().kind(),
            std::io::ErrorKind::NotConnected
        );
        assert!(manager.sessions[&id].pending_prompt.is_none());
        assert_eq!(
            manager.sessions[&id]
                .pending_startup_input
                .as_ref()
                .unwrap()
                .input,
            "KEEP UNTRACKED STARTUP"
        );
        assert!(manager.sessions.contains_key(&id));
    }
}

impl Drop for PtyManager {
    fn drop(&mut self) {
        // Local PTYs transfer to the aggregate closer before their controller
        // disappears. Remote births use endpoint release, preserving hot restore.
        let ids = self.sessions.keys().copied().collect::<Vec<_>>();
        for id in ids {
            let _ = self.close(id);
        }
    }
}

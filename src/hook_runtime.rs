//! Process endpoint authority. All SQLite and waits run on workers. Managed
//! failures stay opaque and cannot enter the global unscoped memory branch.
use crate::hook_authority::*;
use crate::memory::hook_ledger::LedgerIntent;
use crate::memory::MemoryStore;
use crate::orchestration::HookEvent;
use anyhow::{anyhow, Result};
use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::Duration;
use uuid::Uuid;

static RUNTIME: OnceLock<Arc<HookRuntime>> = OnceLock::new();
pub(crate) fn process_runtime() -> Option<&'static Arc<HookRuntime>> {
    RUNTIME.get()
}
pub(crate) fn bootstrap(owner: Uuid) -> &'static Arc<HookRuntime> {
    RUNTIME.get_or_init(|| {
        let store = if cfg!(test) {
            None
        } else {
            match MemoryStore::open_default_for_app() {
                Ok(store) => Some(store),
                Err(error) => {
                    log::error!("Managed terminal memory could not be opened safely: {error:#}");
                    None
                }
            }
        };
        HookRuntime::new(owner, store)
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AliasStatus {
    Pending,
    Acknowledged,
    Failed,
}
#[derive(Clone)]
pub(crate) struct AliasFence(Arc<Mutex<AliasStatus>>);
impl AliasFence {
    pub fn status(&self) -> AliasStatus {
        self.0.lock().map(|s| *s).unwrap_or(AliasStatus::Failed)
    }
    #[cfg(test)]
    pub fn from_closed_ledger_for_fixture(store: &MemoryStore, key: LeafKey) -> Result<Self> {
        let (_, closed, _) = store.hook_alias_state(key)?;
        anyhow::ensure!(closed, "fixture requires actual committed alias closure");
        Ok(Self(Arc::new(Mutex::new(AliasStatus::Acknowledged))))
    }
}
struct Closing {
    context: OwnedHookContext,
    fence: RetireFence,
    permanent: bool,
    physical_confirmed: bool,
}
enum Work {
    CloseAlias(LeafKey, AliasFence),
    Finish(Uuid),
    Release(Uuid),
    CloseLocal(crate::runtime::SharedPtyHandle),
}
struct PublicationJob {
    event: HookEvent,
    ticket: AdmissionTicket,
    sender: SyncSender<AcceptedHook>,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum PublicationState {
    Offered,
    Claimed,
    Finished,
    Abandoned,
}
struct Publication {
    state: Mutex<PublicationState>,
    wake: Condvar,
}

/// The worker retains its SQLite writer transaction while this envelope is
/// offered/claimed. Another process cannot retire/adopt before this effect ends.
pub(crate) struct AcceptedHook {
    pub event: HookEvent,
    ticket: AdmissionTicket,
    authority: Arc<HookAuthority>,
    publication: Arc<Publication>,
}
impl AcceptedHook {
    pub fn begin_publication(&self) -> std::result::Result<Claim, Denial> {
        let mut state = self
            .publication
            .state
            .lock()
            .map_err(|_| Denial::Poisoned)?;
        if *state != PublicationState::Offered {
            return Err(Denial::StaleClaim);
        }
        let claim = self.authority.validate_ui(&self.ticket)?;
        *state = PublicationState::Claimed;
        Ok(claim)
    }
}
impl Drop for AcceptedHook {
    fn drop(&mut self) {
        if let Ok(mut state) = self.publication.state.lock() {
            if *state != PublicationState::Abandoned {
                *state = PublicationState::Finished;
            }
            self.publication.wake.notify_one();
        }
    }
}

pub(crate) struct HookRuntime {
    pub owner: Uuid,
    pub authority: Arc<HookAuthority>,
    store: Option<MemoryStore>,
    claims: Mutex<HashMap<LeafKey, OwnedHookContext>>,
    closing: Mutex<HashMap<Uuid, Closing>>,
    cancelled: Mutex<HashSet<Uuid>>,
    aliases: Mutex<HashMap<(Uuid, Uuid), AliasFence>>,
    work: SyncSender<Work>,
    publications: SyncSender<PublicationJob>,
    local_queued: Mutex<HashMap<(Uuid, Option<Uuid>), bool>>,
    observed_local: Mutex<HashMap<(Uuid, Option<Uuid>), crate::runtime::SharedPtyHandle>>,
}
impl HookRuntime {
    fn new(owner: Uuid, store: Option<MemoryStore>) -> Arc<Self> {
        let (work, rx) = std::sync::mpsc::sync_channel(64);
        let (publications, pub_rx) = std::sync::mpsc::sync_channel(256);
        let runtime = Arc::new(Self {
            owner,
            authority: Arc::new(HookAuthority::default()),
            store,
            claims: Mutex::new(HashMap::new()),
            closing: Mutex::new(HashMap::new()),
            cancelled: Mutex::new(HashSet::new()),
            aliases: Mutex::new(HashMap::new()),
            work,
            publications,
            local_queued: Mutex::new(HashMap::new()),
            observed_local: Mutex::new(HashMap::new()),
        });
        let worker = Arc::clone(&runtime);
        let _ = std::thread::Builder::new()
            .name("hook-ledger".into())
            .spawn(move || worker.worker(rx));
        let worker = Arc::clone(&runtime);
        let _ = std::thread::Builder::new()
            .name("hook-publication".into())
            .spawn(move || worker.publication_worker(pub_rx));
        runtime
    }
    pub fn store(&self) -> Result<&MemoryStore> {
        self.store.as_ref().ok_or_else(|| {
            anyhow!("managed hook ledger unavailable; explicit schema cutover may be required")
        })
    }
    pub fn current_claim_for_runtime(&self, key: LeafKey, runtime: Uuid) -> Option<Claim> {
        self.claims
            .lock()
            .ok()?
            .get(&key)
            .filter(|c| c.claim.runtime == runtime && c.claim.phase == ClaimPhase::Active)
            .map(|c| c.claim)
    }
    pub fn admit(&self, identity: QueryIdentity) -> Result<AdmissionTicket> {
        let context = self.store()?.hook_resolve(identity, self.owner)?;
        Ok(self
            .authority
            .ticket(context)
            .map_err(|_| anyhow!("managed hook denied"))?)
    }
    pub fn publish(
        &self,
        event: HookEvent,
        ticket: AdmissionTicket,
        sender: SyncSender<AcceptedHook>,
    ) {
        if self
            .publications
            .try_send(PublicationJob {
                event,
                ticket,
                sender,
            })
            .is_err()
        {
            log::warn!("managed hook publication queue unavailable");
        }
    }
    fn register(&self, claim: Claim) -> Result<()> {
        let context = OwnedHookContext {
            claim,
            legacy_alias_epoch: None,
        };
        let same = self
            .claims
            .lock()
            .map_err(|_| anyhow!("claim registry poisoned"))?
            .get(&claim.key)
            .copied();
        if same.is_some_and(|old| same_authority(old.claim, claim)) {
            if claim.phase == ClaimPhase::Active {
                self.authority
                    .mark_installed(claim)
                    .map_err(|_| anyhow!("birth retired before installation"))?;
            }
        } else {
            self.authority
                .install(context)
                .map_err(|_| anyhow!("local hook authority unavailable"))?;
        }
        self.claims
            .lock()
            .map_err(|_| anyhow!("claim registry poisoned"))?
            .insert(claim.key, context);
        Ok(())
    }
    /// Runs on the runtime recovery worker before any Request::Spawn or local
    /// PTY factory. A retry retains the same authorized nonce and never consumes
    /// it on the client side for a remote host.
    pub fn authorize_birth(&self, key: LeafKey, runtime: Uuid) -> Result<Claim> {
        anyhow::ensure!(
            !self
                .cancelled
                .lock()
                .map_err(|_| anyhow!("cancel registry poisoned"))?
                .contains(&runtime),
            "birth cancelled before reservation"
        );
        anyhow::ensure!(
            !self
                .closing
                .lock()
                .map_err(|_| anyhow!("close registry poisoned"))?
                .values()
                .any(|c| c.context.claim.key == key),
            "previous birth still closing"
        );
        let store = self.store()?;
        let prior = store.hook_claim(key)?;
        if let Some(claim) = prior {
            if claim.owner == Some(self.owner)
                && claim.runtime == runtime
                && claim.phase == ClaimPhase::AuthorizedBirth
                && claim.generation.is_some()
            {
                self.register(claim)?;
                if self
                    .cancelled
                    .lock()
                    .map_err(|_| anyhow!("cancel registry poisoned"))?
                    .contains(&runtime)
                {
                    // A retry might already have emitted Spawn. Fence its local
                    // admission, but retain uncertainty until physical closure.
                    self.begin_close(runtime, true);
                    anyhow::bail!("retry cancelled before publication of authority");
                }
                return Ok(claim);
            }
            anyhow::ensure!(
                claim.phase == ClaimPhase::Retired,
                "birth requires confirmed prior retirement; no cold ownership guess"
            );
        }
        let receipt = store.hook_transition(
            self.owner,
            Uuid::new_v4(),
            &LedgerIntent::Reserve {
                expected: prior,
                key,
                birth: BirthBinding {
                    runtime,
                    generation: Uuid::new_v4(),
                },
                owner: self.owner,
            },
        )?;
        let claim = receipt
            .claim
            .ok_or_else(|| anyhow!("birth receipt missing"))?;
        self.register(claim)?;
        if self
            .cancelled
            .lock()
            .map_err(|_| anyhow!("cancel registry poisoned"))?
            .contains(&runtime)
        {
            // This is a fresh reserve, and the caller has not received authority
            // to invoke its factory or emit Spawn. This is an actual no-factory
            // cancellation, never inferred for an already-consumed retry.
            self.physical_close_confirmed(runtime);
            anyhow::bail!("birth cancelled after reserve and before factory");
        }
        Ok(claim)
    }
    pub fn attach(&self, key: LeafKey, host: HostEvidence, legacy_root: bool) -> Result<Claim> {
        anyhow::ensure!(
            !self
                .cancelled
                .lock()
                .map_err(|_| anyhow!("cancel registry poisoned"))?
                .contains(&host.runtime),
            "attachment cancelled"
        );
        anyhow::ensure!(
            host.target == Some(key.into()),
            "host identity unavailable or different; explicit host upgrade required"
        );
        let store = self.store()?;
        let prior = store.hook_claim(key)?;
        let intent = match prior {
            Some(expected)
                if host.generation.is_some()
                    && host.proves(expected)
                    && expected.owner == Some(self.owner)
                    && expected.phase == ClaimPhase::AuthorizedBirth =>
            {
                LedgerIntent::Install { expected, host }
            }
            Some(expected)
                if host.generation.is_some()
                    && host.proves(expected)
                    && expected.owner == Some(self.owner)
                    && expected.phase == ClaimPhase::Active =>
            {
                self.register(expected)?;
                return Ok(expected);
            }
            Some(expected) if host.generation.is_some() && host.proves(expected) => {
                LedgerIntent::Adopt {
                    expected,
                    host,
                    new_owner: self.owner,
                }
            }
            None if host.generation.is_some() => LedgerIntent::ObserveExisting {
                key,
                host,
                new_owner: self.owner,
            },
            None if host.generation.is_none() => LedgerIntent::ImportLegacy {
                key,
                runtime: host.runtime,
                owner: self.owner,
                single_original_root: legacy_root,
            },
            Some(expected)
                if host.generation.is_none()
                    && expected.generation.is_none()
                    && expected.runtime == host.runtime
                    && !expected.require_binding =>
            {
                LedgerIntent::AdoptLegacy {
                    expected,
                    new_owner: self.owner,
                }
            }
            _ => anyhow::bail!("attachment birth does not match durable authority"),
        };
        let claim = store
            .hook_transition(self.owner, Uuid::new_v4(), &intent)?
            .claim
            .ok_or_else(|| anyhow!("attachment receipt missing"))?;
        self.register(claim)?;
        Ok(claim)
    }
    /// UI-safe close admission. This is not a durable or host cancellation ACK.
    pub fn begin_close(&self, runtime: Uuid, permanent: bool) {
        if let Ok(mut cancelled) = self.cancelled.lock() {
            cancelled.insert(runtime);
        }
        let context = self.claims.lock().ok().and_then(|mut claims| {
            let key = claims
                .iter()
                .find(|(_, c)| c.claim.runtime == runtime)
                .map(|(k, _)| *k)?;
            claims.remove(&key)
        });
        if let Some(context) = context {
            if let Ok(fence) = self.authority.begin_retire(context.claim) {
                if let Ok(mut closing) = self.closing.lock() {
                    closing.insert(
                        runtime,
                        Closing {
                            context,
                            fence,
                            permanent,
                            physical_confirmed: false,
                        },
                    );
                }
            }
        }
    }
    /// Called only after the aggregated physical drain and durable raw tail ACK,
    /// or an actual fresh no-factory proof. Child wait alone is insufficient.
    pub fn physical_close_confirmed(&self, runtime: Uuid) {
        self.begin_close(runtime, true);
        if let Ok(mut closing) = self.closing.lock() {
            if let Some(closing) = closing.get_mut(&runtime) {
                closing.physical_confirmed = true;
            }
        }
        if self.work.try_send(Work::Finish(runtime)).is_err() {
            log::warn!("hook retirement queue unavailable; birth remains fenced");
        }
    }
    pub(crate) fn confirm_no_child_birth(
        &self,
        proof: crate::terminal::pty::NoChildAttempt,
    ) -> Result<()> {
        let claim = proof.claim();
        anyhow::ensure!(
            claim.owner == Some(self.owner),
            "no-child proof belongs to another endpoint"
        );
        self.prepare_physical_close_bound(HostEvidence {
            runtime: claim.runtime,
            generation: claim.generation,
            target: Some(claim.key.into()),
        })?;
        self.physical_close_confirmed(claim.runtime);
        Ok(())
    }
    /// Runs on the closer worker before any remote Kill/CloseBirth. Winning this
    /// exact owner/epoch CAS blocks adoption and Birth B until physical ACK and
    /// final Retire. A transferred endpoint cannot kill its former birth.
    pub fn prepare_physical_close(&self, runtime: Uuid) -> Result<()> {
        self.prepare_physical_close_inner(runtime, None)
    }
    fn prepare_physical_close_bound(&self, evidence: HostEvidence) -> Result<()> {
        self.prepare_physical_close_inner(evidence.runtime, Some(evidence))
    }
    fn begin_close_bound(&self, evidence: HostEvidence) -> Result<()> {
        let mut claims = self
            .claims
            .lock()
            .map_err(|_| anyhow!("claim registry poisoned"))?;
        let context = claims
            .values()
            .find(|context| {
                evidence.proves(context.claim) && context.claim.owner == Some(self.owner)
            })
            .copied();
        if let Some(context) = context {
            let mut closing = self
                .closing
                .lock()
                .map_err(|_| anyhow!("close registry poisoned"))?;
            anyhow::ensure!(
                !closing.contains_key(&evidence.runtime),
                "another birth already closing under this runtime ID"
            );
            let fence = self
                .authority
                .begin_retire(context.claim)
                .map_err(|_| anyhow!("birth effects cannot be fenced"))?;
            claims.remove(&context.claim.key);
            closing.insert(
                evidence.runtime,
                Closing {
                    context,
                    fence,
                    permanent: true,
                    physical_confirmed: false,
                },
            );
        } else {
            let closing = self
                .closing
                .lock()
                .map_err(|_| anyhow!("close registry poisoned"))?;
            anyhow::ensure!(
                closing
                    .get(&evidence.runtime)
                    .is_some_and(|closing| evidence.proves(closing.context.claim)
                        && closing.context.claim.owner == Some(self.owner)),
                "physical close binding no longer owned"
            );
        }
        self.cancelled
            .lock()
            .map_err(|_| anyhow!("cancel registry poisoned"))?
            .insert(evidence.runtime);
        Ok(())
    }
    fn prepare_physical_close_inner(
        &self,
        runtime: Uuid,
        evidence: Option<HostEvidence>,
    ) -> Result<()> {
        if let Some(evidence) = evidence {
            self.begin_close_bound(evidence)?;
        } else {
            self.begin_close(runtime, true);
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let prior = loop {
            let guard = self
                .closing
                .lock()
                .map_err(|_| anyhow!("close registry poisoned"))?;
            let closing = guard
                .get(&runtime)
                .ok_or_else(|| anyhow!("physical close lacks owned authority"))?;
            anyhow::ensure!(
                evidence.is_none_or(|evidence| evidence.proves(closing.context.claim)),
                "physical close birth changed"
            );
            if closing.fence.locally_drained() {
                break closing.context.claim;
            }
            drop(guard);
            anyhow::ensure!(
                std::time::Instant::now() < deadline,
                "hook effects still running; no close ACK"
            );
            std::thread::sleep(Duration::from_millis(10));
        };
        if prior.phase == ClaimPhase::Retiring {
            anyhow::ensure!(
                self.store()?.hook_claim(prior.key)? == Some(prior),
                "physical close fence changed"
            );
            return Ok(());
        }
        let current = self
            .store()?
            .hook_claim(prior.key)?
            .ok_or_else(|| anyhow!("closing target absent"))?;
        anyhow::ensure!(
            same_authority(prior, current),
            "physical close owner transferred"
        );
        let fenced = self
            .store()?
            .hook_transition(
                self.owner,
                Uuid::new_v4(),
                &LedgerIntent::FenceRetire { expected: current },
            )?
            .claim
            .ok_or_else(|| anyhow!("retirement fence receipt absent"))?;
        let mut closing = self
            .closing
            .lock()
            .map_err(|_| anyhow!("close registry poisoned"))?;
        let context = &mut closing
            .get_mut(&runtime)
            .ok_or_else(|| anyhow!("retirement fence local ownership lost"))?
            .context;
        anyhow::ensure!(
            same_authority(context.claim, prior),
            "local closing authority changed during durable fence"
        );
        context.claim = fenced;
        Ok(())
    }
    /// Register the actual published handle, not an invented child-exit event.
    pub fn observe_local(&self, handle: &crate::runtime::SharedPtyHandle) -> Result<()> {
        let pty = handle
            .lock()
            .map_err(|_| anyhow!("local observation poisoned"))?;
        if pty.is_remote() {
            return Ok(());
        }
        let evidence = pty.hook_host_evidence();
        if evidence.generation.is_none() || pty.hook_local_owner() != Some(self.owner) {
            return Ok(());
        }
        let mut observed = self
            .observed_local
            .lock()
            .map_err(|_| anyhow!("local observer poisoned"))?;
        anyhow::ensure!(
            observed.len() < 1_000_000,
            "local observer identity limit reached"
        );
        observed.insert(
            (evidence.runtime, evidence.generation),
            // Retain the physical handle even if its manager/window is dropped.
            // The worker will acquire the owner CAS before terminating it.
            Arc::clone(handle),
        );
        Ok(())
    }
    fn observed_local_handles(&self) -> Vec<(crate::runtime::SharedPtyHandle, bool)> {
        self.observed_local
            .lock()
            .map(|observed| {
                observed
                    .values()
                    .map(|handle| {
                        let abandoned = Arc::strong_count(handle) == 1;
                        (Arc::clone(handle), abandoned)
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    }
    /// Factory/recovery worker only. Preserve the handle on queue pressure;
    /// returning an error/absence may not drop a spawned managed child.
    pub fn request_local_close_from_factory(&self, handle: crate::runtime::SharedPtyHandle) {
        while self.request_local_close(Arc::clone(&handle)).is_err() {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    /// Actual aggregate receipts unblock a native backend cleanup. This is not
    /// a child-wait check or an inference from a missing published handle.
    pub fn local_close_completed(&self, runtime: Uuid) -> bool {
        if self
            .claims
            .lock()
            .map(|claims| claims.values().any(|c| c.claim.runtime == runtime))
            .unwrap_or(true)
        {
            return false;
        }
        let closing = match self.closing.lock() {
            Ok(closing) => closing
                .get(&runtime)
                .map(|closing| (closing.context.claim.generation, closing.physical_confirmed)),
            Err(_) => return false,
        };
        if let Some((generation, confirmed)) = closing {
            // Admission acquires local_queued before closing. Release this
            // snapshot's closing lock before looking up the aggregate receipt.
            return confirmed
                && self
                    .local_queued
                    .lock()
                    .is_ok_and(|queued| queued.get(&(runtime, generation)) == Some(&true));
        }
        self.local_queued.lock().is_ok_and(|queued| {
            queued
                .iter()
                .any(|((id, _), complete)| *id == runtime && *complete)
        })
    }
    /// UI-safe ownership handoff. No SQLite, fsync, wait, or device closure.
    /// The caller retains its handle when the bounded worker queue refuses it.
    pub fn request_local_close(&self, handle: crate::runtime::SharedPtyHandle) -> Result<()> {
        let mut pty = handle
            .lock()
            .map_err(|_| anyhow!("local retirement handle poisoned"))?;
        anyhow::ensure!(!pty.is_remote(), "local closer cannot certify remote birth");
        let evidence = pty.hook_host_evidence();
        let identity = (evidence.runtime, evidence.generation);
        let mut queued = self
            .local_queued
            .lock()
            .map_err(|_| anyhow!("local retire registry poisoned"))?;
        if queued.contains_key(&identity) {
            return Ok(());
        }
        anyhow::ensure!(
            queued.values().filter(|completed| !**completed).count() < 64,
            "outstanding local retirements full; handle must be retained"
        );
        anyhow::ensure!(
            queued.len() < 1_000_000,
            "local retirement identity limit reached; no tombstone eviction"
        );
        anyhow::ensure!(
            evidence.generation.is_some(),
            "local retirement requires managed birth identity"
        );
        self.begin_close_bound(evidence)?;
        pty.claim_retirement_log_owner();
        drop(pty);
        self.work
            .try_send(Work::CloseLocal(handle))
            .map_err(|_| anyhow!("local retirement queue unavailable; handle must be retained"))?;
        queued.insert(identity, false);
        Ok(())
    }
    pub fn release_endpoint(&self, runtime: Uuid) {
        self.begin_close(runtime, false);
        if self.work.try_send(Work::Release(runtime)).is_err() {
            log::warn!("hook release queue unavailable; endpoint remains fenced");
        }
    }
    pub fn request_close_alias(&self, key: LeafKey) -> AliasFence {
        let mut aliases = match self.aliases.lock() {
            Ok(v) => v,
            Err(_) => return AliasFence(Arc::new(Mutex::new(AliasStatus::Failed))),
        };
        if let Some(fence) = aliases.get(&(key.workspace, key.panel)) {
            return fence.clone();
        }
        let fence = AliasFence(Arc::new(Mutex::new(AliasStatus::Pending)));
        if self
            .work
            .try_send(Work::CloseAlias(key, fence.clone()))
            .is_err()
        {
            if let Ok(mut s) = fence.0.lock() {
                *s = AliasStatus::Failed;
            }
        }
        aliases.insert((key.workspace, key.panel), fence.clone());
        fence
    }
    fn finish(&self, runtime: Uuid, release: bool) -> Result<bool> {
        let guard = self
            .closing
            .lock()
            .map_err(|_| anyhow!("close registry poisoned"))?;
        let Some(closing) = guard.get(&runtime) else {
            return Ok(true);
        };
        if !closing.fence.locally_drained() {
            return Ok(false);
        }
        if closing.permanent && !closing.physical_confirmed {
            return Ok(false);
        }
        let prior = closing.context.claim;
        let permanent = closing.permanent;
        drop(guard);
        let current = self
            .store()?
            .hook_claim(prior.key)?
            .ok_or_else(|| anyhow!("closing target absent"))?;
        anyhow::ensure!(
            prior == current || same_authority(prior, current),
            "closing authority transferred"
        );
        let intent = if release && !permanent {
            LedgerIntent::Release { expected: current }
        } else {
            LedgerIntent::Retire { expected: current }
        };
        self.store()?
            .hook_transition(self.owner, Uuid::new_v4(), &intent)?;
        let mut closing = self
            .closing
            .lock()
            .map_err(|_| anyhow!("close registry poisoned"))?;
        anyhow::ensure!(
            closing
                .get(&runtime)
                .is_some_and(|closing| closing.context.claim == prior),
            "closing authority changed during finalization"
        );
        closing.remove(&runtime);
        Ok(true)
    }
    fn worker(&self, rx: Receiver<Work>) {
        let mut pending = Vec::new();
        let mut local = Vec::<LocalRetirement>::new();
        loop {
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(Work::CloseAlias(key, fence)) => {
                    let result = (|| -> Result<()> {
                        let store = self.store()?;
                        let (epoch, closed, root) = store.hook_alias_state(key)?;
                        if closed {
                            return Ok(());
                        }
                        store.hook_transition(
                            self.owner,
                            Uuid::new_v4(),
                            &LedgerIntent::CloseLegacyAlias {
                                workspace: key.workspace,
                                panel: key.panel,
                                expected_epoch: epoch,
                                root: root.unwrap_or(key.leaf),
                            },
                        )?;
                        Ok(())
                    })();
                    if let Ok(mut s) = fence.0.lock() {
                        *s = if result.is_ok() {
                            AliasStatus::Acknowledged
                        } else {
                            AliasStatus::Failed
                        };
                    }
                }
                Ok(Work::CloseLocal(handle)) => local.push(LocalRetirement::new(handle)),
                Ok(Work::Finish(id)) => {
                    if !pending.contains(&(id, false)) {
                        pending.push((id, false));
                    }
                }
                Ok(Work::Release(id)) => {
                    if !pending.contains(&(id, true)) {
                        pending.push((id, true));
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            }
            // Natural child/reader exit enters the same owner CAS, prefix,
            // aggregate drain and durable-tail coordinator as explicit close.
            let observed = self.observed_local_handles();
            for (handle, abandoned) in observed {
                let ended = abandoned || handle.try_lock().is_ok_and(|pty| !pty.alive());
                if ended {
                    let _ = self.request_local_close(handle);
                }
            }
            local.retain_mut(|retirement| !matches!(retirement.advance(self), Ok(true)));
            // Queue saturation cannot suppress a proved close forever. Poll
            // durable-finalization candidates as well as explicit wake items.
            let closings = self
                .closing
                .lock()
                .map(|closings| {
                    closings
                        .iter()
                        .filter_map(|(id, c)| {
                            (!c.permanent || c.physical_confirmed).then_some((*id, !c.permanent))
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            for closing in closings {
                if !pending.contains(&closing) {
                    pending.push(closing);
                }
            }
            pending.retain(|(id, release)| !matches!(self.finish(*id, *release), Ok(true)));
            // Refresh is for observation/cache invalidation. Effect publication
            // additionally holds the durable lock through the actual UI setter.
            let contexts = self
                .claims
                .lock()
                .map(|v| v.values().copied().collect::<Vec<_>>())
                .unwrap_or_default();
            for context in contexts {
                self.refresh_claim(context);
            }
        }
    }
    fn refresh_claim(&self, context: OwnedHookContext) {
        if !self
            .store()
            .and_then(|store| store.hook_claim(context.claim.key))
            .is_ok_and(|current| current.is_some_and(|claim| same_authority(claim, context.claim)))
        {
            self.authority.invalidate(context.claim);
            if let Ok(mut claims) = self.claims.lock() {
                // The durable read used a snapshot of A. A concurrent install
                // may already have registered B under the same leaf key.
                if claims
                    .get(&context.claim.key)
                    .is_some_and(|current| same_authority(current.claim, context.claim))
                {
                    claims.remove(&context.claim.key);
                }
            }
        }
    }
    fn publication_worker(&self, rx: Receiver<PublicationJob>) {
        // A birth awaiting installation does not occupy the only worker. The
        // local retry ring and incoming channel are each bounded at 256 jobs.
        let mut pending = std::collections::VecDeque::new();
        loop {
            if pending.is_empty() {
                match rx.recv() {
                    Ok(job) => pending.push_back(job),
                    Err(_) => return,
                }
            }
            while pending.len() < 256 {
                match rx.try_recv() {
                    Ok(job) => pending.push_back(job),
                    Err(_) => break,
                }
            }
            let count = pending.len();
            for _ in 0..count {
                let job = pending.pop_front().expect("bounded publication ring");
                match self.publication_attempt(&job) {
                    Ok(true) => {}
                    Ok(false) => pending.push_back(job),
                    Err(_) => self.authority.invalidate(job.ticket.context().claim),
                }
            }
            if !pending.is_empty() {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
    fn publication_attempt(&self, job: &PublicationJob) -> Result<bool> {
        match self.authority.validate_ui(&job.ticket) {
            Err(Denial::PendingBirth) => return Ok(false),
            Err(_) => return Ok(true),
            Ok(_) => {}
        }
        self.store().and_then(|store| {
            store.with_hook_publication(job.ticket.context(), || {
                let publication = Arc::new(Publication {
                    state: Mutex::new(PublicationState::Offered),
                    wake: Condvar::new(),
                });
                let envelope = AcceptedHook {
                    event: job.event.clone(),
                    ticket: job.ticket.clone(),
                    authority: Arc::clone(&self.authority),
                    publication: Arc::clone(&publication),
                };
                match job.sender.try_send(envelope) {
                    Ok(()) => {}
                    Err(TrySendError::Full(_)) => return Ok(false),
                    Err(TrySendError::Disconnected(_)) => return Ok(true),
                }
                let mut state = publication
                    .state
                    .lock()
                    .map_err(|_| anyhow!("publication poisoned"))?;
                loop {
                    match *state {
                        PublicationState::Finished => return Ok(true),
                        PublicationState::Abandoned => return Ok(false),
                        PublicationState::Claimed => {
                            state = publication
                                .wake
                                .wait(state)
                                .map_err(|_| anyhow!("publication poisoned"))?
                        }
                        PublicationState::Offered => {
                            let (next, wait) = publication
                                .wake
                                .wait_timeout(state, Duration::from_millis(100))
                                .map_err(|_| anyhow!("publication poisoned"))?;
                            state = next;
                            if wait.timed_out() && *state == PublicationState::Offered {
                                *state = PublicationState::Abandoned;
                                return Ok(false);
                            }
                        }
                    }
                }
            })
        })
    }
}

struct LocalRetirement {
    handle: crate::runtime::SharedPtyHandle,
    durable: Vec<u8>,
    dest_seq: u64,
    prefix_captured: bool,
    draining: bool,
    fenced: bool,
    journal: Option<std::path::PathBuf>,
}
impl LocalRetirement {
    fn new(handle: crate::runtime::SharedPtyHandle) -> Self {
        Self {
            handle,
            durable: Vec::new(),
            dest_seq: 0,
            prefix_captured: false,
            draining: false,
            fenced: false,
            journal: None,
        }
    }
    fn append(&mut self) -> Result<()> {
        if self.durable.is_empty() {
            return Ok(());
        }
        let path = self
            .journal
            .as_ref()
            .ok_or_else(|| anyhow!("retirement journal absent"))?;
        let existing = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        anyhow::ensure!(
            existing.saturating_add(self.durable.len() as u64) <= 16 * 1024 * 1024,
            "retirement journal limit reached; no ACK"
        );
        crate::state::scrollback_log::append_frames(path, &self.durable)?;
        self.durable.clear(); // Actual complete append + sync_data, not submission.
        Ok(())
    }
    fn advance(&mut self, runtime: &HookRuntime) -> Result<bool> {
        let evidence = self
            .handle
            .lock()
            .map_err(|_| anyhow!("retirement poisoned"))?
            .hook_host_evidence();
        if !self.fenced {
            runtime.prepare_physical_close_bound(evidence)?;
            self.fenced = true;
        }
        if self.journal.is_none() {
            let dir = runtime
                .store()?
                .path()
                .parent()
                .ok_or_else(|| anyhow!("memory parent absent"))?
                .join("hook-retirement");
            std::fs::create_dir_all(&dir)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
            }
            let nonce = evidence
                .generation
                .ok_or_else(|| anyhow!("retirement nonce absent"))?;
            self.journal = Some(dir.join(format!("{nonce}.mtlg")));
        }
        if !self.prefix_captured {
            let ready = self
                .handle
                .lock()
                .map_err(|_| anyhow!("retirement poisoned"))?
                .take_retirement_prefix(&mut self.durable, &mut self.dest_seq, true)?;
            if !ready {
                return Ok(false);
            }
            self.prefix_captured = true;
        }
        self.append()?;
        if !self.draining {
            self.handle
                .lock()
                .map_err(|_| anyhow!("retirement poisoned"))?
                .begin_retirement_drain()?;
            self.draining = true;
        }
        // Sample the physical boundary BEFORE the final transfer. A tail read
        // after an earlier empty sample must never hide output produced later.
        let physical = self
            .handle
            .lock()
            .map_err(|_| anyhow!("retirement poisoned"))?
            .retirement_physical_complete();
        let tail = self
            .handle
            .lock()
            .map_err(|_| anyhow!("retirement poisoned"))?
            .take_retirement_tail(&mut self.durable, &mut self.dest_seq, true)?;
        self.append()?;
        if physical && tail && self.durable.is_empty() {
            runtime.physical_close_confirmed(evidence.runtime);
            if let Ok(mut queued) = runtime.local_queued.lock() {
                queued.insert((evidence.runtime, evidence.generation), true);
            }
            if let Ok(mut observed) = runtime.observed_local.lock() {
                observed.remove(&(evidence.runtime, evidence.generation));
            }
            return Ok(true);
        }
        Ok(false)
    }
}

#[cfg(test)]
#[path = "hook_runtime_fixtures.rs"]
mod tests;

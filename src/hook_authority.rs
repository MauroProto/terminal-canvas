//! Birth authority with local effects permits and durable ownership claims.
//! A permit prevents local replacement from admitting more work; every SQL transaction
//! still validates its own claim because a different endpoint may have transferred it.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) struct LeafKey {
    pub workspace: Uuid,
    pub panel: Uuid,
    pub leaf: Uuid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct BirthBinding {
    pub runtime: Uuid,
    pub generation: Uuid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) enum ClaimPhase {
    AuthorizedBirth,
    Active,
    Detached,
    Retiring,
    Retired,
}

impl ClaimPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AuthorizedBirth => "authorized_birth",
            Self::Active => "active",
            Self::Detached => "detached",
            Self::Retiring => "retiring",
            Self::Retired => "retired",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) struct Claim {
    pub key: LeafKey,
    pub runtime: Uuid,
    pub generation: Option<Uuid>,
    pub epoch: i64,
    pub owner: Option<Uuid>,
    pub phase: ClaimPhase,
    pub require_binding: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct HostHookTarget {
    pub workspace_id: Uuid,
    pub panel_id: Uuid,
    pub leaf_id: Uuid,
}

impl From<LeafKey> for HostHookTarget {
    fn from(key: LeafKey) -> Self {
        Self {
            workspace_id: key.workspace,
            panel_id: key.panel,
            leaf_id: key.leaf,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct HostEvidence {
    pub runtime: Uuid,
    pub generation: Option<Uuid>,
    pub target: Option<HostHookTarget>,
}

impl HostEvidence {
    pub fn proves(self, claim: Claim) -> bool {
        claim.generation.is_some()
            && self.runtime == claim.runtime
            && self.generation == claim.generation
            && self.target == Some(claim.key.into())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Denial {
    BindingInvalid,
    TargetInvalid,
    UnknownTarget,
    Quarantined,
    StaleClaim,
    Retiring,
    PendingBirth,
    Poisoned,
}

/// Raw options preserve omitted versus empty binding fields. No parse error
/// becomes None, and denied managed identities cannot become unscoped memory.
pub(crate) struct RawHookIdentity<'a> {
    pub workspace: Option<&'a str>,
    pub panel: Option<&'a str>,
    pub leaf: Option<&'a str>,
    pub runtime: Option<&'a str>,
    pub generation: Option<&'a str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum QueryIdentity {
    Bound {
        key: LeafKey,
        binding: BirthBinding,
    },
    LegacyTarget {
        workspace: Option<Uuid>,
        panel: Uuid,
        leaf: Option<Uuid>,
    },
    UnscopedMemory,
}

fn uuid_field(value: Option<&str>) -> Result<Uuid, Denial> {
    value
        .filter(|raw| !raw.is_empty())
        .and_then(|raw| Uuid::parse_str(raw).ok())
        .ok_or(Denial::TargetInvalid)
}

impl RawHookIdentity<'_> {
    pub fn classify(self) -> Result<QueryIdentity, Denial> {
        if self.runtime.is_some() || self.generation.is_some() {
            let binding = BirthBinding {
                runtime: uuid_field(self.runtime).map_err(|_| Denial::BindingInvalid)?,
                generation: uuid_field(self.generation).map_err(|_| Denial::BindingInvalid)?,
            };
            return Ok(QueryIdentity::Bound {
                key: LeafKey {
                    workspace: uuid_field(self.workspace)?,
                    panel: uuid_field(self.panel)?,
                    leaf: uuid_field(self.leaf)?,
                },
                binding,
            });
        }
        let empty = |raw: Option<&str>| raw.is_none_or(str::is_empty);
        if empty(self.workspace) && empty(self.panel) && empty(self.leaf) {
            return Ok(QueryIdentity::UnscopedMemory);
        }
        // Legacy supports omitted workspace/leaf, not provided malformed values.
        let optional = |raw| match raw {
            None => Ok(None),
            Some(value) => uuid_field(Some(value)).map(Some),
        };
        Ok(QueryIdentity::LegacyTarget {
            workspace: optional(self.workspace)?,
            panel: uuid_field(self.panel)?,
            leaf: optional(self.leaf)?,
        })
    }
}

/// Context cannot encode a missing/failed managed claim as a legacy fallback.
#[derive(Debug, Clone, Copy)]
pub(crate) struct OwnedHookContext {
    pub claim: Claim,
    pub legacy_alias_epoch: Option<i64>,
}

#[derive(Debug)]
struct GateState {
    open: bool,
    in_flight: usize,
    installed: bool,
}

#[derive(Debug)]
struct Gate {
    claim: Claim,
    state: Mutex<GateState>,
}

#[derive(Debug, Clone)]
pub(crate) struct AdmissionTicket {
    gate: Arc<Gate>,
    context: OwnedHookContext,
}

/// Drop ends only the real work. An HTTP future must not own this permit if
/// another job continues after it is dropped. This type is intentionally not Clone.
pub(crate) struct EffectsPermit {
    gate: Arc<Gate>,
    context: OwnedHookContext,
}

impl EffectsPermit {
    pub fn context(&self) -> &OwnedHookContext {
        &self.context
    }
}

impl Drop for EffectsPermit {
    fn drop(&mut self) {
        if let Ok(mut state) = self.gate.state.lock() {
            state.in_flight = state.in_flight.saturating_sub(1);
        }
    }
}

#[derive(Clone)]
pub(crate) struct RetireFence {
    gate: Arc<Gate>,
}

impl RetireFence {
    /// Readiness is not a durable receipt. Birth B also needs the SQL receipt.
    pub fn locally_drained(&self) -> bool {
        self.gate
            .state
            .lock()
            .is_ok_and(|state| !state.open && state.in_flight == 0)
    }
}

#[derive(Default)]
pub(crate) struct HookAuthority {
    registry: Mutex<HashMap<LeafKey, Arc<Gate>>>,
}

impl HookAuthority {
    /// Caller must supply the current durable receipt and host proof. Registration
    /// replaces an exact closed local claim, never an open or different target.
    pub fn install(&self, context: OwnedHookContext) -> Result<(), Denial> {
        let claim = context.claim;
        if claim.owner.is_none()
            || !matches!(
                claim.phase,
                ClaimPhase::Active | ClaimPhase::AuthorizedBirth
            )
        {
            return Err(Denial::StaleClaim);
        }
        if claim.generation.is_none() && claim.require_binding {
            return Err(Denial::Quarantined);
        }
        if claim.generation.is_some() && context.legacy_alias_epoch.is_some() {
            return Err(Denial::TargetInvalid);
        }
        let mut registry = self.registry.lock().map_err(|_| Denial::Poisoned)?;
        if let Some(previous) = registry.get(&claim.key) {
            let state = previous.state.lock().map_err(|_| Denial::Poisoned)?;
            if state.open || state.in_flight != 0 {
                return Err(Denial::Retiring);
            }
            if previous.claim.epoch >= claim.epoch {
                return Err(Denial::StaleClaim);
            }
        }
        registry.insert(
            claim.key,
            Arc::new(Gate {
                claim,
                state: Mutex::new(GateState {
                    open: true,
                    in_flight: 0,
                    installed: claim.phase == ClaimPhase::Active,
                }),
            }),
        );
        Ok(())
    }

    pub fn ticket(&self, context: OwnedHookContext) -> Result<AdmissionTicket, Denial> {
        let registry = self.registry.lock().map_err(|_| Denial::Poisoned)?;
        let gate = registry
            .get(&context.claim.key)
            .ok_or(Denial::UnknownTarget)?;
        if !same_authority(gate.claim, context.claim) {
            return Err(Denial::StaleClaim);
        }
        if !gate.state.lock().map_err(|_| Denial::Poisoned)?.open {
            return Err(Denial::Retiring);
        }
        Ok(AdmissionTicket {
            gate: gate.clone(),
            context,
        })
    }

    pub fn begin_retire(&self, expected: Claim) -> Result<RetireFence, Denial> {
        let registry = self.registry.lock().map_err(|_| Denial::Poisoned)?;
        let gate = registry.get(&expected.key).ok_or(Denial::UnknownTarget)?;
        if !same_authority(gate.claim, expected) {
            return Err(Denial::StaleClaim);
        }
        gate.state.lock().map_err(|_| Denial::Poisoned)?.open = false;
        Ok(RetireFence { gate: gate.clone() })
    }

    /// Used immediately before UI setter/cache/activity; tickets do not remap leaf aliases.
    pub fn validate_ui(&self, ticket: &AdmissionTicket) -> Result<Claim, Denial> {
        let registry = self.registry.lock().map_err(|_| Denial::Poisoned)?;
        let current = registry
            .get(&ticket.context.claim.key)
            .ok_or(Denial::UnknownTarget)?;
        if !Arc::ptr_eq(current, &ticket.gate)
            || !same_authority(current.claim, ticket.context.claim)
        {
            return Err(Denial::StaleClaim);
        }
        let state = current.state.lock().map_err(|_| Denial::Poisoned)?;
        if !state.open {
            return Err(Denial::Retiring);
        }
        if !state.installed {
            return Err(Denial::PendingBirth);
        }
        Ok(Claim {
            phase: ClaimPhase::Active,
            ..current.claim
        })
    }

    /// The first hook may arrive between consume and Attached. Activate that
    /// very gate, preserving its tickets and in-flight effects. Never reopen a
    /// gate that retire/endpoint release already closed.
    pub fn mark_installed(&self, active: Claim) -> Result<(), Denial> {
        if active.phase != ClaimPhase::Active {
            return Err(Denial::StaleClaim);
        }
        let registry = self.registry.lock().map_err(|_| Denial::Poisoned)?;
        let gate = registry.get(&active.key).ok_or(Denial::UnknownTarget)?;
        if !same_authority(gate.claim, active) {
            return Err(Denial::StaleClaim);
        }
        let mut state = gate.state.lock().map_err(|_| Denial::Poisoned)?;
        if !state.open {
            return Err(Denial::Retiring);
        }
        state.installed = true;
        Ok(())
    }

    pub fn invalidate(&self, expected: Claim) {
        let _ = self.begin_retire(expected);
    }
}

pub(crate) fn same_authority(a: Claim, b: Claim) -> bool {
    a.key == b.key
        && a.runtime == b.runtime
        && a.generation == b.generation
        && a.epoch == b.epoch
        && a.owner == b.owner
        && a.require_binding == b.require_binding
        && matches!(a.phase, ClaimPhase::AuthorizedBirth | ClaimPhase::Active)
        && matches!(b.phase, ClaimPhase::AuthorizedBirth | ClaimPhase::Active)
}

impl AdmissionTicket {
    pub fn context(&self) -> &OwnedHookContext {
        &self.context
    }
    pub fn begin_effects(&self) -> Result<EffectsPermit, Denial> {
        let mut state = self.gate.state.lock().map_err(|_| Denial::Poisoned)?;
        if !state.open {
            return Err(Denial::Retiring);
        }
        state.in_flight = state.in_flight.checked_add(1).ok_or(Denial::Poisoned)?;
        Ok(EffectsPermit {
            gate: self.gate.clone(),
            context: self.context,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claim(epoch: i64) -> Claim {
        Claim {
            key: LeafKey {
                workspace: Uuid::new_v4(),
                panel: Uuid::new_v4(),
                leaf: Uuid::new_v4(),
            },
            runtime: Uuid::new_v4(),
            generation: Some(Uuid::new_v4()),
            epoch,
            owner: Some(Uuid::new_v4()),
            phase: ClaimPhase::Active,
            require_binding: true,
        }
    }

    #[test]
    fn effects_permission_is_at_work_start_and_retire_does_not_cancel_real_work() {
        let authority = HookAuthority::default();
        let a = claim(1);
        let context = OwnedHookContext {
            claim: a,
            legacy_alias_epoch: None,
        };
        authority.install(context).unwrap();
        let queued = authority.ticket(context).unwrap();
        let running = queued.begin_effects().unwrap();
        let fence = authority.begin_retire(a).unwrap();
        assert!(!fence.locally_drained());
        assert!(matches!(queued.begin_effects(), Err(Denial::Retiring)));
        assert!(matches!(
            authority.validate_ui(&queued),
            Err(Denial::Retiring)
        ));
        drop(running);
        assert!(fence.locally_drained());
        let b = Claim {
            epoch: 2,
            runtime: Uuid::new_v4(),
            generation: Some(Uuid::new_v4()),
            ..a
        };
        authority
            .install(OwnedHookContext {
                claim: b,
                legacy_alias_epoch: None,
            })
            .unwrap();
        assert!(matches!(
            authority.validate_ui(&queued),
            Err(Denial::StaleClaim)
        ));
    }

    #[test]
    fn invalid_binding_cannot_become_unscoped_memory() {
        let raw = |runtime, generation| RawHookIdentity {
            workspace: None,
            panel: None,
            leaf: None,
            runtime,
            generation,
        };
        assert_eq!(
            raw(None, None).classify(),
            Ok(QueryIdentity::UnscopedMemory)
        );
        assert_eq!(raw(Some(""), None).classify(), Err(Denial::BindingInvalid));
        assert_eq!(
            raw(Some("bad"), Some("bad")).classify(),
            Err(Denial::BindingInvalid)
        );
        assert_eq!(
            RawHookIdentity {
                workspace: Some(""),
                panel: Some(""),
                leaf: None,
                runtime: None,
                generation: None
            }
            .classify(),
            Ok(QueryIdentity::UnscopedMemory)
        );
        assert_eq!(
            RawHookIdentity {
                workspace: None,
                panel: Some("bad"),
                leaf: None,
                runtime: None,
                generation: None
            }
            .classify(),
            Err(Denial::TargetInvalid)
        );
    }
}

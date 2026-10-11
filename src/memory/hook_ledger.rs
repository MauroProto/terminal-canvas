//! Durable managed-hook ledger candidate. Call only inside the store's real
//! BEGIN IMMEDIATE transaction; no independent database or captured bool guard.

use anyhow::{anyhow, bail, ensure, Result};
use rusqlite::{functions::FunctionFlags, params, Connection, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::hook_authority::{
    BirthBinding, Claim, ClaimPhase, HostEvidence, LeafKey, OwnedHookContext,
};

const MAX_GENERATIONS: i64 = 1_000_000;
const WRITER_FUNCTION: &str = "tc_memory_writer_v3";

/// Unlike a version bump alone, triggers calling this function also fence a
/// baseline connection already open before migration. Baseline has no function
/// and cannot write protected tables after the migration commit. This is a
/// compatibility fence for cooperating software, not process authentication.
pub(super) fn register_writer(conn: &Connection) -> Result<()> {
    conn.create_scalar_function(
        WRITER_FUNCTION,
        0,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        |_| Ok(1_i64),
    )?;
    Ok(())
}

pub(super) fn migrate_v3(tx: &Transaction<'_>) -> Result<()> {
    tx.execute_batch("CREATE TABLE IF NOT EXISTS tc_hook_leaf_authority (
        workspace_id TEXT NOT NULL, panel_id TEXT NOT NULL, leaf_id TEXT NOT NULL,
        epoch INTEGER NOT NULL CHECK(epoch > 0), owner_id TEXT, runtime_id TEXT NOT NULL,
        generation TEXT, phase TEXT NOT NULL CHECK(phase IN ('authorized_birth','active','detached','retiring','retired')),
        require_binding INTEGER NOT NULL CHECK(require_binding IN (0,1)),
        CHECK(phase NOT IN ('authorized_birth','active') OR owner_id IS NOT NULL),
        PRIMARY KEY(workspace_id,panel_id,leaf_id));
      CREATE TABLE IF NOT EXISTS tc_hook_generations (
        generation TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, panel_id TEXT NOT NULL,
        leaf_id TEXT NOT NULL, runtime_id TEXT NOT NULL, birth_owner_id TEXT NOT NULL,
        born_epoch INTEGER NOT NULL CHECK(born_epoch > 0),
        spawn_consumed INTEGER NOT NULL CHECK(spawn_consumed IN (0,1)),
        phase TEXT NOT NULL CHECK(phase IN ('authorized_birth','active','retired')),
        retired_epoch INTEGER);
      CREATE TABLE IF NOT EXISTS tc_hook_panel_legacy_policy (
        workspace_id TEXT NOT NULL, panel_id TEXT NOT NULL,
        alias_epoch INTEGER NOT NULL CHECK(alias_epoch > 0), root_leaf_id TEXT NOT NULL,
        legacy_root_alias_closed INTEGER NOT NULL CHECK(legacy_root_alias_closed IN (0,1)),
        PRIMARY KEY(workspace_id,panel_id));
      CREATE TABLE IF NOT EXISTS tc_hook_operation_receipts (
        owner_id TEXT NOT NULL, operation_id TEXT NOT NULL, intent_json TEXT NOT NULL,
        receipt_json TEXT NOT NULL, PRIMARY KEY(owner_id,operation_id));")?;
    // Every baseline memory mutation reaches at least one ordinary table before
    // FTS. SQLite disallows triggers on FTS virtual tables; direct external FTS or
    // DDL clients are outside this cooperating-binary guarantee.
    for table in [
        "schema_migrations",
        "projects",
        "tasks",
        "memories",
        "memory_revisions",
        "handoffs",
        "mutation_receipts",
        "tc_hook_leaf_authority",
        "tc_hook_generations",
        "tc_hook_panel_legacy_policy",
        "tc_hook_operation_receipts",
    ] {
        for action in ["INSERT", "UPDATE", "DELETE"] {
            tx.execute_batch(&format!(
                "CREATE TRIGGER IF NOT EXISTS tc_v3_{table}_{action}
                BEFORE {action} ON {table} BEGIN
                SELECT CASE WHEN tc_memory_writer_v3() != 1
                THEN RAISE(ABORT, 'memory writer requires schema v3') END; END;"
            ))?;
        }
    }
    // Reinforce sticky policies against accidental new-code overwrite. A stale
    // layout/default must not erase history even if its connection is v3-aware.
    tx.execute_batch("CREATE TRIGGER IF NOT EXISTS tc_no_generation_revive
      BEFORE UPDATE ON tc_hook_generations WHEN
        OLD.phase = 'retired' OR
        NEW.spawn_consumed < OLD.spawn_consumed OR
        NEW.generation != OLD.generation OR NEW.runtime_id != OLD.runtime_id OR
        NEW.workspace_id != OLD.workspace_id OR NEW.panel_id != OLD.panel_id OR
        NEW.leaf_id != OLD.leaf_id OR NEW.birth_owner_id != OLD.birth_owner_id OR
        NEW.born_epoch != OLD.born_epoch
      BEGIN SELECT RAISE(ABORT, 'hook birth is immutable'); END;
      CREATE TRIGGER IF NOT EXISTS tc_no_generation_delete
      BEFORE DELETE ON tc_hook_generations BEGIN SELECT RAISE(ABORT, 'hook tombstones retained'); END;
      CREATE TRIGGER IF NOT EXISTS tc_no_leaf_policy_reset
      BEFORE UPDATE ON tc_hook_leaf_authority WHEN
        NEW.require_binding < OLD.require_binding OR NEW.epoch < OLD.epoch
      BEGIN SELECT RAISE(ABORT, 'hook policy is monotonic'); END;
      CREATE TRIGGER IF NOT EXISTS tc_no_leaf_history_delete
      BEFORE DELETE ON tc_hook_leaf_authority BEGIN SELECT RAISE(ABORT, 'hook history retained'); END;
      CREATE TRIGGER IF NOT EXISTS tc_no_alias_policy_reset
      BEFORE UPDATE ON tc_hook_panel_legacy_policy WHEN
        NEW.legacy_root_alias_closed < OLD.legacy_root_alias_closed OR
        NEW.alias_epoch < OLD.alias_epoch OR NEW.root_leaf_id != OLD.root_leaf_id
      BEGIN SELECT RAISE(ABORT, 'legacy alias is sticky'); END;
      CREATE TRIGGER IF NOT EXISTS tc_no_alias_policy_delete
      BEFORE DELETE ON tc_hook_panel_legacy_policy BEGIN SELECT RAISE(ABORT, 'legacy alias retained'); END;")?;
    Ok(())
}

type StoredClaimRow = (i64, Option<String>, String, Option<String>, String, bool);

pub(super) fn load_claim(tx: &Transaction<'_>, key: LeafKey) -> Result<Option<Claim>> {
    let raw: Option<StoredClaimRow> = tx.query_row(
        "SELECT epoch,owner_id,runtime_id,generation,phase,require_binding FROM tc_hook_leaf_authority
         WHERE workspace_id=?1 AND panel_id=?2 AND leaf_id=?3",
        params![key.workspace.to_string(), key.panel.to_string(), key.leaf.to_string()],
        |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?))).optional()?;
    raw.map(
        |(epoch, owner, runtime, generation, phase, require_binding)| {
            let phase = match phase.as_str() {
                "authorized_birth" => ClaimPhase::AuthorizedBirth,
                "active" => ClaimPhase::Active,
                "detached" => ClaimPhase::Detached,
                "retiring" => ClaimPhase::Retiring,
                "retired" => ClaimPhase::Retired,
                _ => bail!("invalid managed hook phase"),
            };
            Ok(Claim {
                key,
                epoch,
                owner: owner.map(|v| Uuid::parse_str(&v)).transpose()?,
                runtime: Uuid::parse_str(&runtime)?,
                generation: generation.map(|v| Uuid::parse_str(&v)).transpose()?,
                phase,
                require_binding,
            })
        },
    )
    .transpose()
}

fn require_exact(tx: &Transaction<'_>, expected: Claim) -> Result<()> {
    ensure!(
        load_claim(tx, expected.key)? == Some(expected),
        "managed hook ownership lost"
    );
    Ok(())
}

fn generation_live(tx: &Transaction<'_>, claim: Claim, require_consumed: bool) -> Result<()> {
    let generation = claim
        .generation
        .ok_or_else(|| anyhow::anyhow!("managed birth binding missing"))?;
    let found: Option<(String, String, String, String, bool, String)> = tx
        .query_row(
            "SELECT workspace_id,panel_id,leaf_id,runtime_id,spawn_consumed,phase
         FROM tc_hook_generations WHERE generation=?1",
            params![generation.to_string()],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .optional()?;
    let Some((w, p, l, r, consumed, phase)) = found else {
        bail!("managed birth not registered");
    };
    ensure!(
        w == claim.key.workspace.to_string()
            && p == claim.key.panel.to_string()
            && l == claim.key.leaf.to_string()
            && r == claim.runtime.to_string()
            && phase != "retired"
            && (!require_consumed || consumed),
        "managed birth no longer authorized"
    );
    Ok(())
}

/// Must be the first effectful step inside EVERY transaction/retry, including
/// context's project/task upserts and search_visible's visible_scopes upserts.
pub(super) fn validate_owned(tx: &Transaction<'_>, context: &OwnedHookContext) -> Result<()> {
    let claim = context.claim;
    let current =
        load_claim(tx, claim.key)?.ok_or_else(|| anyhow!("managed hook target missing"))?;
    ensure!(
        crate::hook_authority::same_authority(current, claim),
        "managed hook ownership lost"
    );
    ensure!(
        claim.owner.is_some()
            && matches!(
                claim.phase,
                ClaimPhase::AuthorizedBirth | ClaimPhase::Active
            ),
        "managed hook claim inactive"
    );
    if claim.generation.is_some() {
        ensure!(
            context.legacy_alias_epoch.is_none(),
            "bound hook cannot use legacy alias"
        );
        generation_live(tx, claim, true)?;
    } else {
        ensure!(
            !claim.require_binding && claim.phase == ClaimPhase::Active,
            "legacy hook quarantined"
        );
        if let Some(epoch) = context.legacy_alias_epoch {
            let alias: Option<(i64,String,bool)> = tx.query_row(
                "SELECT alias_epoch,root_leaf_id,legacy_root_alias_closed FROM tc_hook_panel_legacy_policy
                 WHERE workspace_id=?1 AND panel_id=?2",
                params![claim.key.workspace.to_string(),claim.key.panel.to_string()],
                |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?))).optional()?;
            ensure!(
                alias == Some((epoch, claim.key.leaf.to_string(), false)),
                "legacy root alias closed"
            );
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) enum LedgerIntent {
    Reserve {
        expected: Option<Claim>,
        key: LeafKey,
        birth: BirthBinding,
        owner: Uuid,
    },
    Install {
        expected: Claim,
        host: HostEvidence,
    },
    Retire {
        expected: Claim,
    },
    /// Fences adoption and replacement before a physical close starts. Retire
    /// is committed only after real OS/host closure and local effects drain.
    FenceRetire {
        expected: Claim,
    },
    Release {
        expected: Claim,
    },
    Adopt {
        expected: Claim,
        host: HostEvidence,
        new_owner: Uuid,
    },
    AdoptLegacy {
        expected: Claim,
        new_owner: Uuid,
    },
    ObserveExisting {
        key: LeafKey,
        host: HostEvidence,
        new_owner: Uuid,
    },
    ImportLegacy {
        key: LeafKey,
        runtime: Uuid,
        owner: Uuid,
        single_original_root: bool,
    },
    CloseLegacyAlias {
        workspace: Uuid,
        panel: Uuid,
        expected_epoch: Option<i64>,
        root: Uuid,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct LedgerReceipt {
    pub claim: Option<Claim>,
    pub alias_epoch: Option<i64>,
}

/// A historical receipt proves only that an intent committed. Before effects,
/// install or spawn, callers must revalidate the current row for that receipt.
pub(super) fn apply_intent(
    tx: &Transaction<'_>,
    owner: Uuid,
    operation: Uuid,
    intent: &LedgerIntent,
) -> Result<LedgerReceipt> {
    let actor_matches = match *intent {
        LedgerIntent::Reserve {
            owner: proposed, ..
        }
        | LedgerIntent::ImportLegacy {
            owner: proposed, ..
        } => owner == proposed,
        LedgerIntent::Install { expected, .. }
        | LedgerIntent::Retire { expected }
        | LedgerIntent::FenceRetire { expected }
        | LedgerIntent::Release { expected } => expected.owner == Some(owner),
        LedgerIntent::Adopt { new_owner, .. }
        | LedgerIntent::AdoptLegacy { new_owner, .. }
        | LedgerIntent::ObserveExisting { new_owner, .. } => new_owner == owner,
        LedgerIntent::CloseLegacyAlias { .. } => true,
    };
    ensure!(actor_matches, "managed hook intent owner mismatch");
    let intent_json = serde_json::to_string(intent)?;
    let cached: Option<(String,String)> = tx.query_row(
        "SELECT intent_json,receipt_json FROM tc_hook_operation_receipts WHERE owner_id=?1 AND operation_id=?2",
        params![owner.to_string(),operation.to_string()], |row| Ok((row.get(0)?,row.get(1)?))).optional()?;
    if let Some((prior, json)) = cached {
        ensure!(prior == intent_json, "hook operation intent conflict");
        return Ok(serde_json::from_str(&json)?);
    }
    let receipt = transition(tx, intent)?;
    tx.execute(
        "INSERT INTO tc_hook_operation_receipts(owner_id,operation_id,intent_json,receipt_json)
        VALUES (?1,?2,?3,?4)",
        params![
            owner.to_string(),
            operation.to_string(),
            intent_json,
            serde_json::to_string(&receipt)?
        ],
    )?;
    Ok(receipt)
}

fn insert_claim(tx: &Transaction<'_>, claim: Claim) -> Result<()> {
    tx.execute(
        "INSERT INTO tc_hook_leaf_authority(workspace_id,panel_id,leaf_id,epoch,owner_id,
        runtime_id,generation,phase,require_binding) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
        params![
            claim.key.workspace.to_string(),
            claim.key.panel.to_string(),
            claim.key.leaf.to_string(),
            claim.epoch,
            claim.owner.map(|v| v.to_string()),
            claim.runtime.to_string(),
            claim.generation.map(|v| v.to_string()),
            claim.phase.as_str(),
            claim.require_binding
        ],
    )?;
    Ok(())
}

fn update_claim(tx: &Transaction<'_>, expected: Claim, next: Claim) -> Result<()> {
    require_exact(tx, expected)?;
    let changed = tx.execute("UPDATE tc_hook_leaf_authority SET epoch=?4,owner_id=?5,runtime_id=?6,
        generation=?7,phase=?8,require_binding=?9 WHERE workspace_id=?1 AND panel_id=?2 AND leaf_id=?3
        AND epoch=?10 AND owner_id IS ?11 AND runtime_id=?12 AND generation IS ?13 AND phase=?14
        AND require_binding=?15",
        params![next.key.workspace.to_string(),next.key.panel.to_string(),next.key.leaf.to_string(),
            next.epoch,next.owner.map(|v| v.to_string()),next.runtime.to_string(),
            next.generation.map(|v| v.to_string()),next.phase.as_str(),next.require_binding,
            expected.epoch,expected.owner.map(|v| v.to_string()),expected.runtime.to_string(),
            expected.generation.map(|v| v.to_string()),expected.phase.as_str(),expected.require_binding])?;
    ensure!(changed == 1, "managed hook ownership lost");
    Ok(())
}

fn next_epoch(claim: Claim) -> Result<i64> {
    claim
        .epoch
        .checked_add(1)
        .ok_or_else(|| anyhow::anyhow!("managed hook epoch exhausted"))
}

fn insert_generation(
    tx: &Transaction<'_>,
    claim: Claim,
    consumed: bool,
    phase: &str,
) -> Result<()> {
    let count: i64 = tx.query_row("SELECT COUNT(*) FROM tc_hook_generations", [], |row| {
        row.get(0)
    })?;
    ensure!(
        count < MAX_GENERATIONS,
        "managed hook generation capacity reached; tombstones retained"
    );
    tx.execute(
        "INSERT INTO tc_hook_generations(generation,workspace_id,panel_id,leaf_id,runtime_id,
        birth_owner_id,born_epoch,spawn_consumed,phase) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
        params![
            claim.generation.map(|v| v.to_string()),
            claim.key.workspace.to_string(),
            claim.key.panel.to_string(),
            claim.key.leaf.to_string(),
            claim.runtime.to_string(),
            claim.owner.map(|v| v.to_string()),
            claim.epoch,
            consumed,
            phase
        ],
    )?;
    Ok(())
}

fn transition(tx: &Transaction<'_>, intent: &LedgerIntent) -> Result<LedgerReceipt> {
    let next = match *intent {
        LedgerIntent::Reserve {
            expected,
            key,
            birth,
            owner,
        } => {
            ensure!(
                load_claim(tx, key)? == expected,
                "managed hook ownership lost"
            );
            let epoch = match expected {
                Some(old) => {
                    ensure!(
                        old.key == key && old.phase == ClaimPhase::Retired,
                        "birth requires retired target"
                    );
                    next_epoch(old)?
                }
                None => 1,
            };
            let claim = Claim {
                key,
                runtime: birth.runtime,
                generation: Some(birth.generation),
                epoch,
                owner: Some(owner),
                phase: ClaimPhase::AuthorizedBirth,
                require_binding: true,
            };
            // INSERT fails for any previously seen nonce, including a tombstone.
            insert_generation(tx, claim, false, "authorized_birth")?;
            match expected {
                Some(old) => update_claim(tx, old, claim)?,
                None => insert_claim(tx, claim)?,
            }
            claim
        }
        LedgerIntent::Install { expected, host } => {
            require_exact(tx, expected)?;
            ensure!(
                expected.phase == ClaimPhase::AuthorizedBirth && host.proves(expected),
                "host birth evidence mismatch"
            );
            generation_live(tx, expected, true)?;
            let claim = Claim {
                phase: ClaimPhase::Active,
                ..expected
            };
            update_claim(tx, expected, claim)?;
            tx.execute("UPDATE tc_hook_generations SET phase='active' WHERE generation=?1 AND phase='authorized_birth'",
                params![claim.generation.map(|v| v.to_string())])?;
            claim
        }
        LedgerIntent::FenceRetire { expected } => {
            require_exact(tx, expected)?;
            ensure!(
                matches!(
                    expected.phase,
                    ClaimPhase::AuthorizedBirth | ClaimPhase::Active
                ),
                "retirement already fenced or transferred"
            );
            let claim = Claim {
                epoch: next_epoch(expected)?,
                phase: ClaimPhase::Retiring,
                require_binding: true,
                ..expected
            };
            update_claim(tx, expected, claim)?;
            claim
        }
        LedgerIntent::Retire { expected } => {
            require_exact(tx, expected)?;
            ensure!(
                expected.phase != ClaimPhase::Retired,
                "birth already retired"
            );
            let claim = Claim {
                epoch: next_epoch(expected)?,
                phase: ClaimPhase::Retired,
                require_binding: true,
                ..expected
            };
            update_claim(tx, expected, claim)?;
            if let Some(generation) = claim.generation {
                ensure!(
                    tx.execute(
                        "UPDATE tc_hook_generations SET phase='retired',retired_epoch=?2
                    WHERE generation=?1 AND phase!='retired'",
                        params![generation.to_string(), claim.epoch]
                    )? == 1,
                    "managed birth no longer current"
                );
            }
            claim
        }
        LedgerIntent::Release { expected } => {
            require_exact(tx, expected)?;
            ensure!(
                expected.phase == ClaimPhase::Active,
                "only active endpoint can release"
            );
            let claim = Claim {
                epoch: next_epoch(expected)?,
                owner: None,
                phase: ClaimPhase::Detached,
                ..expected
            };
            update_claim(tx, expected, claim)?;
            claim
        }
        LedgerIntent::Adopt {
            expected,
            host,
            new_owner,
        } => {
            require_exact(tx, expected)?;
            ensure!(
                matches!(
                    expected.phase,
                    ClaimPhase::Active | ClaimPhase::Detached | ClaimPhase::AuthorizedBirth
                ) && host.proves(expected),
                "host evidence mismatch or retired generation"
            );
            generation_live(tx, expected, true)?;
            let claim = Claim {
                epoch: next_epoch(expected)?,
                owner: Some(new_owner),
                phase: ClaimPhase::Active,
                ..expected
            };
            update_claim(tx, expected, claim)?;
            tx.execute("UPDATE tc_hook_generations SET phase='active' WHERE generation=?1 AND phase!='retired'",
                params![claim.generation.map(|v| v.to_string())])?;
            claim
        }
        LedgerIntent::AdoptLegacy {
            expected,
            new_owner,
        } => {
            require_exact(tx, expected)?;
            ensure!(
                expected.generation.is_none()
                    && !expected.require_binding
                    && matches!(expected.phase, ClaimPhase::Active | ClaimPhase::Detached),
                "legacy target quarantined"
            );
            let claim = Claim {
                epoch: next_epoch(expected)?,
                owner: Some(new_owner),
                phase: ClaimPhase::Active,
                ..expected
            };
            update_claim(tx, expected, claim)?;
            claim
        }
        LedgerIntent::ObserveExisting {
            key,
            host,
            new_owner,
        } => {
            ensure!(
                load_claim(tx, key)?.is_none(),
                "managed hook target has history"
            );
            let claim = Claim {
                key,
                runtime: host.runtime,
                generation: host.generation,
                epoch: 1,
                owner: Some(new_owner),
                phase: ClaimPhase::Active,
                require_binding: true,
            };
            ensure!(host.proves(claim), "observed host evidence incomplete");
            insert_generation(tx, claim, true, "active")?;
            insert_claim(tx, claim)?;
            claim
        }
        LedgerIntent::ImportLegacy {
            key,
            runtime,
            owner,
            single_original_root,
        } => {
            ensure!(
                load_claim(tx, key)?.is_none(),
                "legacy target has managed history"
            );
            let panel_policy: Option<bool> = tx
                .query_row(
                    "SELECT legacy_root_alias_closed FROM tc_hook_panel_legacy_policy
                WHERE workspace_id=?1 AND panel_id=?2",
                    params![key.workspace.to_string(), key.panel.to_string()],
                    |row| row.get(0),
                )
                .optional()?;
            ensure!(!panel_policy.unwrap_or(false), "legacy target quarantined");
            let claim = Claim {
                key,
                runtime,
                generation: None,
                epoch: 1,
                owner: Some(owner),
                phase: ClaimPhase::Active,
                require_binding: false,
            };
            insert_claim(tx, claim)?;
            if single_original_root && panel_policy.is_none() {
                tx.execute(
                    "INSERT INTO tc_hook_panel_legacy_policy(workspace_id,panel_id,alias_epoch,
                    root_leaf_id,legacy_root_alias_closed) VALUES (?1,?2,1,?3,0)",
                    params![
                        key.workspace.to_string(),
                        key.panel.to_string(),
                        key.leaf.to_string()
                    ],
                )?;
            }
            claim
        }
        LedgerIntent::CloseLegacyAlias {
            workspace,
            panel,
            expected_epoch,
            root,
        } => {
            let next = match expected_epoch {
                Some(epoch) => {
                    let next = epoch
                        .checked_add(1)
                        .ok_or_else(|| anyhow::anyhow!("legacy alias epoch exhausted"))?;
                    ensure!(tx.execute("UPDATE tc_hook_panel_legacy_policy SET legacy_root_alias_closed=1,alias_epoch=?5
                WHERE workspace_id=?1 AND panel_id=?2 AND alias_epoch=?3 AND root_leaf_id=?4 AND legacy_root_alias_closed=0",
                        params![workspace.to_string(),panel.to_string(),epoch,root.to_string(),next])? == 1,
                        "legacy alias ownership lost");
                    next
                }
                None => {
                    tx.execute(
                        "INSERT INTO tc_hook_panel_legacy_policy(workspace_id,panel_id,alias_epoch,
                        root_leaf_id,legacy_root_alias_closed) VALUES (?1,?2,1,?3,1)",
                        params![workspace.to_string(), panel.to_string(), root.to_string()],
                    )?;
                    1
                }
            };
            return Ok(LedgerReceipt {
                claim: None,
                alias_epoch: Some(next),
            });
        }
    };
    Ok(LedgerReceipt {
        claim: Some(next),
        alias_epoch: None,
    })
}

/// One-shot capability is never reconstructed from operation receipts. A crash
/// after consume but before factory stays uncertain; the same nonce cannot spawn.
pub(crate) struct SpawnPermit {
    claim: Claim,
}

impl SpawnPermit {
    pub fn run_guarded<T>(
        self,
        store: &super::store::MemoryStore,
        spawn: impl FnOnce(Claim) -> Result<T>,
    ) -> Result<T> {
        store.run_spawn_guarded(self.claim, || spawn(self.claim))
    }

    #[cfg(test)]
    pub fn run<T>(self, spawn: impl FnOnce(Claim) -> T) -> T {
        spawn(self.claim)
    }
}

pub(super) fn validate_spawn(tx: &Transaction<'_>, expected: Claim) -> Result<()> {
    require_exact(tx, expected)?;
    ensure!(
        expected.phase == ClaimPhase::AuthorizedBirth,
        "birth retired before factory"
    );
    generation_live(tx, expected, true)
}

pub(super) fn resolve_context(
    tx: &Transaction<'_>,
    identity: crate::hook_authority::QueryIdentity,
    owner: Uuid,
) -> Result<OwnedHookContext> {
    use crate::hook_authority::QueryIdentity;
    let context = match identity {
        QueryIdentity::Bound { key, binding } => {
            let claim = load_claim(tx, key)?.ok_or_else(|| anyhow!("managed target absent"))?;
            ensure!(
                claim.runtime == binding.runtime && claim.generation == Some(binding.generation),
                "binding mismatch"
            );
            OwnedHookContext {
                claim,
                legacy_alias_epoch: None,
            }
        }
        QueryIdentity::LegacyTarget {
            workspace,
            panel,
            leaf,
        } => {
            let mut stmt = tx.prepare("SELECT workspace_id,leaf_id FROM tc_hook_leaf_authority WHERE panel_id=?1 AND owner_id=?2 AND phase='active' AND generation IS NULL AND require_binding=0")?;
            let rows = stmt
                .query_map(params![panel.to_string(), owner.to_string()], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let mut matches = Vec::new();
            for (w, l) in rows {
                let key = LeafKey {
                    workspace: Uuid::parse_str(&w)?,
                    panel,
                    leaf: Uuid::parse_str(&l)?,
                };
                if workspace.is_some_and(|wanted| wanted != key.workspace)
                    || leaf.is_some_and(|wanted| wanted != key.leaf)
                {
                    continue;
                }
                let claim = load_claim(tx, key)?.ok_or_else(|| anyhow!("legacy target absent"))?;
                let alias_epoch = if leaf.is_none() {
                    tx.query_row("SELECT alias_epoch FROM tc_hook_panel_legacy_policy WHERE workspace_id=?1 AND panel_id=?2 AND root_leaf_id=?3 AND legacy_root_alias_closed=0",params![w,panel.to_string(),l],|r|r.get::<_,i64>(0)).optional()?
                } else {
                    None
                };
                if leaf.is_none() && alias_epoch.is_none() {
                    continue;
                }
                matches.push(OwnedHookContext {
                    claim,
                    legacy_alias_epoch: alias_epoch,
                });
            }
            ensure!(matches.len() == 1, "legacy target ambiguous or quarantined");
            matches.remove(0)
        }
        QueryIdentity::UnscopedMemory => anyhow::bail!("unscoped memory is not a managed target"),
    };
    ensure!(
        context.claim.owner == Some(owner),
        "endpoint ownership lost"
    );
    validate_owned(tx, &context)?;
    Ok(context)
}

pub(super) fn consume_birth_once(
    tx: &Transaction<'_>,
    expected: Claim,
) -> Result<Option<SpawnPermit>> {
    require_exact(tx, expected)?;
    ensure!(
        expected.phase == ClaimPhase::AuthorizedBirth && expected.owner.is_some(),
        "birth claim inactive"
    );
    generation_live(tx, expected, false)?;
    let changed = tx.execute("UPDATE tc_hook_generations SET spawn_consumed=1 WHERE generation=?1
        AND phase='authorized_birth' AND spawn_consumed=0 AND runtime_id=?2 AND birth_owner_id=?3 AND born_epoch=?4",
        params![expected.generation.map(|v| v.to_string()),expected.runtime.to_string(),
            expected.owner.map(|v| v.to_string()),expected.epoch])?;
    Ok((changed == 1).then_some(SpawnPermit { claim: expected }))
}

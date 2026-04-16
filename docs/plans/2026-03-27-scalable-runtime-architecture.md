# Scalable Runtime Architecture Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Re-architect the app so it can support many projects and many parallel terminals on moderate hardware without changing the product vision, visual language, or desktop-native stack.

**Architecture:** Split the current design into a headless runtime layer and a thinner UI layer. The runtime becomes the owner of PTYs, session state, and scheduling; the UI becomes a consumer of snapshots plus an emitter of intents. This keeps the desktop/workspace UX while making repaint rate, terminal quality tiers, persistence, and future features manageable under load.

**Tech Stack:** Rust 2021, eframe/egui, portable-pty, alacritty_terminal, serde/serde_json, anyhow, log

---

### Task 1: Establish Performance Budget And Baseline

**Files:**
- Create: `docs/architecture/performance-budget.md`
- Create: `tests/runtime/perf_budget.rs`
- Modify: `Cargo.toml`

**Step 1: Write the failing test**

Add a test scaffold that encodes the first budget target:
- `20` terminals registered
- `6` visible
- `3` with sustained output simulation
- UI snapshot/update path stays below the chosen budget in a synthetic benchmark harness

Example skeleton:

```rust
#[test]
fn runtime_budget_smoke_target_is_defined() {
    let budget = PerfBudget::default();
    assert_eq!(budget.open_terminals, 20);
    assert_eq!(budget.visible_terminals, 6);
    assert_eq!(budget.streaming_terminals, 3);
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test runtime_budget_smoke_target_is_defined`

Expected: FAIL because the budget/test scaffolding does not exist yet.

**Step 3: Write minimal implementation**

Create a small performance-budget module and a test-only harness. Do not optimize yet. The goal is to lock the target before refactoring.

**Step 4: Run test to verify it passes**

Run: `cargo test runtime_budget_smoke_target_is_defined`

Expected: PASS

**Step 5: Commit**

```bash
git add Cargo.toml docs/architecture/performance-budget.md tests/runtime/perf_budget.rs
git commit -m "docs: define runtime performance budget"
```

### Task 2: Introduce Runtime Domain Layer

**Files:**
- Create: `src/runtime/mod.rs`
- Create: `src/runtime/session.rs`
- Create: `src/runtime/workspace.rs`
- Create: `src/runtime/registry.rs`
- Modify: `src/main.rs`
- Modify: `src/state/workspace.rs`
- Test: `tests/runtime/registry.rs`

**Step 1: Write the failing test**

Add tests for a headless registry that can:
- create workspaces
- register terminal sessions
- look up sessions by workspace
- expose immutable snapshot data without touching `egui`

Example skeleton:

```rust
#[test]
fn registry_groups_sessions_by_workspace() {
    let mut registry = RuntimeRegistry::new();
    let workspace = registry.create_workspace("api", None);
    let session = registry.create_session(workspace);

    let snapshot = registry.snapshot();

    assert_eq!(snapshot.workspaces.len(), 1);
    assert_eq!(snapshot.sessions_by_workspace(&workspace).len(), 1);
    assert_eq!(snapshot.sessions_by_workspace(&workspace)[0].id, session);
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test registry_groups_sessions_by_workspace`

Expected: FAIL because the runtime layer does not exist.

**Step 3: Write minimal implementation**

Create the runtime module with plain data types and snapshots. Keep PTYs out of this first pass. The output should be serializable, testable, and UI-agnostic.

**Step 4: Run test to verify it passes**

Run: `cargo test registry_groups_sessions_by_workspace`

Expected: PASS

**Step 5: Commit**

```bash
git add src/runtime/mod.rs src/runtime/session.rs src/runtime/workspace.rs src/runtime/registry.rs src/main.rs src/state/workspace.rs tests/runtime/registry.rs
git commit -m "refactor: introduce headless runtime registry"
```

### Task 3: Move PTY Ownership Out Of `TerminalPanel`

**Files:**
- Create: `src/runtime/pty_manager.rs`
- Modify: `src/terminal/panel.rs`
- Modify: `src/terminal/pty.rs`
- Modify: `src/panel.rs`
- Test: `tests/runtime/pty_manager.rs`

**Step 1: Write the failing test**

Add tests for a PTY manager API that:
- creates a session handle
- stores PTY lifecycle separately from panel geometry
- can close a session without going through the UI panel type

Example skeleton:

```rust
#[test]
fn pty_manager_closes_session_without_panel_object() {
    let mut manager = PtyManager::new_for_tests();
    let session_id = manager.create_detached(SessionSpec::default());

    manager.close(session_id);

    assert!(!manager.is_alive(session_id));
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test pty_manager_closes_session_without_panel_object`

Expected: FAIL because PTYs still belong to `TerminalPanel`.

**Step 3: Write minimal implementation**

Refactor so `TerminalPanel` stops owning `PtyHandle` directly. It should reference a session/runtime id and render from runtime snapshot data. The PTY manager becomes the owner of creation, shutdown, resize, title, and output buffers.

**Step 4: Run test to verify it passes**

Run: `cargo test pty_manager_closes_session_without_panel_object`

Expected: PASS

**Step 5: Commit**

```bash
git add src/runtime/pty_manager.rs src/terminal/panel.rs src/terminal/pty.rs src/panel.rs tests/runtime/pty_manager.rs
git commit -m "refactor: move pty ownership into runtime manager"
```

### Task 4: Replace Per-Terminal Thread Explosion With Shared Runtime Scheduling

**Files:**
- Modify: `src/runtime/pty_manager.rs`
- Modify: `src/terminal/pty.rs`
- Create: `tests/runtime/scheduling.rs`

**Step 1: Write the failing test**

Add tests that validate the runtime scheduling contract:
- session I/O can be polled/coalesced centrally
- output can wake the UI without forcing one dedicated reader thread per session
- bursty output from many sessions collapses into bounded update batches

Example skeleton:

```rust
#[test]
fn scheduler_coalesces_many_session_updates() {
    let mut scheduler = RuntimeScheduler::new_for_tests();
    scheduler.enqueue_output_batch(20, 50);

    let batch = scheduler.drain_ui_updates();

    assert!(batch.session_updates.len() <= 20);
    assert!(batch.repaint_requested);
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test scheduler_coalesces_many_session_updates`

Expected: FAIL because there is no shared scheduler yet.

**Step 3: Write minimal implementation**

Refactor toward a shared runtime loop or small bounded worker set. The implementation detail can be:
- a central runtime thread that owns PTY readers
- a bounded queue of UI-visible updates
- a coalesced wake-up model

Do not optimize rendering yet. Only reduce concurrency overhead and repaint storms.

**Step 4: Run test to verify it passes**

Run: `cargo test scheduler_coalesces_many_session_updates`

Expected: PASS

**Step 5: Commit**

```bash
git add src/runtime/pty_manager.rs src/terminal/pty.rs tests/runtime/scheduling.rs
git commit -m "perf: centralize runtime scheduling and coalesce updates"
```

### Task 5: Add Terminal Quality Tiers And Visibility-Based Rendering

**Files:**
- Create: `src/runtime/render_qos.rs`
- Modify: `src/app.rs`
- Modify: `src/terminal/panel.rs`
- Modify: `src/terminal/renderer.rs`
- Test: `src/terminal/panel.rs`
- Test: `tests/runtime/render_qos.rs`

**Step 1: Write the failing test**

Add tests for render quality decisions:
- focused visible terminal => full render
- visible but small terminal => reduced live render
- offscreen terminal => no expensive render path
- burst output from a background terminal does not promote it to full render

Example skeleton:

```rust
#[test]
fn qos_downgrades_background_terminal() {
    let qos = RenderQos::decide(RenderInputs {
        visible: true,
        focused: false,
        screen_area: 12_000.0,
        streaming: true,
    });

    assert_eq!(qos, RenderTier::Preview);
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test qos_downgrades_background_terminal`

Expected: FAIL because render tiers do not exist.

**Step 3: Write minimal implementation**

Introduce explicit render tiers:
- `Full`
- `ReducedLive`
- `Preview`
- `Hidden`

Wire the app to compute tier from viewport visibility and panel metrics before calling terminal rendering.

**Step 4: Run test to verify it passes**

Run: `cargo test qos_downgrades_background_terminal`

Expected: PASS

**Step 5: Commit**

```bash
git add src/runtime/render_qos.rs src/app.rs src/terminal/panel.rs src/terminal/renderer.rs tests/runtime/render_qos.rs
git commit -m "perf: add visibility-based terminal render tiers"
```

### Task 6: Decouple UI Repaint Rate From Raw Terminal Output

**Files:**
- Modify: `src/app.rs`
- Modify: `src/runtime/pty_manager.rs`
- Modify: `src/update.rs`
- Test: `tests/runtime/repaint_policy.rs`

**Step 1: Write the failing test**

Add tests for a repaint policy that:
- caps background repaint frequency
- allows focused terminal responsiveness
- batches multiple runtime events into one UI wake-up window

Example skeleton:

```rust
#[test]
fn repaint_policy_batches_bursty_runtime_events() {
    let mut policy = RepaintPolicy::new(Duration::from_millis(33));
    policy.note_runtime_event();
    policy.note_runtime_event();

    assert!(policy.should_repaint_now());
    assert!(!policy.should_repaint_now());
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test repaint_policy_batches_bursty_runtime_events`

Expected: FAIL because repaint policy does not exist yet.

**Step 3: Write minimal implementation**

Move to an explicit repaint policy:
- input-driven repaints stay immediate
- focused terminal output stays near-real-time
- background output is batched

**Step 4: Run test to verify it passes**

Run: `cargo test repaint_policy_batches_bursty_runtime_events`

Expected: PASS

**Step 5: Commit**

```bash
git add src/app.rs src/runtime/pty_manager.rs src/update.rs tests/runtime/repaint_policy.rs
git commit -m "perf: add bounded repaint policy"
```

### Task 7: Harden Session Persistence For Multi-Workspace Recovery

**Files:**
- Modify: `src/state/persistence.rs`
- Create: `src/runtime/session_persistence.rs`
- Test: `tests/runtime/session_persistence.rs`

**Step 1: Write the failing test**

Add tests that verify:
- many workspaces and many sessions serialize/restore correctly
- session metadata survives restart
- restoration can reconstruct runtime sessions without relying on the UI panel type

Example skeleton:

```rust
#[test]
fn session_restore_rebuilds_workspace_session_graph() {
    let state = sample_runtime_state(4, 12);
    let restored = RuntimePersistence::round_trip(state.clone());

    assert_eq!(restored.workspace_count(), state.workspace_count());
    assert_eq!(restored.session_count(), state.session_count());
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test session_restore_rebuilds_workspace_session_graph`

Expected: FAIL because persistence is still UI-centric.

**Step 3: Write minimal implementation**

Persist runtime-owned session metadata separately from panel chrome/layout so future features like tabs, pinning, routing, and daemon-mode are not blocked by the current panel model.

**Step 4: Run test to verify it passes**

Run: `cargo test session_restore_rebuilds_workspace_session_graph`

Expected: PASS

**Step 5: Commit**

```bash
git add src/state/persistence.rs src/runtime/session_persistence.rs tests/runtime/session_persistence.rs
git commit -m "refactor: persist runtime sessions separately from ui panels"
```

### Task 8: Add Load-Test Coverage For Realistic Scale

**Files:**
- Create: `tests/runtime/load_20_terminals.rs`
- Create: `tests/runtime/fixtures/mod.rs`
- Modify: `Cargo.toml`

**Step 1: Write the failing test**

Create a realistic scale test:
- `5` workspaces
- `20` sessions
- mixed visible/hidden/focused states
- synthetic output bursts

Example skeleton:

```rust
#[test]
fn load_test_twenty_terminals_keeps_runtime_consistent() {
    let mut harness = RuntimeHarness::new();
    harness.seed(5, 20);
    harness.emit_output_bursts();
    harness.step();

    assert_eq!(harness.session_count(), 20);
    assert!(harness.no_deadlocks());
    assert!(harness.snapshot_is_consistent());
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test load_test_twenty_terminals_keeps_runtime_consistent`

Expected: FAIL because the harness/fixtures do not exist.

**Step 3: Write minimal implementation**

Build the harness and fixtures. Keep it deterministic. The goal is regression protection, not benchmark theater.

**Step 4: Run test to verify it passes**

Run: `cargo test load_test_twenty_terminals_keeps_runtime_consistent`

Expected: PASS

**Step 5: Commit**

```bash
git add Cargo.toml tests/runtime/load_20_terminals.rs tests/runtime/fixtures/mod.rs
git commit -m "test: add runtime load coverage for multi-workspace scale"
```

### Task 9: Optional Phase After Runtime Stabilization - Local Daemon Sessions

**Files:**
- Create: `docs/architecture/local-daemon.md`
- Create: `src/daemon/mod.rs`
- Create: `src/daemon/protocol.rs`
- Test: `tests/daemon/protocol.rs`

**Step 1: Write the failing test**

Only after Tasks 1-8 are green, add a protocol test for a local session daemon that can:
- host long-lived PTYs
- survive UI restarts
- reconnect workspaces/sessions

**Step 2: Run test to verify it fails**

Run: `cargo test daemon_protocol_round_trip`

Expected: FAIL because daemon support does not exist.

**Step 3: Write minimal implementation**

Keep this phase optional. Do not start here. Only proceed if true session continuity across UI restarts becomes a hard product requirement.

**Step 4: Run test to verify it passes**

Run: `cargo test daemon_protocol_round_trip`

Expected: PASS

**Step 5: Commit**

```bash
git add docs/architecture/local-daemon.md src/daemon/mod.rs src/daemon/protocol.rs tests/daemon/protocol.rs
git commit -m "feat: add local daemon protocol for persistent sessions"
```

### Task 10: Final Verification

**Files:**
- Modify if needed after verification: `src/app.rs`, `src/runtime/*.rs`, `src/terminal/*.rs`, `src/state/*.rs`

**Step 1: Run focused verification**

Run:
- `cargo test runtime_budget_smoke_target_is_defined`
- `cargo test registry_groups_sessions_by_workspace`
- `cargo test pty_manager_closes_session_without_panel_object`
- `cargo test scheduler_coalesces_many_session_updates`
- `cargo test qos_downgrades_background_terminal`
- `cargo test repaint_policy_batches_bursty_runtime_events`
- `cargo test session_restore_rebuilds_workspace_session_graph`
- `cargo test load_test_twenty_terminals_keeps_runtime_consistent`

Expected: PASS

**Step 2: Run full verification**

Run:
- `cargo fmt --check`
- `cargo test`

Expected: PASS

**Step 3: Manual smoke verification**

Run: `cargo run`

Verify:
- multiple workspaces still open correctly
- folder-backed workspaces still spawn terminals in the right `cwd`
- drag/resize/zoom interactions still feel responsive
- double-click focus animation still works
- autosave/restore still reconstructs the last session

**Step 4: Capture results**

Update:
- `docs/architecture/performance-budget.md`
- `docs/architecture/local-daemon.md` if Task 9 was implemented

with:
- measured bottlenecks
- achieved scale
- known residual limits

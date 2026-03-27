# Infinite Canvas Terminal Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Build a native Rust infinite-canvas terminal emulator matching `documentacion-completa.md`.

**Architecture:** Use `eframe`/`egui` for the native GPU UI shell, `portable-pty` plus `alacritty_terminal` for terminal emulation, and a modular Rust crate structure aligned with the documented 8-phase build order. Keep rendering on-demand, panel state workspace-scoped, and terminal drawing split between canvas-space backgrounds and screen-space text.

**Tech Stack:** Rust 2021, eframe/egui 0.30, alacritty_terminal 0.25.1, portable-pty 0.9, serde/serde_json, minreq, sha2, anyhow, env_logger

---

### Task 1: Scaffold Project And Repo Layout

**Files:**
- Create: `Cargo.toml`
- Create: `src/main.rs`
- Create: `src/app.rs`
- Create: `src/panel.rs`
- Create: `src/canvas/mod.rs`
- Create: `src/canvas/config.rs`
- Create: `src/canvas/viewport.rs`
- Create: `src/canvas/scene.rs`
- Create: `src/canvas/grid.rs`
- Create: `src/canvas/minimap.rs`
- Create: `src/canvas/snap.rs`
- Create: `src/canvas/layout.rs`
- Create: `src/terminal/mod.rs`
- Create: `src/terminal/panel.rs`
- Create: `src/terminal/renderer.rs`
- Create: `src/terminal/input.rs`
- Create: `src/terminal/pty.rs`
- Create: `src/terminal/colors.rs`
- Create: `src/sidebar/mod.rs`
- Create: `src/sidebar/workspace_list.rs`
- Create: `src/sidebar/terminal_list.rs`
- Create: `src/command_palette/mod.rs`
- Create: `src/command_palette/commands.rs`
- Create: `src/command_palette/fuzzy.rs`
- Create: `src/state/mod.rs`
- Create: `src/state/workspace.rs`
- Create: `src/state/persistence.rs`
- Create: `src/state/panel_state.rs`
- Create: `src/theme/mod.rs`
- Create: `src/theme/builtin.rs`
- Create: `src/theme/colors.rs`
- Create: `src/theme/fonts.rs`
- Create: `src/shortcuts/mod.rs`
- Create: `src/shortcuts/default_bindings.rs`
- Create: `src/update.rs`
- Create: `src/utils/mod.rs`
- Create: `src/utils/platform.rs`
- Create: `assets/icon.png`
- Create: `assets/brand.png`

**Step 1: Write the failing tests**

Add unit tests for viewport transforms, snap scoring, fuzzy matching, input mapping, cursor blinking, and update helpers in the destination modules.

**Step 2: Run test to verify it fails**

Run: `cargo test --locked`
Expected: FAIL because the crate and modules do not exist yet.

**Step 3: Write minimal implementation**

Create the crate manifest, source tree, placeholder assets, and enough types/functions for the test modules to compile.

**Step 4: Run test to verify it passes**

Run: `cargo test --locked`
Expected: foundational unit tests pass.

**Step 5: Commit**

```bash
git add Cargo.toml src assets docs/plans/2026-03-26-infinite-canvas-terminal.md
git commit -m "feat: scaffold infinite canvas terminal app"
```

### Task 2: Implement Canvas Foundation

**Files:**
- Modify: `src/app.rs`
- Modify: `src/canvas/config.rs`
- Modify: `src/canvas/viewport.rs`
- Modify: `src/canvas/scene.rs`
- Modify: `src/canvas/grid.rs`
- Modify: `src/canvas/minimap.rs`
- Modify: `src/canvas/snap.rs`

**Step 1: Write the failing tests**

Add tests that assert zoom anchoring, `pan_to_center`, visible rect calculations, closest-snap selection, and minimap coordinate conversion.

**Step 2: Run test to verify it fails**

Run: `cargo test --locked canvas::viewport::tests canvas::snap::tests`
Expected: FAIL with missing methods or wrong behavior.

**Step 3: Write minimal implementation**

Implement canvas constants, viewport math, grid drawing guardrails, scene input handling, snap guide scoring, and minimap navigation helpers; integrate them into a visible canvas in `app.rs`.

**Step 4: Run test to verify it passes**

Run: `cargo test --locked canvas::viewport::tests canvas::snap::tests`
Expected: PASS.

**Step 5: Commit**

```bash
git add src/app.rs src/canvas
git commit -m "feat: add infinite canvas foundation"
```

### Task 3: Implement Terminal Core

**Files:**
- Modify: `src/terminal/colors.rs`
- Modify: `src/terminal/pty.rs`
- Modify: `src/terminal/input.rs`
- Modify: `src/terminal/renderer.rs`
- Modify: `src/terminal/panel.rs`

**Step 1: Write the failing tests**

Add tests for ANSI/256-color mapping, cursor mode key encoding, copy-vs-SIGINT behavior, cursor blink visibility, and version comparison helpers.

**Step 2: Run test to verify it fails**

Run: `cargo test --locked terminal::input::tests terminal::renderer::tests`
Expected: FAIL because terminal helpers are incomplete.

**Step 3: Write minimal implementation**

Implement ANSI color mapping, PTY spawn with three threads, keyboard-to-byte translation, two-pass terminal rendering helpers, and a terminal panel that can spawn a system shell and display its output.

**Step 4: Run test to verify it passes**

Run: `cargo test --locked terminal::input::tests terminal::renderer::tests`
Expected: PASS.

**Step 5: Commit**

```bash
git add src/terminal
git commit -m "feat: add terminal emulation core"
```

### Task 4: Integrate Panels, Workspaces, And Chrome

**Files:**
- Modify: `src/panel.rs`
- Modify: `src/state/workspace.rs`
- Modify: `src/state/panel_state.rs`
- Modify: `src/app.rs`
- Modify: `src/sidebar/mod.rs`
- Modify: `src/sidebar/workspace_list.rs`
- Modify: `src/sidebar/terminal_list.rs`
- Modify: `src/command_palette/commands.rs`
- Modify: `src/command_palette/fuzzy.rs`
- Modify: `src/command_palette/mod.rs`

**Step 1: Write the failing tests**

Add tests for workspace placement, panel save/load conversion, fuzzy scoring, and command registry behavior.

**Step 2: Run test to verify it fails**

Run: `cargo test --locked state::workspace::tests command_palette::fuzzy::tests`
Expected: FAIL because workspace placement and command palette are incomplete.

**Step 3: Write minimal implementation**

Implement the `CanvasPanel` wrapper, workspace model with gap-filling placement, sidebar navigation, command palette overlay, minimap overlay, and panel focus/z-order integration in the app shell.

**Step 4: Run test to verify it passes**

Run: `cargo test --locked state::workspace::tests command_palette::fuzzy::tests`
Expected: PASS.

**Step 5: Commit**

```bash
git add src/app.rs src/panel.rs src/state src/sidebar src/command_palette
git commit -m "feat: add workspaces and app chrome"
```

### Task 5: Persist State, Add Updating, And Polish

**Files:**
- Modify: `src/state/persistence.rs`
- Modify: `src/update.rs`
- Modify: `src/theme/fonts.rs`
- Modify: `src/shortcuts/default_bindings.rs`
- Modify: `src/utils/platform.rs`
- Modify: `src/terminal/panel.rs`
- Modify: `src/app.rs`

**Step 1: Write the failing tests**

Add tests for state roundtrip helpers, update version comparison, checksum verification, and stale terminal mode recovery helpers.

**Step 2: Run test to verify it fails**

Run: `cargo test --locked update::tests`
Expected: FAIL because persistence/update helpers are incomplete.

**Step 3: Write minimal implementation**

Implement JSON persistence, GitHub release polling and checksum verification helpers, font fallback setup, shortcut bindings, stale TUI recovery, and remaining polish paths required for a functional desktop app.

**Step 4: Run test to verify it passes**

Run: `cargo test --locked update::tests`
Expected: PASS.

**Step 5: Commit**

```bash
git add src/state/persistence.rs src/update.rs src/theme/fonts.rs src/shortcuts src/utils src/app.rs src/terminal/panel.rs
git commit -m "feat: add persistence updater and polish"
```

### Task 6: Verify The Whole Application

**Files:**
- Modify as needed from previous tasks

**Step 1: Run formatting**

Run: `cargo fmt --all`

**Step 2: Run static analysis**

Run: `cargo clippy --locked --all-targets --all-features -- -D warnings`

**Step 3: Run tests**

Run: `cargo test --locked`

**Step 4: Run the app**

Run: `cargo run`
Expected: native window with sidebar, infinite canvas, grid, workspace state, terminal panels, command palette, and minimap.

**Step 5: Commit**

```bash
git add .
git commit -m "feat: finalize infinite canvas terminal emulator"
```

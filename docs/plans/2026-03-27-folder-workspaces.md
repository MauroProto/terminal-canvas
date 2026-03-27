# Folder Workspaces Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Let users open local folders from the left sidebar and have every terminal in that workspace spawn inside the selected folder.

**Architecture:** Treat folder-backed workspaces as the primary unit in the sidebar. Reuse the existing `Workspace.cwd -> TerminalPanel::spawn_pty -> PtyHandle::spawn` chain so the PTY launch path stays single-source-of-truth, and add a folder-picker flow in the app layer that deduplicates already-open folders before creating a workspace.

**Tech Stack:** Rust, eframe/egui, portable-pty, rfd, serde

---

### Task 1: Folder-backed workspace metadata

**Files:**
- Modify: `src/state/workspace.rs`
- Test: `src/state/workspace.rs`

**Step 1: Write the failing tests**

Add tests for:
- Creating a workspace from a folder path uses that folder as `cwd`
- The visible workspace name comes from the selected folder name
- Reopening the same folder can be matched against an existing workspace path

**Step 2: Run test to verify it fails**

Run: `cargo test workspace::tests`

Expected: FAIL because the folder helpers do not exist yet.

**Step 3: Write minimal implementation**

Add helper methods for:
- Normalizing/comparing workspace paths
- Creating a folder-backed workspace from a `PathBuf`
- Exposing display/path metadata for the sidebar without changing PTY spawn semantics

**Step 4: Run test to verify it passes**

Run: `cargo test workspace::tests`

Expected: PASS

### Task 2: Folder open flow in app state

**Files:**
- Modify: `src/app.rs`
- Modify: `src/command_palette/commands.rs`
- Modify: `src/shortcuts/mod.rs`
- Test: `src/app.rs`

**Step 1: Write the failing test**

Add a test for an app-level helper that:
- Reuses an existing workspace when a folder is already open
- Creates a new workspace when the folder is new

**Step 2: Run test to verify it fails**

Run: `cargo test app::tests`

Expected: FAIL because the helper/command path does not exist yet.

**Step 3: Write minimal implementation**

Add:
- Folder picker command (`Open Folder`)
- App helper to upsert/switch folder-backed workspaces
- Automatic first terminal spawn inside the folder when creating a new workspace

**Step 4: Run test to verify it passes**

Run: `cargo test app::tests`

Expected: PASS

### Task 3: Sidebar integration

**Files:**
- Modify: `src/sidebar/mod.rs`
- Modify: `src/sidebar/workspace_list.rs`

**Step 1: Write the UI behavior against the new responses**

Make the workspace tab expose:
- `Open folder` action
- Folder name as workspace title
- Full/truncated path as secondary label when available
- Existing `+` per workspace still spawning a terminal in that workspace folder

**Step 2: Verify compilation**

Run: `cargo test`

Expected: PASS

### Task 4: Verification and cleanup

**Files:**
- Modify if needed after verification: `src/app.rs`, `src/state/workspace.rs`, `src/sidebar/*.rs`

**Step 1: Run focused and full verification**

Run:
- `cargo test workspace::tests`
- `cargo test app::tests`
- `cargo test`

**Step 2: Refactor only if all tests are green**

Keep the flow minimal:
- no duplicate PTY cwd logic
- no per-frame path recomputation in hot paths unless necessary
- no behavior regressions for scratch workspaces already persisted

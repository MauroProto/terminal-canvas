# My Terminal

Native desktop workspace for terminals and coding agents.

`My Terminal` is a Rust desktop app built around floating terminal panels, workspaces, taskbar layouts, live collaboration, and agent orchestration. The product is no longer centered on an infinite canvas UX, even though some internal geometry modules still use that legacy vocabulary.

## What it does today

- Native terminal panels with resize, minimize, snap, and taskbar restore
- Folder-backed workspaces
- Layout presets from the taskbar
- Detached runtime sessions with lazy restore
- Trusted live collaboration with local TLS and device approval
- Agent/session orchestration with provider detection and Git worktree support

## Stack

- Rust
- `eframe` / `egui` / `wgpu`
- `alacritty_terminal`
- `portable-pty`
- `axum` / WebSocket / Rustls

## Quickstart

```bash
cargo run --bin mi-terminal
```

Optimized release build:

```bash
cargo build --release
./target/release/mi-terminal
```

On macOS you can also launch the bundled helper:

```bash
./abrir-mi-terminal.command
```

## Current architecture direction

The current product shape is:

> a native desktop/panel manager for terminals and agent sessions

There is still legacy `canvas` naming inside the repo. That is implementation debt, not the intended product identity.

## Development status

The repository is in active consolidation. The main priorities are:

- align public/docs naming with the current desktop product
- harden collaboration privacy and protocol guarantees
- finish splitting shell/runtime responsibilities
- reconcile performance budget docs with the actual UI behavior

## Verification

Runtime and regression coverage lives under `tests/runtime` plus module-local tests.

Typical verification command:

```bash
cargo test --quiet
```

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
- Scrollback search (regex) with match highlight and jump
- Mouse reporting to TUIs (click/drag/motion), word/line selection
- In-band agent status via the OSC 9999 channel, with an attention inbox in the sidebar
- Built-in code review: unified diff viewer (changed + new files, colored, open-in-editor)
- Send review feedback to the agent straight from the code review (bracketed-paste injection)
- Git worktree lifecycle: list and clean up agent worktrees from the code review
- Quick Open: async fuzzy file finder for the active workspace
- Built-in code viewer docked to the right of the canvas: resizable panel with a line-number
  gutter, selectable text and real syntax highlighting — TextMate grammars via `two-face`
  (the extended set `bat` ships, 213 languages incl. TypeScript/TSX/TOML) rendered with the
  Catppuccin Mocha theme. Highlighting runs on a worker thread (~145 ms for 2400 lines, off the
  UI thread), one `LayoutJob` per line, and rows are virtualized
- Clickable URLs: Cmd/Ctrl+click a link in the terminal to open it
- OS notifications when an agent needs attention (waiting approval / input / failed)
- New terminals inherit the focused shell's directory (OSC 7, if the shell reports it)
- Settings dialog (`Ctrl+,`): edit font size, scrollback, bell, OSC 52, copy-on-select and
  shell live, applied immediately and persisted to `config.toml`
- Broadcast (`Ctrl+Shift+Enter`): send one command to many terminals at once, picking targets
  from a list (dead panels can't be selected)
- Export terminal output (`Ctrl+Shift+E`): dump the focused terminal's scrollback to a text
  file in your Downloads folder
- Git branch badge in each panel's title bar, with a dot when the repo is dirty (drawn only
  when it fits without covering the title)
- Resume past agent conversations: the app does not store them, it reads the history the CLI
  already wrote (Claude Code keeps one JSONL per session under `~/.claude/projects/<cwd-slug>/`)
  and relaunches the one you pick with `--resume <id>`. A restored panel also re-enters its
  agent with the provider's continue flag, so a restart no longer orphans the conversation
- Toast notices confirming actions that have no other visible feedback
- Project file explorer (sidebar "Files" tab): lazy tree of the active workspace, click a file
  to read it in the built-in viewer, heavy directories (`.git`, `node_modules`, `target`) skipped
- Scrollback survives restarts: each panel's history is persisted and replayed into the grid on
  restore, so a restored panel shows its previous session instead of an empty rectangle

## Configuration

Open the settings dialog with `Ctrl+,` to change everything below from the UI:
changes apply live and are written back to disk.

Settings are read from `config.toml` in the platform config directory (same
location family as the persisted layout). Missing keys fall back to defaults.

```toml
[terminal]
font_size = 15.0        # 8..32, base terminal font size
scrollback_lines = 10000
allow_osc52 = false     # let terminal output write the system clipboard
audio_bell = false      # play a sound on bell (in addition to the visual flash)
copy_on_select = false  # auto-copy to clipboard when you select text
agent_notifications = true  # OS notification when an agent needs attention
# shell = "/opt/homebrew/bin/fish"   # custom shell (default: system login shell)
```

The `MI_TERMINAL_ALLOW_OSC52` environment variable still overrides `allow_osc52`.

## Keyboard shortcuts

| Action | Shortcut |
| --- | --- |
| Command palette | `Ctrl+Shift+P` |
| New terminal | `Ctrl+Shift+T` |
| Close terminal | `Ctrl+Shift+W` |
| Rename terminal | `F2` |
| Search in terminal | `Ctrl+Shift+F` |
| Review changes (code review) | `Ctrl+Shift+D` |
| Quick open file (opens the built-in viewer) | `Ctrl+P` |
| Settings | `Ctrl+,` |
| Export terminal output | `Ctrl+Shift+E` |
| Broadcast command to terminals | `Ctrl+Shift+Enter` |
| Resume a past agent conversation | `Ctrl+Shift+R` |
| Launch agent | `Ctrl+Shift+A` |
| Focus next / prev | `Ctrl+Shift+]` / `Ctrl+Shift+[` |
| Toggle sidebar | `Ctrl+B` |
| Toggle fullscreen | `F11` |

In the terminal, double-click selects a word and triple-click selects a line.

## Stack

- Rust
- `eframe` / `egui` / `wgpu`
- `alacritty_terminal`
- optional experimental `libghostty-vt` backend probe
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

Optional Ghostty VT backend spike:

```bash
PATH=/opt/homebrew/opt/zig@0.15/bin:$PATH \
MACOSX_DEPLOYMENT_TARGET=13.0 \
RUSTFLAGS='-C link-arg=-Wl,-ld_classic' \
cargo test --features ghostty-vt ghostty_probe --quiet
```

To run the app with the experimental Ghostty VT path:

```bash
PATH=/opt/homebrew/opt/zig@0.15/bin:$PATH \
MACOSX_DEPLOYMENT_TARGET=13.0 \
RUSTFLAGS='-C link-arg=-Wl,-ld_classic' \
MI_TERMINAL_BACKEND=ghostty \
cargo run --features ghostty-vt --bin mi-terminal
```

This is not the production backend yet. It uses Ghostty's VT core for parsing/render snapshots while the stable default backend remains `alacritty_terminal`.

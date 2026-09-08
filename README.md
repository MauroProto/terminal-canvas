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
- Git worktree lifecycle: list, archive and restore agent worktrees from the code review;
  uncommitted files are retained and directories used by live terminals are protected
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
- Drag & drop files from the OS onto a terminal: their paths are typed shell-escaped
  (single-quoted, like Terminal.app), ready to submit; dropping onto the code viewer opens
  the file there
- Attach a screenshot to the focused agent (`Ctrl+Shift+K`): pick a screen region and the
  capture's path is pasted into the agent's prompt
- Git branch badge in each panel's title bar, with a dot when the repo is dirty (drawn only
  when it fits without covering the title)
- Resume agent conversations (`Ctrl+Shift+R`): Claude Code gets an exact-session picker backed by
  its own history; Codex, Gemini, OpenCode and Copilot use their verified native “latest session”
  command. Resume replaces the focused runtime in a fresh shell, so the command is never submitted
  as chat text. Restored panels use an exact hook-captured session id when available, otherwise the
  provider's native latest-session command. Split leaves persist their command and session id
  independently, so each terminal resumes its own conversation
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

[integrations]
# linear_token = "lin_api_..."   # enables the Linear source in the Tasks tab
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
| Attach screenshot to agent | `Ctrl+Shift+K` |
| Broadcast command to terminals | `Ctrl+Shift+Enter` |
| Resume a past agent conversation | `Ctrl+Shift+R` |
| Launch agent | `Ctrl+Shift+A` |
| Focus next / prev | `Ctrl+Shift+]` / `Ctrl+Shift+[` |
| Split terminal right / down | macOS: `Cmd+D` / `Cmd+Shift+D`; Windows/Linux: `Ctrl+Alt+D` / `Ctrl+Alt+Shift+D` |
| Close split (leaf) | macOS: `Cmd+W`; Windows/Linux: `Ctrl+Alt+W` |
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

Use Rust through `rustup`; the repository pins its compiler and tools in
`rust-toolchain.toml`, matching CI. Windows source builds require the MSVC C++
build tools. Build commands use the committed dependency lockfile.

```bash
cargo run --locked --bin mi-terminal
```

Optimized release build:

```bash
cargo build --release --locked --bins
./target/release/mi-terminal
```

### PTY daemon

The macOS bundle is built with the `daemon` feature and includes
`Contents/MacOS/mi-terminal-daemon`. Terminals can therefore live in a separate
process, so closing (or crashing) the UI does not kill the agents that are
working. The app starts and reconnects to the bundled helper automatically.

Source and development builds keep the feature opt-in. To exercise the same
path on Unix:

```bash
cargo build --features daemon --bins
./target/debug/mi-terminal
```

The app spawns the daemon on first run (`fork+setsid`), reattaches to its own
sessions on restart — same shell, same history, same running processes — and
asks it to shut down once the last app closes and no sessions are left. If the
daemon cannot start, the app falls back to in-process terminals and says why in
the log. A plain `cargo run --bin mi-terminal` does not enable the feature and
uses the in-process runtime.

Daemon clients use bounded output queues: a stalled UI is disconnected instead
of growing memory without limit, then reattaches from the latest snapshot and
sequence number. Scrollback checkpoints are written outside the global session
lock so disk latency cannot block input, resize or attach operations.

The daemon protocol is versioned (currently v4). Each app identifies itself
with a `client_id` at handshake so the daemon can track session ownership: a
live app cannot reconcile away another live app's sessions, and orphaned
sessions are only cleaned up after their owner disconnects. A second daemon
cannot steal an active socket — the existing socket is probed for liveness and
refused connections leave the pid-file untouched. Hot reattach exports a
semantic ANSI snapshot from the live terminal grid (history + visible screen)
instead of a raw byte tail that can start mid-UTF-8 or miss scrollback still in
the grid. Incremental scrollback logs carry monotonic sequence numbers and the
decoder rejects gaps, so a torn append cannot silently corrupt history.
Persistence uses non-destructive snapshots with durable acknowledgements: the
pending log is only trimmed after the worker confirms the append, and failed
appends stay in RAM for retry. Scrollback restore and SQLite memory operations
run on background workers so the UI frame never blocks on file I/O.

Build the distributable macOS app (and, optionally, its DMG) with:

```bash
scripts/bundle.sh
scripts/bundle.sh --dmg
```

Signing and notarization require external Apple credentials; producing a local
bundle is not evidence that a public release has been signed or published. See
[`docs/RELEASE.md`](docs/RELEASE.md) for the current release status.

## Current architecture direction

The current product shape is:

> a native desktop/panel manager for terminals and agent sessions

There is still legacy `canvas` naming inside the repo. That is implementation debt, not the intended product identity.

Shared project memory and task handoffs now have an MVP backed by a private
local SQLite database. The app exposes review/approve/forget UI, `tc-memory`
provides a provider-neutral CLI, and `tc-memory-mcp` exposes read/propose tools:

```bash
cargo run --bin tc-memory -- health
cargo run --bin tc-memory -- remember --cwd "$PWD" \
  --key architecture/auth --content "Use HttpOnly cookies"
cargo run --bin tc-memory -- context --cwd "$PWD"
```

Agent proposals stay pending until a human approves them. Different projects
are isolated by default; worktrees of the same repository share project memory
but keep task handoffs separate. The implemented MVP and the remaining
single-writer daemon / memory-space roadmap are documented in:
[`docs/architecture/shared-agent-memory.md`](docs/architecture/shared-agent-memory.md).

The bundled MCP bridge is project/worktree-scoped through `TC_MEMORY_ROOT`
(exported by TerminalCanvas terminals). Automatic Codex and OpenCode MCP/plugin
registration is not shipped yet; current automatic integration is launch-time
context for providers plus managed lifecycle hooks for Claude Code.

## Development status

The repository is in active consolidation. The main priorities are:

- align public/docs naming with the current desktop product
- harden collaboration privacy and protocol guarantees
- finish splitting shell/runtime responsibilities
- reconcile performance budget docs with the actual UI behavior
- execute the externally credentialed signed/notarized release and publish the Homebrew cask
- connect the existing release checker to a verified download/install flow

## Verification

Runtime and regression coverage lives under `tests/runtime` plus module-local tests.

Run the same checks used by CI with the pinned Rust toolchain:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --all-targets --locked
node --test extension/tests/*.test.cjs
```

On macOS/Linux, repeat Clippy and tests with `--features daemon`. CI covers
Windows x86_64, Linux x86_64, macOS Intel and Apple Silicon. The live PTY test
starts twenty real shells and checks output, resizing and exit handling.

See [support and recovery](docs/SUPPORT.md) for persistence boundaries,
diagnostics and Windows toolchain selection, and [portable packages](docs/PORTABLE.md)
for installation and manual updates.

Optional Ghostty VT backend spike:

> Experimental: requires Zig 0.15.2 and a compatible full Xcode/SDK toolchain.
> It is not part of the production release matrix. On the currently validated
> macOS 26 Command Line Tools-only host, `libghostty-vt-sys 0.1.1` fails while
> linking its Zig build runner before the Rust tests can start.

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

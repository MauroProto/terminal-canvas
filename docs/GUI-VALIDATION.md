# GUI, persistence, provider and Online validation

The CI workflow combines Rust checks on Windows, Linux, macOS Intel and Apple
Silicon with a native GUI scenario on a disposable Linux Actions desktop.
The GUI uses the default WGPU application, X11 input, real PTYs and real files.
It runs with a new `TERMINAL_CANVAS_HOME`, a fixture folder and software graphics.
`scripts/gui-smoke.py` rejects execution outside Linux GitHub Actions.

The native scenario checks:

- Creating and renaming two terminals, splitting one, restoring the window and
  closing and reopening the application with the same profile.
- Keeping independent leaf and runtime identities, the workspace folder and a
  7,000-row history larger than 512 KiB through export and durable restoration.
- Making the data directory unwritable, showing the save error in the actual
  window, preserving the previous layout and recovering after permissions return.
- Closing while a foreground process emits output, saving its history and
  observing the application's exit and the foreground process's physical exit.

The `native-gui-evidence-<commit>` artifact contains screenshots, OCR text,
exports, layout snapshots, application logs and `result.json`. The result records
the source commit, binary SHA-256, completion status and measured close times.
Artifacts expire after 14 days. CI uploads failure evidence too; an artifact's
existence does not mean the scenario passed. Require a successful workflow and
`completed: true` for the same commit.

The Online regression uses a real embedded TLS broker and host/guest WebSockets
with in-memory terminal grids. It checks approval, control grants and that modal
or unfocused guest text never reaches the host. Terminal tests fence input to the
leaf and runtime shown in the snapshot: stale targets are rejected, and legacy
input without a target is accepted only for an unambiguous single-leaf terminal.
Broker regressions also join before the host socket exists, reconnect the host
and recover pending requests without replaying approvals or denials. Membership
updates precede actionable requests; expired requests and departed guests lose
their pending UI entries and control grants, including after a late HTTP failure.

`tests/provider_bridge_process.rs` interprets the app's Claude JSON and Codex TOML
launch settings and starts the configured `--chat-mcp` executable over stdio.
It checks initialization, Unicode message chunking, durable history across a
process restart, access boundaries, live credential rotation, revocation and
bridge cleanup. It does not launch or authenticate a model session. Claude's
history and global hook paths respect `CLAUDE_CONFIG_DIR`; an isolated app profile
continues to prohibit rewriting global provider configuration.

These checks do not cover the native folder picker, native Windows/macOS GUI
interaction, hardware GPU performance, authenticated provider replies, signed
updates or collaboration between two physical machines across a real network.
The measured close times describe the exercised scenarios, not a universal
latency guarantee under slow storage or every possible workload.

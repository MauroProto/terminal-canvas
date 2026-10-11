# Integration of T3 Code reliability patterns

Reference: pingdotgg/t3code at
`5f7294d452a5c6d6e0b7bd2c992478f830fbe212`.
Initial TerminalCanvas base: `c7b989e18a61fbbcadf9dcf7e382ac55128340ef`.
Shared changes included through: `ea6923257adc2af8542e4d2691ee5f7e20af09ad`.

This work adapts patterns to the existing Rust terminal/canvas application.
It does not replace the terminal UI or change online/invitation behavior.
T3 source is a reference; the prompt-submission implementation is original.

## Prompt acceptance

The checked APIs distinguish two results:

- `PromptSubmission::InputQueued`: the complete sanitized input was accepted by
  the bounded terminal writer. This is not proof that an agent read the input,
  completed a task, or that the asynchronous writer has finished writing it.
- `PromptSubmission::Deferred`: the runtime retains one bounded startup prompt
  and returns a receipt. The caller keeps its original feedback/notes until
  that receipt reports input-queue acceptance or failure.

`submit_prompt_to_ready_panel` is for capture and broadcast actions. It does
not start a detached terminal or change the active terminal leaf. A terminal
with startup/deferred input rejects a later submission rather than allowing
it to overtake existing input.

The review updates only the submitted draft/note versions. A newer draft,
edited note, or note added while delivery was pending remains pending.
Closing or replacing the review first applies available receipts. Graceful
shutdown applies them before draining the existing preference-save barrier.
Closing the review then drops its receipt and cancels a prompt that has not
started being accepted. Already accepted input cannot be recalled. Legacy
fire-and-forget APIs explicitly preserve their old lifetime and are not
confirmation APIs for new UI flows.

A second deferred prompt is rejected with `WouldBlock`; it cannot replace
the first. Capacity retries retain the same prompt and receipt. A closed
session, closed input channel, or poisoned terminal handle produces failure
instead of an acknowledgement. Partial broadcast failures preserve the draft
and deselect successful destinations, so retrying does not resend to them.

This startup slot is volatile and does not survive process loss. The separate
conversation journal persists transfer operations; it does not make the
startup slot durable or establish exactly-once external execution.

## Integration boundaries

The subsequent [conversation layer](PROVIDER-CONTINUITY.md) adds a separate
durable journal and cross-provider MCP tools. It uses checked ready-only input,
separate acceptance/result states, explicit links, per-chat credentials, live
bridge leases, frozen previews, and conservative recovery of ambiguous delivery.
The original startup prompt slot remains volatile; the new journal does not
turn that slot into a durable task scheduler or add provider-native steering.

Work was developed on `codex/t3-quality-integration`. Existing worktrees and
shared history are not reset or rewritten. The conversation implementation
builds on the locally integrated `7d130691b0e30e2c3e70432f7f5a22de256f7ae0`.
Configuration and directory-recovery
proposals, and the already integrated diagnostic and rename improvements,
belong to other work streams and are preserved by this implementation.

When combining changes, merge the narrow call-site changes in `src/app.rs` and
the new review state field into existing work. Do not replace those files with
whole copies. Existing test bodies remain; review fixtures add
`pending_delivery: None` and share their empty-workspace constructor.

## Verification scope

Validation uses the pinned Rust 1.98 toolchain, the committed dependency lock,
one Cargo job and one test thread. The original delivery Windows run passed 89 selected
library tests, with zero failures or ignored tests; 25 are new delivery
regressions. The selected library tests cover prompt
receipts, restored terminals, blocked/closed input, feedback and note versions,
broadcast retries, preferences, note persistence, shutdown, layout-save
acknowledgements and the shared rename regressions. Test fixtures use sink PTYs
and controlled workers; the input-writer tests simulate blocked and failed
writers through the real bounded input queue. They do not launch a native
application or an installed provider.

The subsequent conversation/MCP integration passed **112 selected library
tests** on Windows, including persistence, scope/credentials, context budgets,
delegation limits, native argument fixtures, an unfocused-leaf UI exchange,
and shutdown with full queues. Its exact selection and remaining native-client
validation boundaries are recorded in
[PROVIDER-CONTINUITY.md](PROVIDER-CONTINUITY.md#verification).

Formatting is checked with `cargo fmt --all -- --check`; all default targets
are checked with `cargo clippy --all-targets --locked -- -D warnings`.
This Windows validation does not establish Unix daemon behavior, installed
package behavior, process-loss durability, or application performance. Those
require the existing platform CI and the additional evidence below.

## Subsequent work and required evidence

Cross-provider messaging and context continuity are a product priority. The
selected-excerpt feature and the implementation boundaries for durable linked
conversations are described in
[PROVIDER-CONTINUITY.md](PROVIDER-CONTINUITY.md).

1. Integrate and verify the existing typed configuration-recovery proposal,
   preserving its origin and reviewed behavior. Keep the existing diagnostic
   collection report and explicit layout-save acknowledgement behavior.
2. Persist a session descriptor per leaf: cwd/worktree, shell, provider profile,
   native conversation ID, and recovery grade.
3. Introduce a user-owned Windows session host with versioned local IPC and
   process-generation checks. Distinguish UI reconnection from reboot recovery.
4. Extend the conversation journal's existing operation IDs and receipts with
   a native execution outbox. Reconcile ambiguous external outcomes; do not
   claim exactly-once execution.
5. Implement provider capability contracts and actionable permission requests.
   Only expose actions whose installed provider actually supports them.
6. Extend persistent conversation queues with editing, priorities, and native
   task lifecycle events. Keep restart review and explicit readiness; terminal
   output heuristics do not establish authoritative agent availability.
7. Extend the existing selected/omitted message snapshots with versioned complete
   task exports, attachments, checkpoints, and scoped memory retrieval.
8. Add execution-scoped diffs/checkpoints with owned-worktree restoration and
   separate outcomes for filesystem and provider rollback.
9. Measure UI/service/provider resources separately, with bounded histories,
   closed-view subscription cleanup, and concurrency admission under pressure.
10. Extend command/focus routing, project preferences, accessibility checks,
    traceable diagnostics, and service-aware update rollback.

Acceptance must include dead/full input channels, repeated operation IDs,
process loss, suspension, inaccessible configuration, disk exhaustion, stale
permissions, interrupted migrations, and installed-package smoke tests.
Performance comparisons need identical hardware/workloads and measured p95
input/frame latency, RSS, CPU, recovery time, and long idle sessions. Source
inspection and encoded-payload limits do not establish those measurements.

## Primary references

- [Transactional command admission](https://github.com/pingdotgg/t3code/blob/5f7294d452a5c6d6e0b7bd2c992478f830fbe212/apps/server/src/orchestration-v2/EventSink.ts)
- [Durable effects and process-loss policy](https://github.com/pingdotgg/t3code/blob/5f7294d452a5c6d6e0b7bd2c992478f830fbe212/apps/server/src/orchestration-v2/EffectOutbox.ts)
- [Provider adapter boundary](https://github.com/pingdotgg/t3code/blob/5f7294d452a5c6d6e0b7bd2c992478f830fbe212/packages/provider-core/src/server/ProviderAdapter.ts)
- [Queued runs](https://github.com/pingdotgg/t3code/blob/5f7294d452a5c6d6e0b7bd2c992478f830fbe212/docs/user/composer.md)
- [Portable context handoffs](https://github.com/pingdotgg/t3code/blob/5f7294d452a5c6d6e0b7bd2c992478f830fbe212/docs/user/portable-handoffs.md)
- [Background service platform support](https://github.com/pingdotgg/t3code/blob/5f7294d452a5c6d6e0b7bd2c992478f830fbe212/docs/user/background-service.md)
- [Performance regression strategy](https://github.com/pingdotgg/t3code/blob/5f7294d452a5c6d6e0b7bd2c992478f830fbe212/docs/internals/performance-regressions.md)
- [Provider replay harness](https://github.com/pingdotgg/t3code/blob/5f7294d452a5c6d6e0b7bd2c992478f830fbe212/apps/server/src/orchestration-v2/testkit/ProviderReplayHarness.ts)

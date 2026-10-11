# Conversations and context across providers

The product goal is to connect conversations on the canvas: ask another
provider to review an answer, send a follow-up, and continue a task without
rebuilding its context manually. Providers retain their own native sessions;
Terminal Canvas owns the relationships, transferable content, and delivery
records.

## Available first step: share a selected excerpt

1. Select text in the source terminal and open **Broadcast a terminales**.
2. Enable **Incluir selección de …** and inspect **Vista previa del contexto**.
3. Write the instruction for the recipient and choose the destination agents.
4. Click **Enviar**, or use the existing Ctrl+Enter shortcut.

The capture is opt-in. Enabling or disabling it clears destination selections,
so an excerpt cannot silently inherit the ordinary broadcast's select-all
default. Context delivery excludes the captured source leaf and terminals
without a recognized provider hint. Ordinary broadcasts continue to allow
shell commands. The existing destination list represents each panel's focused
leaf, not every leaf inside that panel.

The immutable-in-use snapshot carries the complete selection, capture time,
workspace/panel/leaf/runtime IDs, memory-task identity, source title, provider
hint, and the leaf's reported cwd when available. These are provenance. They
are not a provider-native conversation ID or a guarantee that a particular
process is currently reading the terminal. Provider labels are detection hints,
not capability contracts. Choose the target whose agent is ready to receive
the instruction.

The wire payload separates the current instruction from historical content in
one JSON object. Whitespace and Unicode are retained, control bytes and field
delimiters are JSON-escaped, and no message roles are invented from terminal
output. A terminal selection is not a structured chat transcript. Labeling it
as quoted content does not make a model immune to instructions inside it.

Selection plus encoded provenance must fit 64 KiB; the complete encoded prompt
must fit 256 KiB. These are bounded transport limits, not provider token-window
guarantees. Oversized payloads are rejected without summarizing or truncating
the text, and the draft remains available. The recipient's model may impose a
smaller capacity limit that this terminal transport cannot measure.

The source snapshot and target workspace stay fixed if terminal output or the
active workspace changes. A target whose focused leaf, runtime session, or
provider hint changed is unavailable until the dialog is reopened. Dispatch
uses the existing checked, ready-only terminal input API. Partial failures keep
the instruction and capture and select only failed destinations for retry.
Acceptance means queued terminal input, not provider acknowledgement or an
agent result. No provider is launched as a side effect of dispatch.

This first step has no durable conversation transcript, persistent cross-chat
link, autonomous agent tool, or recoverable transfer journal. Closing the dialog
discards its unsent capture. It does not modify the existing MemoryStore schema,
memory hooks, native resume metadata, or the other work streams' proposals.

## What to adopt from T3

Reference source is pinned at
`5f7294d452a5c6d6e0b7bd2c992478f830fbe212`.

- `t3_thread_list/read/send/wait` gives providers an app-owned interface for
  finding and messaging conversations. Sending has distinct queue, steering,
  and restart semantics, stable retry IDs, and target permission checks.
- Provider adapters distinguish app thread identity from native provider thread
  identity and expose optional capabilities such as historical injection. A
  CLI appearing in PATH is insufficient evidence for these capabilities.
- Portable provider handoffs retain selected full messages, their roles, order,
  and references to omitted history. Capacity accounting leaves room for the
  new instruction and subsequent work. Reasoning, tool-call state, and opaque
  provider-native context are not transferable through the text envelope.
- Delegated tasks have their own identities and result lifecycle. A child turn
  finishing while nested work remains is not necessarily a completed task.

The pinned internal `context-handoffs.md` still describes 240-character
summaries. The actual provider handoff service now uses `selectHistory` and
stores `history.messages` and omission metadata. Summary paths also remain
for fork deltas and legacy compatibility. Read the delivery code and user
documentation together; do not implement every transfer as that old summary.

## Next implementation boundaries

1. **Durable conversation identity and events.** Add a separate app-owned
   conversation journal with versioned message/activity records and source
   provider IDs. Keep per-leaf runtime, native conversation, and memory-task
   identities distinct. Ingest supported provider protocols or explicit user
   captures; never infer an authoritative transcript or run completion from
   arbitrary terminal text. Integrate MemoryStore by IDs rather than replacing
   its schema or treating recalled memories as chat history.
   Keep the current MCP memory caller's task binding: a source memory-task ID
   in a transfer is provenance, not permission to read another task's private
   memories. Cross-conversation retrieval needs its own authorized operation.
2. **Transfer operations and links.** Persist source/target conversation IDs,
   selected message versions, omission references, request, operation ID,
   parent/response relation, and provenance before dispatch. Separate message
   forwarding, task continuation, fork, and merge-back. Resolve repeated IDs
   deterministically. After an ambiguous process loss, show an unresolved
   outcome and reconcile before retrying external execution.
3. **Actual provider capabilities.** Introduce typed adapter contracts for
   reading history, submitting a turn, queueing, steering, stopping, resuming,
   and injecting historical messages. Pin behavior to the installed version;
   use attributed text only where native history injection is unavailable.
   A provider hint and input-queue receipt cannot implement active-run steering.
4. **User-visible context selection.** Preview full selected messages,
   decisions, relevant command outcomes, files and checkpoints, plus omitted
   items and retrieval references. Preserve the new instruction independently.
   Derive budgets from measured/known destination capacity and reserve space
   for tools and work; report estimates and reject insufficient capacity.
5. **Cross-provider agent tools.** Expose list/read/send/wait through the
   app-owned operation service. Authenticate the calling conversation, enforce
   its project scope and target permissions, distinguish explicit routing from
   delegation, bound nested work, and deliver results through recorded links.
   A peer cannot grant itself broader access by choosing a different provider.

These layers are dependencies, not claims about the selected-excerpt feature.
Windows background-host recovery and a durable execution outbox remain the
corresponding steps in [T3-INTEGRATION.md](T3-INTEGRATION.md).

## Verification

Windows validation on pinned Rust 1.98 passed 53 selected library tests,
including 10 new portable-context regressions. The selection also exercises
existing broadcast, review receipt, runtime queue, and bounded writer cases.
The new UI test clicks the context option, chooses a different provider's
destination, and clicks Send through the actual headless egui dialog. Other
cases cover JSON/Unicode/whitespace preservation, encoded-size boundaries,
source exclusion, shell exclusion for context, changed runtime/provider
identity, workspace changes, partial retries, and retained drafts.

`cargo fmt --all -- --check` and default-feature
`cargo clippy --all-targets --locked --offline -- -D warnings` also pass.
Tests use controlled sink PTYs and a single test thread; no installed provider
or native app is launched. This does not establish native provider history
injection, model context-window acceptance, crash durability, or performance.

## Primary sources

- [Thread messaging toolkit](https://github.com/pingdotgg/t3code/blob/5f7294d452a5c6d6e0b7bd2c992478f830fbe212/apps/server/src/mcp/toolkits/orchestrator/tools.ts)
- [Thread fork and merge-back toolkit](https://github.com/pingdotgg/t3code/blob/5f7294d452a5c6d6e0b7bd2c992478f830fbe212/apps/server/src/mcp/toolkits/thread/tools.ts)
- [Provider adapter contracts](https://github.com/pingdotgg/t3code/blob/5f7294d452a5c6d6e0b7bd2c992478f830fbe212/packages/provider-core/src/server/ProviderAdapter.ts)
- [Portable handoff behavior and limits](https://github.com/pingdotgg/t3code/blob/5f7294d452a5c6d6e0b7bd2c992478f830fbe212/docs/user/portable-handoffs.md)
- [Actual handoff selection](https://github.com/pingdotgg/t3code/blob/5f7294d452a5c6d6e0b7bd2c992478f830fbe212/apps/server/src/orchestration-v2/ContextHandoffService.ts)


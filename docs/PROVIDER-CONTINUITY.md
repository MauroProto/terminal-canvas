# Conversations and context across providers

The product goal is to connect conversations on the canvas: ask another
provider to review an answer, send a follow-up, and continue a task without
rebuilding its context manually. Providers retain their own native sessions;
Terminal Canvas owns the relationships, transferable content, and delivery
records.

## Conversations, links, and persistent transfers

Open **Conversaciones y contexto (Chat Connections)** from the command palette.
Each terminal leaf has its own stable conversation identity, including leaves
that do not have focus. Runtime IDs, memory-task IDs, and native provider IDs
remain separate.

1. Choose the source and destination conversations. Plain shells are not
   context-delivery destinations.
2. Save selected terminal text, or select messages already registered in the
   conversation journal. These are explicitly attributed records; arbitrary
   terminal output is not converted into invented user/assistant messages.
3. Write the new instruction and click **Preparar y revisar contexto**.
   The preview shows complete included messages and retrieval references for
   omissions. The new instruction is retained independently and never shortened.
4. Click **Guardar transferencia y enviar**. The app validates the exact
   preview and target binding again, and saves the operation before dispatch.
5. Read its state and eventual result in **Transferencias y respuestas**.
   Open an operation to read the full instruction, preserved context, omitted
   messages, and paginated result; this view also handles approvals and recovery.

Links are explicit and scoped to one workspace. **Guardar vínculo en ambas
direcciones** enables peer context lookup. Agent messages require approval by
default. Enabling **Permitir envíos automáticos entre estos chats** changes
that grant in both directions. Removing a directional link cancels pending
agent requests in that direction without cancelling human-authored requests.
Revoking a chat's credential cancels its undispatched agent requests, including
requests held for session review. Issuing a new credential does not resurrect
those cancelled requests.

The journal lives in `data/conversations.sqlite3` in the selected app profile.
It uses a versioned SQLite schema, WAL, full synchronous commits, foreign keys,
atomic request/result writes, and stable request IDs for retries. Future schema
versions are refused. Selected messages keep their role, original provider and
reported native session ID even after the source changes providers. Selection
ordering remains chronological; budget selection first reserves all omission
references. The default budget is 16 KiB of encoded JSON, configurable from
1 to 64 KiB. Messages/results are limited to 1 MiB, selections to 100 messages,
and outstanding requests to 128 per destination. These are application bounds,
not measured model token limits.

Database and credential-file work runs on a bounded background worker. Only
the existing checked terminal input submission runs on the app thread. The
destination's workspace, panel, leaf, runtime, and binding are validated without
changing focus or starting a process. The journal's closing barrier drains
bounded output while following queued receipt writes, including deliveries
already prepared when closing begins. Closing never submits new terminal input
and reports unconfirmed writes instead of marking a clean shutdown.
Automatic delivery is paused when this app loses profile
ownership; outstanding receipt writes still preserve what already happened.

### MCP bridge and provider setup

New **Claude Code and Codex** launches through the app get a scoped stdio MCP
configuration, including launches with an empty initial brief. The helper adds
native CLI arguments before the prompt and keeps the original brief as a
native argument. It never edits global provider settings or replaces other
MCP servers. Restoring/resuming an old native session follows the existing
resume mechanism; its bridge may need to be imported again.

For an already running agent or another MCP-compatible provider, select its
source chat and use **Configurar MCP de este chat**, then import the generated
configuration using that provider's own MCP format; the exported JSON contains
the `command`, `args`, and `env` to use. Claude accepts the file directly through
`--mcp-config`. This renews the chat credential and
revokes the previous one. The provider must support a local stdio MCP process;
there is no claim that every listed provider accepts the same native flags.
The app binary serves this bridge with `--chat-mcp`, without opening a GUI.

| Tool | Purpose |
| --- | --- |
| `tc_chat_list` | Paginate the caller's chat and authorized linked chats. |
| `tc_chat_read` | Paginate history indexes, or read full message text by UTF-8 offset. |
| `tc_chat_record` | Register an explicit, self-reported message in the caller's chat. |
| `tc_chat_send` | Persist an instruction and chosen context for an authorized peer. |
| `tc_chat_wait` | Query state and read a stored result; bounded wait up to 20 seconds. |
| `tc_chat_reply` | Atomically register an explicit recipient result and finish its operation. |
| `tc_chat_cancel` | Cancel a request whose input has not been dispatched. |
| `tc_chat_ready` | Declare the recipient busy or available, including a leaf without focus. |

The MCP caller comes from a private credential file, not tool arguments. The
database stores a credential digest and authenticates every tool call, including
after initialization and during waits. Tools cannot create links or elevate
their caller's permissions. MemoryStore's independent task/project boundary
is preserved: a memory-task ID in conversation metadata grants no memory access.

The bridge maintains a short-lived connection lease. Revoked/rotated credentials
and disconnected MCP clients cannot receive queued automatic input. An explicit
busy declaration takes priority over idle heuristics; availability permits
delivery to a leaf without focus. Without a declaration, the existing observed
idle/done status is used. **Entregar ahora al terminal** is an explicit user
action for a known pending operation; it still requires a live MCP connection,
the same destination binding, and no previous outstanding delivery.

### Delivery and recovery semantics

Requests are serialized per destination until an explicit result arrives.
Agent sends while that chat has a current received operation are tracked as
children of that operation. A chain cannot revisit its ancestor chats and is
limited to eight levels and 64 total transfers per root operation. Replies
are refused while any child recorded in this journal remains outstanding;
provider-internal subagents are not automatically part of this journal.
`input_queued` confirms the bounded terminal writer accepted input; it does not
prove the provider read it or completed work. Agent replies are explicit reports,
not independently verified task success. A user may also record the actual
result from the operation view if a client cannot call the reply tool.

After a different app/runtime binding, pending operations require review.
An operation left in `dispatching` after a process loss is ambiguous and is
never automatically repeated. Open it, inspect the recipient, and confirm
whether it was delivered, record its result, or mark it undelivered. A new
request requires a fresh operation ID. Already accepted terminal input cannot
be recalled. Pending operations and frozen context survive reopening the
journal; the app does not promise exactly-once external execution.

Delivered context grants the recipient access to the specifically selected
message IDs, including omitted references, without granting the entire source
history. This operation-specific grant persists with the delivery record;
unlinked general history remains inaccessible and revoked caller credentials
cannot use either path.

The journal captures initial app launch briefs, requests sent through this
service, accepted incoming prompts, explicit replies, MCP-registered messages,
and user-saved selections. It does **not** automatically ingest every native
TUI conversation, reasoning trace, tool event, image, or attachment. Unsent UI
drafts are volatile. Model-specific history injection, steering of active turns,
fork/merge-back, and Windows background-host recovery remain future work.

## Existing broadcast: share a selected excerpt

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

The existing broadcast dialog retains its original transient semantics; use
the conversation hub for persistent messages, links, and transfers. Closing
the broadcast dialog discards its unsent capture. Neither flow replaces the
existing MemoryStore schema, memory hooks, or native resume metadata.

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

## Further integration boundaries

The journal, links, explicit history, reviewed transfers, scoped tools, and
bounded parent/child operations are implemented. Further work needs evidence
from supported native protocols and installed clients:

1. **Native history ingestion.** Read supported provider events and native
   conversation IDs rather than reconstructing messages from terminal output.
   Preserve message roles, tool outcomes, attachment provenance, and permission
   requests. Keep runtime and memory-task identities distinct; a memory-task ID
   remains provenance, not permission to read another task's private memories.
2. **Actual provider capabilities.** Introduce typed adapter contracts for
   reading history, submitting a turn, queueing, steering, stopping, resuming,
   and injecting historical messages. Pin behavior to the installed version;
   use attributed text only where native history injection is unavailable.
   A provider hint and input-queue receipt cannot implement active-run steering.
3. **Destination capacity and richer context.** Extend the existing selection
   with file snapshots, attachments, command outcomes, and checkpoints.
   Derive budgets from measured/known destination capacity and reserve space
   for tools and work; report estimates and reject insufficient capacity.
4. **Native task continuation and fork/merge.** Connect provider-internal child
   tasks and native run outcomes to the app journal. Implement explicit fork
   and merge-back semantics with separate filesystem/provider outcomes.
   Reconcile ambiguous external effects through an execution outbox; an
   idempotent app operation does not establish exactly-once provider execution.
5. **Background recovery.** Add a Windows session host and versioned local IPC
   before promising terminal continuity while the UI is closed or after a
   reboot. These service boundaries are also tracked in
   [T3-INTEGRATION.md](T3-INTEGRATION.md).

## Verification

Validation uses pinned Rust 1.98, the committed dependency lock, one Cargo job,
one test thread, and an isolated build directory. The final Windows run passed
**112 selected library tests**, with zero failures or ignored tests.
The focused suite includes
the conversation journal/MCP, headless conversation UI, native launch argument
fixtures, portable context, broadcast, review receipts, runtime queues, bounded
writers, preference shutdown, layout acknowledgements, and shared rename cases.

Conversation coverage includes database reopen, concurrent/idempotent requests,
explicit cross-provider replies, credential rotation/revocation, scope and
identity spoofing, future schema refusal, frozen preview revalidation, UTF-8
chunk retrieval, omission budgets/grants, bounded metadata pages, nested
delegation cycles/depth, one outstanding delivery per target, and ambiguous
delivery recovery. The headless UI clicks preparation and sending through the
actual dialog, delivers to an unfocused split without changing focus, collects
an explicit reply, and retains a draft edited during a delayed save. Closing
with both bounded channels full must persist accepted records and resolve a
late delivery without writing terminal input.

`cargo fmt --all -- --check` and default-feature
`cargo clippy --all-targets --locked --offline -- -D warnings` also pass.
Tests use controlled sink PTYs, temporary databases, and argument-capture
executables; no installed provider or native GUI app is launched. This does
not establish native provider history
injection, model context-window acceptance, crash durability, or performance.

The library selection used was:

```text
cargo test --lib --locked --offline --jobs 1 --target-dir target/t3-quality -- --test-threads=1 conversation:: app::conversation_ui:: app::broadcast_ui:: portable_context orchestration::agent_launcher:: prompt_delivery runtime::pty_manager:: input_writer:: app::preferences_shutdown_tests:: app::preferences_ui_tests:: layout_save rename
```

## Primary sources

- [Thread messaging toolkit](https://github.com/pingdotgg/t3code/blob/5f7294d452a5c6d6e0b7bd2c992478f830fbe212/apps/server/src/mcp/toolkits/orchestrator/tools.ts)
- [Thread fork and merge-back toolkit](https://github.com/pingdotgg/t3code/blob/5f7294d452a5c6d6e0b7bd2c992478f830fbe212/apps/server/src/mcp/toolkits/thread/tools.ts)
- [Provider adapter contracts](https://github.com/pingdotgg/t3code/blob/5f7294d452a5c6d6e0b7bd2c992478f830fbe212/packages/provider-core/src/server/ProviderAdapter.ts)
- [Portable handoff behavior and limits](https://github.com/pingdotgg/t3code/blob/5f7294d452a5c6d6e0b7bd2c992478f830fbe212/docs/user/portable-handoffs.md)
- [Actual handoff selection](https://github.com/pingdotgg/t3code/blob/5f7294d452a5c6d6e0b7bd2c992478f830fbe212/apps/server/src/orchestration-v2/ContextHandoffService.ts)
- [Claude CLI configuration arguments](https://code.claude.com/docs/en/cli-reference)
- [Codex stdio MCP configuration](https://developers.openai.com/codex/mcp/)


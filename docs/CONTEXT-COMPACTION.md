# Context usage and compaction

A conversation shows how full its coding agent's context is, shows when the agent
compacts that context, and can compact it on request. The visible Cairn transcript is
never rewritten: compaction only changes what the native session sends to its model.

## Context usage

Each adapter emits `{"type":"context.usage","usage":{...}}`. The worker stores it as a
conversation event and keeps the latest one as `run.contextUsage`. Chat views expose it as
`contextUsage`, and the public MCP and `cairn_installation` chat summaries include it.

| Field | Meaning |
| --- | --- |
| `contextTokens` | Tokens the model's latest request occupied in its window. |
| `contextWindow` | The model's window, or `null` when the engine does not report it. |
| `cachedTokens` | Of `contextTokens`, input read from the prompt cache, or `null`. |
| `estimated` | The engine's local estimate rather than a model measurement. |

The numerator is never a conversation's cumulative spend. Dividing a thread's total
tokens by the window would report occupancy that does not exist.

- **Codex** (`thread/tokenUsage/updated`): `last.totalTokens` is the latest request. The
  Codex TUI documents it as "the latest active context size" (`codex-rs/tui/src/token_usage.rs`,
  `rust-v0.164.0`). `total` accumulates the whole thread and is ignored. `modelContextWindow`
  is nullable. After compaction Codex reports a local estimate as a request without input
  (`Session::recompute_token_usage`), which Cairn marks `estimated`. Codex also replays the
  latest usage when a saved thread is resumed.
- **Claude Code**: every main-thread API response's `usage`, counting
  `input_tokens + cache_creation_input_tokens + cache_read_input_tokens`. Claude Code computes
  `/context` occupancy with this same formula. The window comes from the stream-json control
  request `get_context_usage` with `detail: "summary"`, which answers locally without a
  token-count or model call. Before the first response and after `/compact` its total is a
  local estimate (`apiUsage: null`). Subagent responses use their own context and are
  excluded, as are synthetic local-command replies.
- **OpenCode**: no usage is reported and compaction is unsupported, so clients show the usage
  as unavailable.

The web header and both details sheets show `used / window · percent`. The percentage
appears only when both values are known. Otherwise they show "window unknown". With no
report, they show "unavailable". The share is raw occupancy: it does not subtract Codex's
fixed 12,000-token baseline the way the Codex TUI's "context left" does.

## Compaction events

Codex `contextCompaction` items and Claude Code `status: compacting` / `compact_boundary`
messages become `context_compaction` items. Their status is `in_progress`, `completed` or
`failed`. Claude Code also reports `trigger` (`manual`/`auto`), `pre_tokens` and
`post_tokens`; Codex reports only the lifecycle. Both clients render a divider where the
compaction happened. Earlier messages stay in place, and the compaction never shows as a
reply or a finished task. Claude Code's injected summary message is not shown.

A coding agent can fail or be stopped without closing the compaction it started: Codex then
fails its turn without completing the item. A divider still running when its run's turn ends
or fails, or the run stops, therefore ends too: failed with the turn or run, otherwise
"stopped". It never shows as completed without the coding agent's confirmation.

## Explicit compaction

`POST /api/chats/:id/compact` and the `compact_chat` MCP tool (scope `run`, also on
`cairn_installation`) start a compaction. It runs as its own execution of the
conversation's run, with `chatExecution.operation = "compact"`. It is not a user message:
it is not queued, acknowledged or added to the transcript.

- **Availability.** `chat.compaction` = `{ supported, running, available, reason }`.
  Compaction requires all of the following:
  - a Codex or Claude Code session saved by a turn that succeeded;
  - an idle run with no recovery pending;
  - no queued or sending messages;
  - no scheduled continuation;
  - no pending fresh-session request;
  - a workspace that has not been cleaned.
  Codex `thread/compact/start` replaces any running turn with its compaction turn
  (`compact()` aborts all tasks in `codex-rs/core/src/session/handlers.rs`), so Cairn never
  overlaps a compaction with a turn.
- **Native controls.**
  - Codex resumes the thread and calls `thread/compact/start`. It then follows the
    compaction turn and its `contextCompaction` item. If the turn completes without that
    item, the compaction fails.
  - Claude Code resumes the session and sends the local `/compact` command. Claude Code
    2.1.280 runs it without a model turn for the command itself, then emits `compact_boundary`.
    A successful result without a boundary fails with Claude Code's own message, for example
    "No messages to compact". At the boundary Cairn asks for a context report, which Claude
    Code answers after the command's result. The execution waits for it, up to five seconds,
    so the run shows the compacted estimate. A report requested before a later measured
    response is ignored, as it describes the older context.
- **Queue and steering.**
  - While a compaction runs, new messages stay queued. They are not steered into it, and
    they start the next turn once it ends.
  - A queued message submitted before the request blocks the compaction rather than being
    reordered.
- **Stop.** The existing stop operation cancels the compaction like a turn and pauses the
  queue.
- **Result.**
  - Success leaves the run `succeeded`, with its previous summary, outcome and `finishedAt`
    restored. It does not mark the conversation unread or newly finished.
  - Failure shows its reason like a failed turn.
- **Recovery.** A compaction interrupted by a restart runs again: it has no external effect.
  Claude Code writes no completion receipt for it, so no empty reply is ever replayed. One
  that completed before the restart settles like a normal completion, keeping the previous
  summary and `finishedAt`.
- **Authorization.** The route and tool use the conversation's native grant like sending a
  message, because compaction spends the agent's model usage. MCP requires the `run`
  scope; reading alone is refused.

## Validation

- **Backend.**
  - `context_usage` unit tests: Codex `last` vs `total`, unknown windows, estimates and the
    Claude formula.
  - Adapter fixtures: `tests/chat_process.rs` and `tests/claude.rs` cover usage, automatic
    and explicit compaction, no steering, failures and missing sessions.
  - Chat state and availability are covered in `chats::tests`, worker persistence in
    `worker::output::tests`, and MCP parity in `tests/installation_mcp.rs`.
- **Clients.**
  - Web: `context-usage.test.ts`, plus the Chromium journeys in `tests/e2e/chats.spec.ts`
    on desktop and phone widths.
  - Android: `ContextUsageTest` and `ContextCompactionJourneyTest`.
- **Engine protocol evidence.**
  - Codex: the 0.164.0 JSON schema (`codex app-server generate-json-schema`) and the
    `rust-v0.164.0` sources.
  - Claude Code: 2.1.280 `get_context_usage` and `/compact` were observed over stream-json
    against a local mock Messages API, without credentials or subscription usage.

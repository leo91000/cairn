# Claude Code

Léo can run the official Claude Code CLI alongside Codex. Claude accounts are
added, selected and limited like Codex accounts: see [coding-agent accounts](AGENT-ACCOUNTS.md)
for sign-in, usage windows, parallel runs, credentials and failover.

Choose **Claude Code** in an agent’s **Coding agent** setting. Existing agents
remain on Codex. Model and effort choices come from the official CLI’s control
protocol, without a model inference request. A saved catalog remains available
while the account is executing. When discovery fails, the UI marks the catalog
unavailable rather than inventing current model capabilities.

Claude agents support scheduled tasks, chats, streaming text and tool results,
image/file attachments, queued follow-ups, steering, native questions, cancellation,
and resumption. Changing a model applies to the next queued turn. The chat composer
also switches between Codex and Claude Code without creating another visible chat.
The server waits for the current turn to finish, starts a fresh native session,
and transfers visible conversation context while retaining the same workspace.
Switching back also starts a fresh session so the intervening work is included.
Native session IDs, pending tool calls and hidden reasoning are not transferred.

Resuming a saved Claude session sends a fresh protocol message ID while keeping
the original Léo message and completion receipt. Claude ignores IDs already in
its transcript, so replaying the original ID would acknowledge the request without
continuing it. Completed receipts are replayed without starting another model turn.

Before resuming, Léo checks the selected session's generated `session-env/<session-id>/*.sh`
files for NUL bytes. Corrupted files block launch with an error identifying the file.
If Claude reports the same shell initialization failure during a turn, Léo stops
that process and marks the attempt failed instead of accepting a later successful
turn result. The error's raw environment exports are not included in the activity
log, and no completion receipt is written for that attempt.

Repair the affected script in the conversation's retained Claude home, then resume
the same conversation. Resume starts a fresh process so cached corrupted text is
discarded. Léo preserves the script, transcript and workspace; it does not guess
replacement shell code, delete hooks, or retry automatically. This detects and
contains corruption but does not identify or repair the cause of the invalid bytes.

Private question answers and raw tool outputs are excluded from the transfer.
For long chats, the transfer includes up to 200 recent transcript entries and
100,000 characters, plus an excerpt of the initial request when earlier history
is omitted. The full visible history stays available in Léo. Previous attachments
remain in the retained input directory. Model and effort defaults are reset when
changing providers; the agent's global configuration is unchanged.

Cairn sets `CLAUDE_CODE_DISABLE_BACKGROUND_TASKS=1` for every Claude launch,
including resumed sessions and both native and microVM execution. This disables
Claude's background Bash commands, background subagents and automatic backgrounding.
Commands that reach their timeout stop instead of continuing in the background;
long builds need an appropriate foreground timeout. Use Cairn's durable
[`wait_for`](WAIT-FOR.md) for CI and other supported waits, so the execution can
release its resources while waiting. See the official
[background command behavior](https://code.claude.com/docs/en/tools-reference#foreground-commands-that-move-to-the-background).

The adapter correlates SDK background notifications with the current session.
Claude's turn results can cover several user messages, or background notifications
unrelated to the current prompt. Léo correlates completion with consumed message
IDs and keeps the process alive while non-ambient background tasks and their
follow-up responses are pending. This SDK work remains part of the
running conversation. Ambient watchers do not keep a run open.
When Claude finishes responding and only those tasks keep the run open, Léo emits
`turn.waiting` with each task's description, and an empty list once they finish.
The conversation then shows « Veille » instead of the working indicator, and the
activity heartbeat reports `waiting_for_background` with the number of tasks.
Completion receipts record acknowledged requests and prevent completed messages
from being repeated after a restart.

## Engine status and subagent identity

Léo reads Claude Code's structured stream-json frames rather than its wording. Each
kind of wait stays separate:

- **Engine retry.** `system/api_retry` becomes `engine.retry`: attempt, retry budget,
  delay, HTTP status, reason and, for a first-byte timeout, `noResponse`. Claude Code
  retries the request itself. The turn continues, so this is neither a failure nor a
  receipt. The conversation shows the latest retry until work continues: the reply
  streams, a tool (a subagent among them) starts or the turn completes. The run log
  then marks it recovered. A subagent's progress or resumption does not count. When
  Claude Code gives up, it reports the last API error as a message and the turn fails;
  that retry is not marked recovered. Neither is a retry whose execution is stopped,
  interrupted, failed by the runner or replaced by a new attempt: the conversation stops
  showing it. These are the
  agent's own requests: Claude Code reports a subagent's retries in its tool progress
  (`subagent_retry`), which Cairn does not show.
- **Provider usage limit.** `rate_limit_event` becomes `provider.limit` when its state
  changes. That covers a warning, a limit covered by extra usage, a limit that stops
  requests (`blocked`), and the return to normal after one of these. Window and reset
  time are included. The conversation shows it until the limit no longer applies, its
  reset time passes, or its execution ends or fails: a later execution, perhaps a new
  attempt on another account, reports its own state. If a
  turn fails because the usage limit refused the agent's request (its message carries
  `error: "rate_limit"`) while the latest state blocks requests, the adapter fails it
  with `code: usage_limit_reached`, the window and `resetsAt`. The worker then takes Cairn's durable account wait or fails over, without
  matching Claude's wording. A limit Claude Code answered after (for example a limit on
  one model's weekly window, with the reply coming from another model), one that only
  refused a subagent's request, or one whose reset time has passed does not explain the
  failure: the turn fails as usual.
- **Cairn durable wait.** Account waits, `wait_for` and automatic retries are Cairn's
  own. They release the execution's resources. Léo does not enable Claude's unattended
  `CLAUDE_CODE_RETRY_WATCHDOG`, which would keep the process waiting for a usage window
  instead of releasing it.
- **Notices.** `system/informational` becomes `engine.notice` with its level, cut at
  4,000 characters. Claude Code adds these frames to its stream-json output from
  2.1.283; 2.1.280 drops its turn warnings there, so it runs without notices. Like all run output, it is
  redacted before storage. Warnings, suggestions and notices that stop the turn show in
  the conversation and are always recorded. Notices and info lines stay in the run log,
  within its diagnostic budget. Claude Code repeats a running tool's progress line
  (same `tool_use_id`); it is reported once. Warnings, suggestions and notices that stop
  the turn are always reported.

`session_state_changed` is opt-in (`CLAUDE_CODE_EMIT_SESSION_STATE_EVENTS`) and
not enabled. Completion stays correlated with result messages and consumed message
IDs. Léo answers permission requests itself, so `requires_action` would add no state.
Frames Léo does not know, including future ones, are ignored.

Subagents are followed by the tool use that started them and by their task ID. Claude
Code 2.1.292 and later add `agent_id` to a subagent's messages (equal to its task ID,
kept when resumed), plus `parent_task_id` and `run_id` to task frames. From then on, a
subagent's messages are never taken as the agent's reply, even without
`parent_tool_use_id`. Older frames rely on `parent_tool_use_id` alone.
When the agent resumes a subagent, its new run reopens the same item, marked
`resumed`. That holds even when the run names another tool use. Claude Code promises
that a task's run IDs sort, by plain string comparison, in the order its runs opened,
so a frame naming an earlier run, or repeating the start of the run that reported, is
late and ignored. A subagent an earlier execution started, resumed by a `SendMessage`
call or by Claude Code itself (its run then names the earlier execution's call, or no
call), shows as a new row of the current execution marked `resumed`, keyed by its task
and run IDs: Cairn rows belong to one execution, and a later execution that resumes it
again gets its own row. A call this execution never made only counts
for a top-level subagent (no `parent_task_id`, `spawn_depth` 1). Other unknown agent
tasks, such as a workflow's agents, nested agents started from work this execution does
not see, or housekeeping agents Claude Code marks `skip_transcript` or `ambient`, are
ignored. Nested subagents keep their parent and `parent_task_id`. Without `agent_id`,
a resumed subagent's messages may name the call that resumed it (such as
`SendMessage`) in `parent_tool_use_id`; once a task frame ties that call to the
subagent, they stay its steps. The minimum supported CLI stays **2.1.280**: older
frames without these fields keep the earlier correlation, and the fixtures cover both
shapes. The events reach the public MCP and `cairn_installation` through
`get_run`.

An explicit provider choice is saved with each queued message. Steering cannot
change the running turn's provider. Messages that omit the provider continue
with the chat's current provider. Completion receipts prevent finished requests
from being repeated after a worker restart. Interrupted requests preserve completed
work and verify external effects before continuing.

The design review used [T3 Code](https://github.com/pingdotgg/t3code/tree/f5ef0ddb90a8c36584e181b1913e7b8a5df30ffc),
under its MIT license. Its provider reactor separates visible threads from native
sessions, but explicitly rejects switching drivers on an existing thread. Léo's
context transfer is an independent implementation, not a port of a T3 Code feature.
No T3 Code runtime dependency or source code is included.

## Installation

The manager and guest images include unmodified Claude Code from Anthropic's
stable release channel, resolved for each image build (see
[tool versions](CLI-UPDATES.md)); `/health` reports the exact version.
For development, install that release and set `CLAUDE_BIN` if it is outside PATH.
The CLI owns the interactive login flow and each account’s private state. Léo never
exposes access/refresh tokens through its public API. Identity responses are
restricted to connected state, email and subscription type. There is no API-key
fallback and no change to the selected provider or subscription billing.

Léo clears inherited Anthropic API-key, bearer-token, cloud-provider, and alternate
OAuth environment overrides from subscription executions. Sign-in and logout are
performed with `claude auth login --claudeai` and `claude auth logout`.

## Permissions

YOLO remains the default and runs within the existing private microVM boundary.
Workspace-write and read-only agents enable Claude’s Bash sandbox with
`failIfUnavailable` and no unsandboxed fallback. The images include Bubblewrap and
socat. Read-only also denies edit tools and retains read-only project mounts.
Only the agent’s configured MCP connections are supplied; HTTP tools keep Léo’s
server-enforced grants, and excluded command-MCP tools are denied explicitly.
Shared/project Claude settings are disabled for the managed invocation. Selected
Léo skills and instructions are supplied through the existing run context.

## Subscription usage

This is programmatic Claude Code usage. Anthropic’s June 15 update currently says
that the proposed separate SDK billing change is paused and subscription-backed
SDK/CLI usage still draws from subscription limits. Léo does not promise permanent
pricing or add paid fallback. Consult [Anthropic’s current billing guidance](https://support.claude.com/en/articles/15036540-use-the-claude-agent-sdk-with-your-claude-plan).

Hosting follows the [Claude Code hosting and credential rules](https://code.claude.com/docs/en/legal-and-compliance): the binary stays unmodified and sign-in completes through Anthropic’s own flow using the owner’s account. The implementation uses the [official streaming CLI](https://code.claude.com/docs/en/headless) and its SDK control protocol.

## Validation

`crates/installation/tests/claude.rs` exercises the streaming adapter and account lifecycle with
a synthetic executable. `tests/e2e/claude.spec.ts` exercises the native backend from
Chromium and WebKit, including mobile layouts and resumed conversations.
`AccountsJourneyTest` covers Android’s code-entry flow and provider serialization.
These fixtures never contact Anthropic or spend subscription allowance.
CI runs every resolved release through the adapter's native tests: credential-free
model discovery, including the effort levels agents may save, usage and
authentication status, a turn against a local Messages API, and session
resumption with its oldest qualified release and the release production serves
(see [tool versions](CLI-UPDATES.md)). Catalogs depend on the release and the
account. With synthetic subscription credentials, 2.1.308 lists `opus` and
`fable` where 2.1.280 lists `opus[1m]` and `claude-fable-5-1`. Both remain
documented `--model` values, so saved agents keep working. Model-scoped
quota windows, such as "Weekly · Opus", cover the models of the current catalog
and saved models of their family that it no longer lists, such as `opus[1m]` or
`claude-opus-4-6[1m]` ([#390](https://github.com/leo91000/cairn/issues/390)).

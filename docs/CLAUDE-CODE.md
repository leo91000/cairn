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

The adapter retains background notification handling for existing session history.
Claude's turn results can cover several user messages, or background notifications
unrelated to the current prompt. Léo correlates completion with consumed message
IDs and keeps the process alive while non-ambient background tasks and their
follow-up responses are pending. Such legacy background work remains part of the
running conversation. Ambient watchers do not keep a run open.
When Claude finishes responding and only those tasks keep the run open, Léo emits
`turn.waiting` with each task's description, and an empty list once they finish.
The conversation then shows « Veille » instead of the working indicator, and the
activity heartbeat reports `waiting_for_background` with the number of tasks.
Resume ignores legacy completion receipts that never acknowledged the request,
preventing an empty result from repeatedly restarting the same pending message.

An explicit provider choice is saved with each queued message. Steering cannot
change the running turn's provider. Older clients that omit the provider continue
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
The real installed 2.1.280 CLI was also checked with a non-inference initialization
request, verifying its current model names and effort capabilities. Stable 2.1.308,
the first automatically resolved release, answered the same credential-free
request with the same model fields plus `supportsThinkingOff`. Its default catalog
lists `opus` and `fable` instead of `opus[1m]` and `claude-fable-5-1`. Both remain
documented `--model` values, so saved agents keep working. It ships the same OAuth
client and token endpoint. CI now runs every resolved release through the adapter's
native tests against a local Messages API, including session resumption with the
release production serves (see [tool versions](CLI-UPDATES.md)). Model-scoped
quota windows, such as "Weekly · Opus", cover the models of the current catalog.
An agent saved with an alias the catalog no longer lists, such as `opus[1m]`,
still runs, but only account-wide windows limit it until another model is chosen
([#390](https://github.com/leo91000/cairn/issues/390)).

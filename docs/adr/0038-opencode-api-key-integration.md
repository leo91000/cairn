# OpenCode through native sessions and model-provider API keys

Date: 2026-10-13.

## Context

ADR-0037 separates native execution adapters from shared worker orchestration.
OpenCode is the first additional engine using that boundary. Its engine identifier
is independent from the model provider: Zen, OpenAI and OpenRouter credentials
cannot be interchanged just because they use the same executable.

## Decision

Register `opencode` as a coding agent with an API-key account driver and a native
HTTP/SSE execution adapter, pinned to OpenCode 1.19.0. Each account binds to one
model provider. Keys use the installation's encrypted vault and existing private
run-scoped credential broker. They are passed to the native server through
`OPENCODE_AUTH_CONTENT`, not a native authentication file. Public account views,
model metadata and audit records do not include keys.

The native server listens on loopback with a random password, private XDG state
and explicit native config. The pinned release's `OPENCODE_TEST_HOME` isolates
native global config discovery while preserving shell HOME and GitHub tooling.
Project configs, automatic updates, sharing and inherited provider environment
keys are disabled. The manager serializes native MCP servers and tool permissions
from the existing engine-independent access configuration.

Models use qualified `model-provider/model-id` identifiers. Native model inventory
and variants populate the web and Android pickers; only connected model providers
appear. OpenCode Zen is named explicitly to distinguish its API key from a Cairn
account and the OpenCode engine. Replacing a key keeps its model-provider binding
and waits for active or recoverable executions to finish.

The adapter translates native delivery, text, reasoning, tool activity and questions
into existing conversation events. A durable submission receipt acknowledges inbox
messages and replays completed results without sending a second model request.
Interrupted recovery inspects native history and asks the agent to preserve work
and verify external effects. Cancellation tears down the native process group.
Terminal truncation and policy stops fail visibly instead of waiting indefinitely.
Automatic compaction and pruning are disabled in this first integration. Unexpected
native continuation user IDs fail visibly, including during recovery, rather than
being mistaken for assistant activity or leaving an unowned response running.
Expired question answers fail visibly and delivered answers are not replayed.

## One model through several connections

Go, Zen and direct provider keys can offer the same model under different names,
such as `opencode-go/claude-sonnet-4.5` and `openrouter/anthropic/claude-sonnet-4.5`.
The catalog groups them by their last path segment, case-folded with `.` and `_`
read as `-`. Each group is one picker entry, and each connected provider offering it
is a route. Names that do not match stay separate entries, so a missed match costs a
duplicate row, never a wrong provider.

An entry's own identifier, `auto/<model>`, runs on the first route available in the
user's connection order. OpenCode accounts rank by that order before the pool's
usual capacity and recent-use rules; the launch then resolves the leased account's
provider to its concrete `provider/model` and records it as a conversation status.
A rate limit uses the existing exhaustion flow, so the resumed session continues on
the next route. A route's `provider/model` pins that provider. Auto offers only the
variants every route shares, because a fallback must accept the chosen reasoning.

## Initial scope

This integration supports model-provider API keys, including OpenCode Zen. It does
not implement OAuth/subscription sign-in, custom provider endpoints or readable
provider quotas. A stored key is syntactically validated; authentication is checked
by the model provider on execution. Usage is displayed as unavailable rather than
inventing a quota. Native HTTP 429 errors use shared exhaustion/account switching;
without a readable quota the account becomes eligible to retry after five minutes.

OpenCode has no implementation of Cairn's read-only or workspace-write operating
system sandbox. Initial OpenCode agents therefore require YOLO access; the adapter
rejects restricted policies. Production executions use the existing isolated runner
requirements. The native CLI version is checked at startup, including catalog
discovery; operators must install the pinned release on the manager and runners.

## References and validation

- [OpenCode native server](https://opencode.ai/docs/server/)
- [Pinned global paths](https://github.com/anomalyco/opencode/blob/v1.19.0/packages/core/src/global.ts)
- [Pinned session prompt loop](https://github.com/anomalyco/opencode/blob/v1.19.0/packages/opencode/src/session/prompt.ts)

Synthetic protocol tests exercise private credentials, account fencing, output,
completed-receipt replay, cancellation and terminal errors. Browser journeys drive
real Cairn processes with a synthetic native server and capture actual account,
model-picker and conversation screens. An optional pinned-native smoke uses a local
model endpoint and a poison global plugin config; it spends no provider capacity.

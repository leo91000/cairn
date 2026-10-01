# Measuring agent execution latency

Agent timings use the existing `leo_performance` tracing target, enabled by the
default binary filter. With a custom filter, include `leo_performance=info` in
`RUST_LOG`. These records do not change database schema or restart policy.

## Correlation

Worker logs include `run_id`. Every process launch receives a fresh `attempt_id`,
also passed as optional `runId`/`attemptId` fields in the chat plan. The adapter
places native RPC and output logs in the same `agent_attempt` tracing span.
Retained older adapters ignore the optional fields; plans without them log
`unknown`. Invalid correlation values also become `unknown`.

Activity records have `side=adapter` (normalized events inside the agent process)
or `side=worker` (events received by the manager). Match run, attempt and the
local turn counter when comparing these records. Existing VM boot, preparation,
shutdown and storage records retain their current run/VM identities.
Archive transfers add a UUID linking host and guest phases; see
[VM import timings](VM-IMPORT-TIMINGS.md) for reading, transport, extraction and
acknowledgement measurements. Guest measurements require the updated VM image;
host measurements also work with retained older images.

## Timings

| Operation | What it distinguishes |
| --- | --- |
| `agent_prepare` | Marking running, access validation, workspace setup, session lookup |
| `workspace_prepare` | Execution restoration, account home, environment, MCP configuration |
| `runner_prepare` | Placement, materialization, plan writing, remote preparation, placement commit |
| `runner_entry` | Guest toolchain preparation before the chat adapter starts |
| `account_broker` | Time to acquire managed tokens, before native account login |
| `codex_rpc` / `mcp_rpc` | Request start and completion, method, local request ID, elapsed time, outbound queue time, deadline and success; no parameters or result |
| `codex_transport` | Native HTTP/WebSocket request interval and status, plus selected thread startup phases; fixed endpoint category and sanitized thread UUID only |
| `agent_activity` | Turn start/end, first item, first nonempty assistant message, item start/end and periodic activity summary |
| `agent_output` | Worker pipe-to-consumer queue delays of at least 100 ms |
| `agent_wait` | Awaits lasting at least 100 ms: stdout writing/flushing, credential-redaction refresh and output persistence |

`agent_activity` emits a heartbeat every 30 seconds while its receive loop is
waiting. If output writing or persistence itself blocks, `agent_wait` instead
reports that specific await every 30 seconds until it returns. Timers skip missed
ticks. They neither cancel work nor change an existing deadline.

The heartbeat reports time since stdout activity and since a recognized agent
event, active-item count, the oldest item's category and age, pending blocking
questions, first-activity/message latency and time in the current turn. Stderr
does not reset stdout/event silence. `waiting_for_agent_event` means no tracked
tool or blocking question is open; it does **not** prove the model is thinking.
Likewise, an open tool is an observation, not proof its subprocess is using CPU.

Categories are fixed: command, MCP, file change, web, collaboration, reasoning,
message and other. Item IDs only match starts to ends in memory; logs use a local
numeric sequence. Duplicate starts do not reset the clock, and deltas update
activity counters without emitting a log per token. Completion-only items log
`item_completed_without_start`; their optional `provider_elapsed_ms` is the
provider's duration, not an observed start/end interval. First-message timing is
time to the first nonempty assistant message, including commentary, not the final
answer. Parallel item durations must not be added to estimate wall time.

Codex's `turn_start_source=provider_turn` uses its explicit turn notification.
Claude has no such notification in the existing adapter, so its
`turn_start_source=thread_initialized` measures from the observed thread
initialization instead. These boundaries must not be treated as identical model
request timestamps. Receipt replay without initialization does not invent a turn.

Native transport timings use Codex's local OTLP HTTP/JSON exporter, with user
prompt logging disabled. A loopback collector accepts at most 2 MiB per batch
and discards the raw document after extracting a fixed allowlist. It ignores
token streams, prompts, tool data, arbitrary attributes and native error text.
No raw telemetry is persisted or sent to an external telemetry service. Export
failure does not fail an agent session.

The collector lives for the VM lifetime, so asynchronous export after an attempt
releases its output lease still reaches the private guest console. Outside a VM,
the native session owns its collector. `codex_transport` records from a guest
must be read from that console and correlated by VM, thread and event time;
they do not inherit the current attempt's span. A startup request without a
thread has `thread_id=unknown`.

Codex batches export asynchronously. `completed_at_ms` comes from the native
event's timestamp (`observedTimeUnixNano` when `timeUnixNano` is zero), and
`request_started_at_ms` subtracts its reported duration at millisecond resolution.
This is the native request timer, not a packet capture or collector arrival.
Abrupt native termination can discard its final unexported batch; shutdown never
waits for telemetry. A missing transport record is not evidence of no request.
For the first model request, select `endpoint=responses` and the corresponding
thread; do not substitute `turn_started` or the first exported batch. A real
native fixture checks that this interval brackets the local model HTTP receipt.

Open-item/question tracking is bounded to 128 entries each and 256 bytes per ID.
`skipped_items` makes saturation visible. New turns clear unfinished previous
items. Memory tracking performs no database writes; adapter stderr uses the
existing bounded diagnostic recording path. Logs never include prompts, tool
commands, arguments, outputs, paths, URLs, credentials, arbitrary item types or
error text.

## Reading a slow run

1. Find `run_id`, then the affected `attempt_id` and turn.
2. Check preparation and VM phase records before `turn_started`. An RPC start
   without completion identifies a pending native request (or a cancelled process).
3. During the turn, inspect item intervals and heartbeats. A long command/MCP
   interval differs from silence with no tool open or a pending blocking question.
4. Compare adapter and worker activity, and inspect `stdout_write`, `queue` and
   `persist_output`. This separates agent silence from downstream backpressure.
5. A silence record is not a diagnosis of CPU, disk or provider latency. Use node
   utilization and existing storage/VM timings to test that next hypothesis.

Durations use monotonic clocks within each process. Do not subtract wall-clock
timestamps from different nodes to infer network delay. Logging remains
observational; it does not send test messages or resume production conversations.

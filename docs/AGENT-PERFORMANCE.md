# Measuring agent execution latency

Agent timings use the existing `cairn_performance` tracing target, enabled by the
default binary filter. With a custom filter, include `cairn_performance=info` in
`RUST_LOG`. These records do not change database schema or restart policy.

## Correlation

Worker logs include `run_id`. Every process launch receives a fresh `attempt_id`,
also passed as optional `runId`/`attemptId` fields in the chat plan. The adapter
places native RPC and output logs in the same `agent_attempt` tracing span.
Retained older adapters ignore the optional fields; plans without them log
`unknown`. Invalid correlation values also become `unknown`.

Activity records have `side=adapter` (normalized events inside the agent process)
or `side=worker` (events received by the manager). The agent process writes its
records to stderr; the manager logs each one as `source=agent_process` with the
sanitized line in `record`, rather than adding it to the run's diagnostics. Only
the runner's formatted tracing records with the `cairn_performance` target are
redirected; another logger mentioning that name remains a diagnostic. The
consumer refreshes configured and account-secret redactions before logging these
records, including after the run's diagnostic budget is exhausted. Match run,
attempt and the local turn counter when comparing these records. Existing VM boot, preparation,
shutdown and storage records retain their current run/VM identities.
Terminal styling is removed before framing diagnostics, with parser state kept
across reads and long fragments. Control-sequence bodies are discarded while
streaming rather than buffered, and incomplete UTF-8 characters wait for the next read.
Archive transfers add a UUID linking host and guest phases; see
[VM import timings](#vm-import-timings) for reading, transport, extraction and
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
| `mcp_startup` | Manager HTTP handling of authenticated initialize/tools-list requests, including authorization and dispatch; fixed endpoint category, optional gateway connection UUID |
| `codex_rpc` / `mcp_rpc` | Request start and completion, method, local request ID, elapsed time, outbound queue time, deadline and success; no parameters or result |
| `codex_transport` | Native HTTP/WebSocket request interval and status, plus selected thread startup phases; fixed endpoint category and sanitized thread UUID only |
| `account_admission` | Manager-side account initialization, eligible-account lookup, required quota refresh, serialized selection and preparation; distinct from guest broker token acquisition |
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
Manager MCP HTTP requests without a run-scoped gateway connection are correlated
by endpoint category and the sample's time interval, not by invented run IDs.

Codex batches export asynchronously. `completed_at_ms` comes from the native
event's timestamp (`observedTimeUnixNano` when `timeUnixNano` is zero), and
`request_started_at_ms` subtracts its reported duration at millisecond resolution.
This is the native request timer, not a packet capture or collector arrival.
`websocket_connect` measures the native connection attempt separately. The
loopback trace exporter also retains `model_client.websocket_connection`
and `model_client.stream_responses_websocket`, with the latter classified from
the native boolean `websocket.warmup`. These setup spans end after constructing
the stream; they do not include waiting for the warmup response. The interval
between warmup and generation sends can also contain catalog refreshes and local
work, so it must not be described as network RTT.
`responses_websocket.stream_request` becomes `websocket_response_stream` and
lasts until the native response worker finishes, including peer-response waiting.
It has no warmup flag; correlate its interval with the explicit setup spans and
the sends in the same owned VM/thread. It is not a pure network timer.
The fixed native `startup_prewarm_*` phases also separate turn-context creation,
tool capture (including MCP readiness), prompt construction, full warmup waiting,
and the first turn's wait for background prewarm. These overlap and must not be
summed as sequential work. Other spans and their arbitrary
attributes are discarded without persistence.
Abrupt native termination can discard its final unexported batch; shutdown never
waits for telemetry. A missing transport record is not evidence of no request.
For HTTP model requests, select `endpoint=responses` and the corresponding
thread. Codex 0.159.3 WebSocket send events have no endpoint and also include
its `generate=false` warmup: the first `websocket_request` is not necessarily
an inference request. A successful one-turn, no-tool production probe emitted
two sends (warmup then generation); their classification uses the pinned native
control flow and is an inference, not an explicit phase field in the exporter.
Do not apply that ordering to arbitrary retries or tool loops. Do not substitute
`turn_started` or the first exported batch. A real native HTTP fixture checks
that the interval brackets the local model receipt.

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

## VM import timings

VM imports emit structured records under the existing `cairn_performance` target.
They contain random transfer IDs, VM IDs, fixed phase names and aggregate byte
counts; they do not include paths, archive contents or credentials.

The host's `vm_import` record links its transfer `id` to `vm_id` and a fixed
`import_kind` (`entrypoint`, `home`, `chat_inbox` or `other`). Find that VM's ID in
the run's `{attempt}.vm.json` record. Project imports use `vm_project_import`
with `import_kind="project"` and the same identifiers.
The optional `traceId` in import requests links host and guest records by `id`.

| Operation | What its phases measure |
| --- | --- |
| `guest_prepare` | Keeps `current_entrypoint` separate from `run_connect` and `run_request`. |
| `vm_import` / `vm_project_import` | Guest status, connection, request, host `tar` startup, archive streaming, end marker, host `tar` exit and guest acknowledgement. Project imports also measure the ready/reused reply. |
| `guest_import` / `guest_project_import` | Target preparation, guest `tar` startup, receiving the archive into `tar`, `tar` exit, ownership, permissions where applicable and acknowledgement. Project imports also measure lock acquisition, reopening, publication and `sync`. |
| `vm_archive_stream` | Total archive `bytes`, `chunks`, cumulative `read_ms` and `write_ms`, labelled `side="host"` or `side="guest"`. |

Host stream reads wait on the archive-producing process; host writes wait on the
VM transport. Guest reads wait on that transport (and include Base64 decoding
for legacy JSON transfers); guest writes wait on the archive extractor's stdin.
These are elapsed waits including scheduling, not exclusive CPU measurements.
Host and guest work overlaps, so do not add their durations together. There is
one aggregate stream record per side, rather than one log per chunk.

Phase start records show the last entered step while an operation is blocked.
Cancellation or failure drops the timer and records `event="incomplete"` with
the current phase and elapsed time. Completed stream totals are only available
when streaming reaches its end.

Host timings are in the runner/controller logs; guest timings are in the VM's
`{vm-id}.boot.log` in runner state. Old guest images accept the optional trace
field but do not emit guest timings. The current controller still supplies host
timings for those retained images. New guest code also accepts requests without
a valid UUID trace ID, recording them as `id="legacy"`. No startup timeouts, archive
encoding, import contents or transfer ordering are changed.

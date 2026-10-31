# Measuring agent execution latency

Agent timings use the existing `cairn_performance` tracing target, enabled by the
default binary filter. With a custom filter, include `cairn_performance=info` in
`RUST_LOG`. These records do not change database schema or restart policy.

## Correlation

Worker logs include `run_id`. Every process launch receives a fresh `attempt_id`,
also passed as optional `runId`/`attemptId` fields in the chat plan. The adapter
places native RPC and output logs in the same `agent_attempt` tracing span.
Plans without correlation fields log `unknown`. Invalid correlation values also
become `unknown`.

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
acknowledgement measurements. Host and guest run the same application build.

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
| `database_actor` | Per SQLite actor: admission/queue wait, execution wall time, completed jobs, errors and replies abandoned by their caller |

`database_actor` aggregates locally in each SQLite actor and emits at most once
every 30 seconds, after a job completes. `lane=write` includes both individual
writes and transactions; `lane=read` covers the four reader actors. `actor` is a
fixed index within its lane, starting at zero. Compare the same process lifetime.
The interval can include idle time; an idle or blocked actor does not emit a timer
record. These records describe completed jobs, not current queue depth or work
still blocked. Existing `agent_wait` records cover long output-persistence waits.

`queue_us` measures from submission to the actor beginning the closure, including
bounded-channel admission and scheduling. `execution_us` measures the closure,
including Rust decoding and transaction commit where applicable. Neither is CPU
time. Totals and maxima cover every completed job, including errors and work whose
caller stopped waiting (`abandoned_replies`). Reader execution times can overlap;
do not sum them as elapsed wall time. SQL, parameters, errors and conversation or
account identifiers are never included. Aggregation performs no database write.

`max_queue_caller` names the source function of the job with the longest queue
wait; `max_execution_caller` names the job with the longest execution. They can
be different: the queued caller is not necessarily the source of the blockage.
These hints come from compile-time Rust closure type names, bounded to 256 UTF-8
bytes, and never inspect captured values. Writes and transactions retain the
original closure's name before wrapping it. Generic helpers can share a name,
and compiler names can change between builds; these are not stable operation
identifiers or unique call-site IDs. Maxima reset with each actor's window, and
equal durations retain the first completed job. There is no per-job log.

The same record samples the connection's pager cache at the interval boundary.
`cache_hits`, `cache_misses` and `cache_writes` are interval deltas of counters
read through SQLite's 64-bit connection-status API; `cache_bytes` is the current approximate pager
heap usage, not a delta or configured limit. Startup/migration work before the
actor starts is excluded. Counters are read without resetting them. If sampling
fails or a counter decreases, `cache_available=false` and all four values are
zero for that interval; do not interpret those zeroes as measured inactivity.
The bundled pager stores individual activity counters in 32 bits; an ordinary
rollover appears as a decrease and invalidates that interval.

These counters describe SQLite pages, not query results, the prepared-statement
cache or the OS page cache. A miss may be served from the OS cache without a
physical disk read. Writes count dirty pages written to WAL/database files and
exclude rollback/recovery writes; they are not bytes or physical disk I/O.
Compare them with the actor's execution time and OS CPU/I/O measurements before
changing cache limits or query projections. Sampling changes no SQLite setting,
schema, stored row or subscriber notification.

Measure the four status calls on a synthetic, warm in-memory connection with:

```sh
pnpm test:backend -p cairn-installation --lib store::performance::cache::tests::measure_pager_counter_sampling -- --ignored --nocapture --test-threads=1
```

This reports local wall time for 100,000 samples, not production query latency.
The sampling path runs only at actor startup and the report boundary, never for
every job. It performs no SQL statement or file I/O.

An explicit local metadata-write benchmark exercises the real `Store` on synthetic
0/128/512 KiB frozen snapshots:

```sh
CARGO_BUILD_JOBS=2 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 \
  node scripts/test-backend.mjs -p cairn-installation --test run_write_performance \
  -- --ignored --nocapture --test-threads=1
```

It reports individual warm-operation wall times, process CPU (including SQLite
actors), snapshot bytes, WAL bytes and subscriber revisions for 32 changed or
unchanged boundary/state writes. Setup and WAL truncation are outside timing;
automatic checkpointing is disabled only on this disposable writer to expose
write amplification. This is a warm local metadata workload, without network,
agent processes or storage-object I/O. WAL bytes are not physical disk-write
bytes. Cold reads and object compression are outside its scope. Keep results and
reference/candidate SHAs in the PR description or temporary evidence, not here.

Repeated lifecycle assignments and boundary patches skip unchanged SQL writes,
so they do not wake live subscribers. Explicitly adding a null boundary field
still differs from leaving it absent. Snapshot and durable-wakeup changes remain
independent of lifecycle equality and commit atomically with their transition.
The benchmark's unchanged cases expose both WAL bytes and subscriber revisions;
`database_actor.completed` counts jobs, including these no-op transactions, not
changed rows or physical writes.

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

## Node inventory reads

The `node_inventory` operation measures the inventory shared by the web/Android
node list, public MCP and authorized agent tools. A fresh random operation UUID
correlates its phases; it is not a node, account or conversation identifier.

- `database_queue`: waiting for a SQLite reader, including queue admission.
- `records`: loading attempts, volumes and agent grants in one read snapshot.
- `stale_disks`: checking active executions, moves and checkpoint ownership.
- `describe`: reading nodes and deriving slots, grants and stale-disk totals.
- `disk_accounting`: calculating native storage usage and reclaimable bytes.

Each completion records `phase_ms` and `total_ms`; failures or dropped work record
`incomplete`. These timings include local scheduling and database work. They
exclude the caller's authentication, local-runner refresh, network and UI rendering.
Logs contain fixed phase names and durations, never stored prompts, checkpoints,
paths or request bodies. No diagnostic rows are written to SQLite.

Run the synthetic inventory benchmark with:

```sh
pnpm test:backend -p cairn-installation --test node_inventory_performance -- --ignored --nocapture --test-threads=1
```

It varies saved snapshots and checkpoints across 300 materialized volumes and
checks stale-disk and unsynchronized-byte results. Measurements are local reads
through the shipped inventory function, not production HTTP or disk-transfer
measurements. Keep raw measurements outside tracked files and include the
reference comparison and host conditions in the PR description.

To qualify native storage accounting with retained workspaces, run:

```sh
pnpm test:backend -p cairn-installation --test native_inventory_performance -- --ignored --nocapture --test-threads=1
```

This fixture reports 100 clean retained native caches with acknowledged S3
publication metadata and released execution leases. It checks the actual
`reclaimableBytes` result while varying frozen snapshots and prepared checkpoints.
The ordinary regression also checks active work, pending recovery, moves,
changed checkpoint ownership, dirty bytes, requested publications and clean
queued resumes. No workspace is opened, uploaded or evicted by these tests.

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
VM transport. Guest reads wait on that binary transport; guest writes wait on
the archive extractor's stdin.
These are elapsed waits including scheduling, not exclusive CPU measurements.
Host and guest work overlaps, so do not add their durations together. There is
one aggregate stream record per side, rather than one log per chunk.

Phase start records show the last entered step while an operation is blocked.
Cancellation or failure drops the timer and records `event="incomplete"` with
the current phase and elapsed time. Completed stream totals are only available
when streaming reaches its end.

Host timings are in the runner/controller logs; guest timings are in the VM's
`{vm-id}.boot.log` in runner state. Import trace IDs correlate host and guest
measurements; invalid or absent trace IDs are recorded as `untraced`.

## Conversation list and live reads

Account-scoped conversation lists keep one immutable shared allocation between
refreshes. Reading a cached list and cloning a live state no longer copy every
conversation. A refresh replaces that account's allocation; previous responses
keep their own snapshot. Revision coalescing, external refreshes, account read
marks, the quarter-second rebuild interval and serialized response shapes stay
unchanged. Sharing does not avoid rebuilding a list or serializing a changed
state, and it does not remove the frozen run snapshot from live responses.

The ignored unit benchmark exercises real Store fixtures and the live read path:

```sh
node scripts/test-backend.mjs -p cairn-installation --lib \
  measure_conversation_reads_by_count_and_snapshot_size -- \
  --ignored --nocapture --test-threads=1
```

It covers 1/100/300 conversations and 0/128/512 KiB of frozen agent instructions,
reporting exact stored snapshot bytes, encoded list/state bytes, the first read
through a new private SQLite page cache, 16 warm reads and process CPU. The host
OS cache remains warm; these are local measurements, not network or disk-cold
latencies. Each live-page case includes first-state serialization but excludes
SSE framing and event serialization. Compare the same harness/profile/host on a
reference; do not infer production savings from one trial. Use the existing
`conversation_reads` aggregate to compare page read/queue time, list rebuilds and
coalescing after deployment.

## Client performance report ingress

Beacon emits a process-wide `client_performance_ingress` record every 30 seconds,
including idle windows. Only POST requests to the account performance-report
endpoint are counted. Production applies the observer once, outside the final
host guard, browser security and JSON extraction,
so rejected uploads remain visible even when the report handler never runs.
It does not read request bodies, headers or query strings, and records no account,
window, installation or device identifiers. Counters are fixed-size, in memory,
and reset on process restart; collection performs no database operation.

`started`, `completed` and `abandoned` count handler-future events within the
window. `active` survives window boundaries; `max_active` includes requests
already active at the start of the window. Completions can therefore exceed
starts in a window. `duration_us` and `max_duration_us` include completed and
abandoned handler lifetimes, through response creation, excluding body transfer.

`accepted` means an HTTP 204 response was produced, including stale sequence
retries that do not update a stored window. It is not a count of distinct reports
or database changes, nor proof that the client received the response. Rejections
distinguish invalid input (400/422), unauthenticated (401), forbidden (403),
too large (413), unsupported content type (415), limited
(429), server errors (5xx), and other outcomes. An abandoned future has no HTTP
status and is counted separately. Zero activity establishes that no upload
entered this observer; it does not explain whether a client was closed, offline
or had opted out.

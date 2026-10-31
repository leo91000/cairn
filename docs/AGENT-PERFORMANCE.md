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

The native controller also exposes [bootstrap phases and bounded costs](NATIVE-BOOTSTRAP-OBSERVATION.md)
before systemd receives a workload. Its memory lookup does not inspect the
conversation's volume. Optional native journal export honors `cairn_performance`
at startup, but uses a separate non-blocking transport rather than the agent's
stderr stream. These are technical preparation observations, never proof of a
coding-engine turn or a successful execution.

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

Each actor also reports cumulative costs by those same bounded caller names.
`database_caller` records share the actor's `window` and `interval_ms` with
`database_actor`. Only the first 64 distinct callers in a window are tracked;
the union of the eight highest execution-wall totals and eight highest CPU totals
produces at most 16 caller records. One additional `caller="other"` record accounts
for all remaining jobs, including names that exceeded the tracking capacity.
`caller_recorded_jobs`, `caller_untracked_jobs` and `caller_coverage_complete` make
incomplete tracking explicit. The selected callers are not necessarily the
global leaders when capacity was exceeded. Counts and cumulative queue, execution
and sampled CPU times reconcile with the actor totals; there is no per-job log or
new reporting timer. Counters and caller storage reset at the existing boundary.

On Linux, two thread-CPU clock samples bracket each SQLite closure, including any
transaction commit and excluding queue admission and reply delivery. A disabled
performance target skips the clocks and caller
tracking. `cpu_sampled_jobs` identifies how many completed jobs have valid CPU
evidence; `cpu_available=false` means the reported CPU sum covers only that subset,
not that the remaining jobs used no CPU. Unsupported platforms and failed or
regressed clock samples stay unavailable. These sums exclude actor bookkeeping,
report formatting and other process threads, and are not process CPU utilization.
Times are rounded down to microseconds per job. Compare cumulative CPU and wall
time before distinguishing repeated computation from disk or other waits.
The same per-actor limits apply independently to the writer and four readers:
at most 320 tracked names and 85 caller rows across five report windows. Reader
work can overlap; cumulative wall time is not elapsed process time or CPU time.

These records transfer an owned, bounded window to one diagnostic worker per
process through a 16-entry queue with nonblocking admission. One additional
report may be executing. A full queue or busy admission drops the new report; a permanently blocked
sink cannot grow this queue or block SQLite jobs or maintenance cleanup. Each
report retains its subscriber and emits parentless events with an empty span
stack, creating no diagnostic spans. Text and JSON exclude the caller's private
context, including with layered or event-only filters. No span cleanup can cause
a second logging panic during unwinding. Disabled diagnostics do not start the
worker. SQL, captured values and error contents remain excluded.

`diagnostic_queue_us` describes transport delay, separately from SQLite job
`queue_us`. `diagnostic_dropped_reports_total` is a process-wide cumulative count
of transport rejection/start/disconnection failures and callbacks that unwind;
it does not certify delivery or detect I/O errors swallowed by a subscriber.
These header fields apply to the caller rows sharing that actor/window. A late
report still describes its original completed-job window. Queued diagnostics are
not persisted or replayed across process exit.
The shared enabled precheck catches unwinding filter callbacks and treats that
observation as disabled. SQLite closures still execute and maintenance cleanup
still runs when the diagnostic filter fails.
No query, checkpoint, database setting, durability or scheduling policy changes.

Measure per-job collection overhead separately from application latency on Linux:

```sh
CARGO_BUILD_JOBS=2 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 pnpm test:backend -p cairn-installation --lib store::performance::measurement::measure_writer_cost_sampling_overhead -- --ignored --nocapture --test-threads=1
```

Replace `measure_writer_cost_sampling_overhead` with
`measure_reader_cost_sampling_overhead` to measure the actual reader path.

The test alternates four enabled/disabled pairs on the same candidate, with
20,000 no-op writer jobs per observation after warmup. Initialization is excluded;
the actual queue, closure dispatch and actor teardown are included. It uses
in-memory SQLite and does not force a report boundary, so periodic formatting is
outside this measurement. Disabled diagnostics are the reference, not an older
released binary.

On 2026-10-31, the test profile with these flags, SQLite 3.53.2, Linux
7.2.11-arch1-1 and an i9-14900K host with 32 logical CPUs produced these median
costs in microseconds per job. Each lane retains all eight observations; host
load shortly after these runs was approximately 7.68 / 7.32 / 9.31.

| Lane | Disabled wall / process CPU | Enabled wall / process CPU |
| --- | --- | --- |
| Writer | 7.447 / 9.393 | 6.323 / 8.471 |
| Reader | 9.032 / 11.488 | 7.330 / 9.930 |

An earlier writer-only observation favored disabled collection by 1.187
microseconds wall / 2.952 microseconds CPU per job. The differing signs and
overlapping ranges do not establish a fixed per-job overhead or a gain from
enabling diagnostics. These are local queue/no-op observations with scheduling
variation, not production query latency, application throughput or release gains.

Measure the report producer and completed delivery separately:

```sh
CARGO_BUILD_JOBS=2 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 pnpm test:backend -p cairn-installation --lib store::performance::measurement::measure_isolated_report_overhead -- --ignored --nocapture --test-threads=1
```

Four alternating pairs of 100 forced report boundaries run after warmup.
`producerMicros` measures the actor's sampling and enqueue work. Total wall and
process CPU also include formatting into a sink and a test-only bounded flush
after each report. The flush ensures completed delivery instead of silently
measuring dropped bursts; its cost is present in both modes. This workload uses
one tracked caller, not the maximum 17 caller rows or real log I/O. Production
boundaries remain at least 30 seconds apart per actor and require a completed job;
production code never flushes or joins the worker.

On the host/profile above, all eight observations yielded median producer costs
of 6.445 microseconds enabled and 1.000 disabled per report. Complete wall time
including the test flush was 44.680 / 9.810 microseconds, and process CPU was
48.055 / 12.025 microseconds. These are local observations with one caller and a
sink writer, not guarantees for maximum cardinality or production log delivery.

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
The cache sampling path runs only at actor startup and the report boundary, never
for every job. It performs no SQL statement or file I/O.

The writer's `database_actor` record also samples WAL state at the same report
boundary. Reader records do not sample the WAL. `wal_available=false` means the
snapshot is unavailable or inapplicable; ignore its zero-valued fields. This can
happen for a non-WAL database or a failed inspection and does not prove that
there are no pending pages.

| Field | Meaning when `wal_available=true` |
| --- | --- |
| `wal_frames` | Current frames in the WAL |
| `wal_checkpointed_frames` | Frames in that WAL already checkpointed |
| `wal_pending_frames` | Frames minus checkpointed frames |
| `wal_autocheckpoint_frames` | Current automatic-checkpoint threshold; zero disables automatic checkpoints |
| `sqlite_synchronous` | Current SQLite synchronous mode: 0 OFF, 1 NORMAL, 2 FULL, 3 EXTRA |

These are point-in-time gauges, not interval deltas or counts of work performed
by the sampler. Frame counts can reset when SQLite restarts the WAL. A low
pending count at report time does not rule out an earlier costly checkpoint or
prove that an in-flight operation has finished.

The bundled SQLite `SQLITE_CHECKPOINT_NOOP` inspection checkpoints zero frames;
it does not install a WAL hook, perform a passive/full checkpoint, reset counters
or change any setting. The two PRAGMA statements only read current settings.
No SQL, paths, error text, payloads or user/account/conversation identifiers are
logged. Regression coverage compares pending-frame counts, database/WAL bytes,
changed rows and settings before and after sampling.

Measure the actual snapshot helper on a synthetic warm WAL connection with:

```sh
CARGO_PROFILE_DEV_DEBUG=0 CARGO_INCREMENTAL=0 \
  pnpm test:backend -p cairn-installation --lib \
  store::performance::wal::tests::measure_wal_snapshot_sampling \
  -- --exact --ignored --nocapture --test-threads=1
```

This reports local wall time and mean sampling cost over 10,000 inspections,
including equality assertions, rather than production query latency or a tail
percentile. Sampling remains writer-only at the existing actor report boundary,
never per job. Keep measured results and exact source/profile/host conditions in
the PR description or temporary evidence. Correlate WAL gauges with actor
execution/queue times, cache activity and OS CPU/I/O before changing checkpoint
policy or durability.


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

## Slow SQLite transactions

`operation=database_transaction` describes a completed `Store.transaction`
whose measured time is at least 250 milliseconds. It separates the writer's
`BEGIN IMMEDIATE` (`begin_us`), application closure (`body_us`) and
`Transaction::commit` (`commit_us`). Each phase has a same-thread CPU duration
and an explicit `*_cpu_available` flag; unavailable or unreached CPU samples
are zero with a false flag. These bookends include small timing costs. Wall
time can include filesystem waits, lock contention and scheduler preemption;
the report alone does not identify which of them caused a pause.

`total_us` excludes admission to the actor queue. `other_us` covers time outside
the three measured phases, including change notification after a successful
commit. The outcome is `committed`, `begin_error`, `body_error`, `commit_error`
or `aborted` during unwinding. A body failure's implicit rollback cleanup remains
in its body measurement; a failed commit's cleanup remains in its commit
measurement. `committed` means the SQLite call succeeded, not that an abandoned
caller received its result. Existing actor reports count abandoned replies.

Only the bounded compile-time caller name, fixed labels and numeric timings are
sent to the existing diagnostic worker. There is one report per slow completed
transaction, no SQL, error message, captured value, private span context or
connection on the worker. `diagnostic_queue_us` and the global
`diagnostic_dropped_reports_total` expose delayed or dropped diagnostics.
Worker admission remains nonblocking; a blocked output writer or panicking
delivery callback cannot hold the transaction's result or the next SQLite job.
There is no production flush, shutdown replay or delivery guarantee.

These records do not describe transactions still running, startup migrations,
direct `rusqlite::Transaction` calls outside `Store.transaction`, or implicit
autocommits in `Store.write`. They change no transaction ordering, timeout,
checkpoint, durability, stored value or change-notification policy.

Measure the actual queue and no-op immediate-transaction path on Linux with:

```sh
CARGO_BUILD_JOBS=2 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 pnpm test:backend -p cairn-installation --lib store::performance::measurement::measure_transaction_cost_sampling_overhead -- --ignored --nocapture --test-threads=1
```

The fixture alternates four enabled/disabled pairs after warmup, with 20,000
transactions per observation, in-memory SQLite and a sink subscriber. Queue,
closure dispatch and actor teardown are included; initialization is excluded.
The enabled/disabled comparison also includes existing caller-CPU observation.
To attribute this change's incremental cost, compare the old and new source
with the identical fixture, compiler flags and host conditions. Retain every
observation and separate no-op overhead from real storage or production latency.

On 2026-10-31, four alternating old/new source pairs used the identical fixture
on `ecf323e4` and the phase-timing implementation. Each process produced four
enabled/disabled pairs of 20,000 transactions after warmup: 64 observations in
total, 16 per source/state. Conditions were the test profile, two Cargo jobs,
dev/test debug information and incremental compilation disabled, SQLite 3.53.2,
Linux 7.2.11-arch1-1 and an i9-14900K host with 32 logical CPUs. Median costs in
microseconds per transaction were:

| Source | Disabled wall / process CPU | Enabled wall / process CPU |
| --- | --- | --- |
| Before phase timing | 14.44 / 17.21 | 14.36 / 17.59 |
| With phase timing | 13.88 / 16.53 | 17.44 / 21.06 |

Enabled source-pair wall differences ranged from +1.51 to +5.05 microseconds;
disabled differences changed sign. A second unpinned 64-observation experiment
after static lint cleanup produced enabled medians of 11.35 / 13.81 before and
11.29 / 13.64 after, wall / process CPU respectively. These differing results
do not establish a fixed incremental cost from the unpinned observations.

A third 64-observation experiment verified the first job's CPU sampling mode in
every observation and restricted both processes to the same two logical CPUs
(0 and 2, on distinct physical cores). Other application affinities were not
changed. On the same host/profile, its medians were:

| Source, fixed two-CPU affinity | Disabled wall / process CPU | Enabled wall / process CPU |
| --- | --- | --- |
| Before phase timing | 8.12 / 9.39 | 8.65 / 10.44 |
| With phase timing | 8.27 / 9.50 | 9.73 / 11.48 |

All 192 observations from the three experiments are retained. The controlled
comparison observes about one additional microsecond per empty transaction in
this test profile. It neither measures release-profile overhead nor predicts
production storage latency. These fixtures do not force slow-report delivery,
which is separate from the completed report-delivery measurement above.

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

## Node maintenance preparation

Once background preparation starts, the manager attempts one
`node_maintenance_drain` record on `cairn_performance`, including error, deadline
or dropped preparation.
It aggregates the whole episode without logging individual attempts or retries.
`outcome` is `completed`, `error`, `deadline` or `cancelled`; `phase` is the last
phase entered. Both fields use fixed labels.

`elapsed_ms` and the phase totals use a monotonic clock. The phase totals are
`inventory_ms`, `preparation_ms`, `pause_request_ms`, `capture_ms`,
`capture_wait_ms` and `placement_release_ms`. `capture_ms` includes the existing
archive synchronization/publication call; `capture_wait_ms` includes the existing
100 ms waits after native 425 responses. These are wall times, not CPU times.
Integer millisecond rounding can make the summed phase totals slightly smaller
than the elapsed duration.

Counters distinguish `attempts`, `stale_attempts`, `pause_requests`,
`pause_failures`, `capture_calls`, `capture_waits`, `capture_failures`,
`placement_releases` and `placement_failures`. Requests and release calls are
counted when entered; failures count completed error results. An interrupted
request therefore need not increment its failure counter. Stale checkpoint
ownership is skipped as before. A failed native archive capture retains placement.

The summary captures preparation before the readiness database write. Delivery
is asynchronous and can follow that write. It is not proof that readiness
was persisted, the controller stopped, the node updated, or real work completed.
`cancelled` means the manager's preparation future was dropped; a separately owned
publication task may continue. Abrupt process termination cannot emit a summary.
Concurrent node drains remain separate records without a node/run identifier.
The event contains no identifiers, URLs, error strings or inherited private spans.
One dedicated logging worker uses the captured subscriber with an empty
thread-local span stack and creates no diagnostic spans. Callers submit bounded,
owned observations with a nonblocking queue; no caller waits for sink IO. There
is at most one executing report and sixteen queued reports, even with a blocked
sink. Startup failure, admission contention, full/disconnected queue and callback
panics drop the observation without a caller-context fallback. Normal shutdown
can also lose pending observations. There is no per-job, attempt or retry thread.

`diagnostic_queue_us` measures the report's wait before its callback starts;
`dropped_reports_total` is the process-wide saturating count of known helper
rejections and callback failures at that point, across all report kinds. It does
not detect IO errors swallowed by subscriber formatting or establish successful
log persistence. The shared `performance::enabled()` precheck catches
subscriber/filter panics and disables that observation, so readiness persistence
and task cleanup continue.
The maintenance API, deadlines, retries and ownership rules are unchanged.

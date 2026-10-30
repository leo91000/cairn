# Execution control and interrupted connections

A node has separate paths for presence and lease delivery, serialized controller
configuration, and project archive preloading. A slow budget change, native mode
transition, archive catalogue or preload cannot hold the heartbeat response or
prevent delivery of grants already authorized by the manager. Configuration
requests remain ordered; a lost caller never implies that a mutation stopped.

Native `/health` reads an in-memory diagnostic snapshot. A single background
sampler performs filesystem usage and retained-capacity accounting. Synchronous
filesystem/capability and tool-bundle probes run on the blocking pool, preserving
HTTP and lease responsiveness even with one executor worker. Samples never overlap. The response
always identifies the running controller and reports `telemetryAgeMs`; no sample
or a sample older than 15 seconds reports `degraded`, preventing new admission.
Missing telemetry preserves the last confirmed native mode for existing grants;
new admission still requires a fresh mode report. Existing grants remain subject
to the same run, attempt, account and node checks, including desired generation.
A known controller version mismatch still stops the connector.

Archive downloads, prefetch and workspace seeds never use cached telemetry to
authorize disk writes. While retaining the existing process-safe storage ownership,
they request local `GET /staging-budget` for a current shared-pool allowance. The
controller reads its live budget and filesystem usage without acquiring that same
lock again. Missing/incompatible quota endpoints fail closed; cached health is
never a fallback. This read-only endpoint stays on the root-private socket and is
not admitted by the manager relay. Slow or failed observations log
`operation=native_staging_budget` with duration and status; callers also record
HTTP rejections and timeout/connect failures.

Native membership evidence retains its 60-second validity bound. Heartbeats can
use an unexpired proof while one coalesced refresh runs. Grants based on that
proof expire no later than its original deadline. Missing or expired evidence
requires a successful refresh; revocation and local authorization stay fail closed.
The archive catalogue has its own authenticated node endpoint and recomputes
current authorization before returning descriptors.

Both controllers support opt-in `GET /runs/{attempt}/logs?cursor=1&offset=N`.
Offsets refer to complete NDJSON records in that attempt's retained log. Each
packet carries its next byte offset and event; the client acknowledges it only
after forwarding the complete event. Heartbeats remain possible while a record
is partially appended. An explicit end packet follows retirement and a final EOF
read. Retirement checks treat a busy attempt map as still running and keep
heartbeats flowing. Project imports retain disk ownership through I/O while
releasing the global attempt map before filesystem work. An incomplete record
or a dropped response never counts as clean EOF.
Legacy streams remain available without the query; they cannot safely reconnect.

The client reconnects to the same attempt with the last acknowledged cursor.
The worker retries its idempotent terminal read with durable reservations.
Neither path repeats startup. Consecutive
connection failures are bounded; only sustained valid packet reception starts a new
incident window. Slow headers or a silent body do not reset that budget. A failed channel does not truncate its intact peer or a progressing output backlog.
A transport error alone does not issue DELETE. Explicit cancellation
still stops the attempt. The worker durably observes the terminal result before
fencing and deciding whether a replacement is safe; output and finalization
errors remain distinct from provider completion. A confirmed provider failure is
also durable and cannot become success from controller exit zero or spend the
controller recovery budget after a lost terminal response.

Controller recovery reservations belong to an attempt and are charged once.
Explicit new turns and manual resumes obtain a new recovery budget. Automatic
controller failures retain their ceiling; scheduled provider retries keep their
separate five-attempt/ten-minute budget. Recovery observations honor cancellation,
shutdown and the remaining execution deadline. Final settlement rechecks accepted
cancellation and physical execution identity in the same transaction. Active
failure and restart settlement use the same provider retry policy; a pending
reservation prevents charging the same retry twice. Terminal recovery, delayed
retry expiry and immediate cancellation release both pending and transferred
destination reservations while preserving retained disks and source fencing.
This includes preparation failures after a move transferred its reservation but
before the destination worker claimed it; cleanup still requires confirmed fencing.

## Diagnostics

The default `cairn_performance=info` filter records `operation=node_heartbeat`
every 30 seconds and on heartbeats taking at least one second, with health,
manager and lease-forwarding durations, grant count, telemetry age and readiness.
Warnings identify heartbeat HTTP status or transport timeout/connect failures,
lease refusals and expired grants. Configuration lasting over ten seconds warns
without holding lease delivery. Native sampler failures preserve the last sample
and its age. Output reconnections record cursor and consecutive failure count.
These records contain identifiers and timings, never credentials or agent output.

Use the real connector regression in `node_heartbeat_liveness` for blocked health,
budget and native mode scenarios. Cursor tests cover reconnect boundaries,
partial records and explicit completion. Worker tests cover durable observations,
confirmed completion, cancellation, deadlines and replay ceilings. These synthetic
checks do not establish production recovery; native runtime qualification requires
disposable KVM/ublk infrastructure.

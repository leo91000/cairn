# C5: conversation purge backlog experiment

Requirements: [#267](https://github.com/leo91000/cairn/issues/267), part of #240.
Reference main: `a1a5571116991e595ef77e024a50f597ea002c4a` (F2 #306),
which includes C4 #312 and the wait-card changes in #236.

## Source observation

`conversation_lifecycle.rs::cleanup_conversations` reads all conversations,
sorts by retry time and attempts at most one eligible purge. The worker's
`clean_conversations` uses a 15-second interval with missed ticks skipped;
deletions and retention changes also notify it. With no notifications and
fast successful purges, 100 passes span 1,485 seconds between first and last
starts, and 1,000 span 14,985 seconds (4 h 9 min 45 s). Initial phase can add
up to 15 seconds, and the last purge takes additional time. These are
cadence calculations, not measured production completion times or guaranteed
lower bounds: notifications can shorten them, and slow I/O can lengthen them.

The process file lock and in-process conversation storage lock cover each
pass. Preparation and active-worker guards protect run setup/finalization;
the writer transaction rechecks lifecycle, activity, policy and pending work
before claiming. Failure records a durable error and a one-minute retry delay.
Purge performs disk deletion and local filesystem operations outside SQLite
transactions, then forgets the remaining records in a transaction. It can
wait on disk requests for up to 7,200 seconds per request. S3 reclamation is
queued durably; forgetting the conversation does not prove remote erasure.
See ADR-0041 and [the retention contract](CONVERSATION-RETENTION.md).

## Reproduction and workload

Use the pinned tools and the isolated backend launcher. Compile serially with
other heavy builds stopped. Use a task-owned persistent temporary directory
whose actual filesystem has more than 11 GiB free after compilation (10 GiB
production reserve plus 1 GiB experiment margin). The benchmark checks this;
the check does not change production admission. On the measurement VM `/tmp`
is a 9.3 GiB tmpfs and cannot satisfy it.

```sh
mkdir -p /path/on/persistent/disk/c5-fixtures
# Record ownership before use. Do not point this at an existing data directory.
printf '%s\n' 'C5 synthetic fixtures only' > /path/on/persistent/disk/c5-fixtures/OWNER
CAIRN_PURGE_MEASUREMENT=1000-chain \
CARGO_BUILD_JOBS=1 TMPDIR=/path/on/persistent/disk/c5-fixtures \
  timeout --signal=TERM --kill-after=30s 15m \
  mise exec -- pnpm test:backend -p cairn-installation \
  --test conversation_purge_measurements measure_purge_backlog \
  -- --ignored --nocapture --test-threads=1
```

The ignored integration test constructs fresh `Service`/`Store` instances with
the worker disabled. Each backlog has 100 or 1,000 immediately due, terminal
conversations. Each has a finished run, sixteen 1 KiB text events, eight 512-byte
messages, one private artifact and a completed checkpoint. Local run and attachment
directories contain two 4 KiB files each, plus one 4 KiB artifact: 20 KiB per
conversation. UUIDs and assigned slugs vary; content and shape are synthetic.
No project, account, disk grant, publication, S3 endpoint or remote node is seeded.
This is a plausible small-history local workload, **not** an observed production
distribution or a demonstrated maximum realistic backlog. Large histories,
worktrees, many shared blocks and unavailable nodes require separate qualification.
This already-trashed workload also excludes the extra claim/revocation work of
expiring active conversations under the inactivity policy.

Choose one explicit case per invocation: `100-single`, `100-chain`,
`1000-single` or `1000-chain`. Each case runs once. Run the cases serially;
changing case selection does not change a production setting. The two modes are:

* `single-pass`: call the existing one-conversation pass repeatedly.
* `chain-250ms`: repeat those identical passes until the experimental 250 ms
  budget is reached, then start another burst. Locks are released/reacquired by
  each ordinary pass. This is an external test driver, not a production batch.

An initial twelve-trial pilot completed two 100-conversation rows (56.4 s and
26.3 s active drain) before it was deliberately stopped during its first
1,000-conversation trial. Its log and rows are retained as pilot evidence, not
a successful full matrix. The final driver selects individual cases to bound
each invocation to 15 minutes without reducing backlog size or removal assertions.
That external command budget is not a purge deadline or a performance assertion;
a timed-out command is not a successful trial.
The final results use the rebuilt individual-case driver; a single trial per
case is descriptive and cannot establish stable production percentiles.

Neither mode sleeps for the worker's 15 seconds. Measured elapsed time is active
drain time; `cadenceOnlyFirstToLastStartMs` separately models 15 seconds between
burst starts, assuming each burst finishes before the next tick and no notifications.
250 ms is an experiment parameter, not a user-approved latency objective or a
hard time bound: an individual purge can overrun it. This repeated-pass alternative
also retains the full list scan on every conversation; it does not model a
single-scan batch or holding storage locks across several conversations.

Before each drain, twenty unchanged chat writes and warm `Store` reads are
measured. Fresh-connection reads time a direct SQL query after opening a new
connection; they exclude connection construction and actor dispatch. They are
not directly interchangeable with `Store` timings and leave the OS page cache
warm. JSON/payload byte totals and file bytes describe the unchanged storage
representation; there is no new envelope or compression policy. Payload totals
count UTF-8 values in the seeded records, not SQLite pages, indexes, WAL bytes
or filesystem allocation overhead. The file total counts logical contents.

A concurrent probe updates one synthetic KV counter, waits 1 ms and repeats.
A 100-probe no-purge reference precedes each drain. Queue delay is measured from
dispatch until its writer closure starts; write time covers `Db::set` inside
that closure. Scheduling and shared-host contention contribute to queue delay.
The writer's `CLOCK_THREAD_CPUTIME_ID` delta includes purge and probe work;
process CPU includes all test threads. Neither is writer **wall-time occupancy**.
These measurements expose CPU demand and observed interference without claiming
exact transaction duty cycle or worst-case writer starvation. Probe intervals
are not guaranteed under load. Setup, read/write reference and assertion/cleanup
time are outside the drain interval; completion sampling adds a final writer job.

Every trial asserts that chats, runs, messages, events, artifact metadata,
checkpoints and all seeded local files were removed. Existing lifecycle and
storage-lock regression tests retain the active-work, policy/recovery and
locking contracts. All raw per-pass, per-burst and probe samples are emitted in
`C5_PURGE` JSON, including the twenty read/write reference samples.
`passesMs` includes locking, selection/list scanning, the claim and deletion;
it is not an isolated timer around `conversation_deletion::purge`. Each successful
trial completes exactly one conversation per recorded pass.

## Recorded local results and decision

Measured on 2026-10-19, 15:40–15:53 UTC, on Linux 6.12.109 x86-64 KVM,
seven Intel Xeon 2.80 GHz vCPUs, 18.56 GiB RAM and an approximately 32 GiB overlay filesystem.
The persistent fixture filesystem had about 18.94 GiB available after compilation.
Rust/Cargo 1.97.1, Node 24.21.0 and pnpm 12.3.4 were pinned; the ordinary
unoptimized/debuginfo test profile was used. Heavy compilation had ended, and
no other benchmark or browser/device workload ran concurrently. No exclusive
reservation or host/profile tuning was used. The one-minute load before cases
ranged from 0.89 to 1.39; host/OS caches remained warm from setup and prior work.
Per-case before/after host snapshots retain the actual available space, memory,
load and source hash. This is a local VM observation, not production/WAN latency.

The final order was `1000-chain`, `1000-single`, `100-chain`, `100-single`,
one complete trial per case. All four commands exited 0 and asserted complete
removal of the seeded records and files. The measured harness SHA256 is
`e7361910f08471e393abeba5983ecc92bda2d3bccb3e2d1bb94418d35ac82f9e`.
The pilot source, its original hash and a byte-identical timed-helper prefix
are preserved separately; only its outer driver differs. The pilot's incomplete
1,000-conversation trial is excluded. Raw final samples, pilot logs, commands,
host snapshots and source provenance accompany the private PR handoff.

| Backlog | Driver | Active drain s | Process CPU s | Writer CPU s | Pass p50 / p95 ms | Queue p95 / max ms | Bursts | Cadence-only s |
| ---: | --- | ---: | ---: | ---: | --- | --- | ---: | ---: |
| 100 | single-pass | 16.783 | 3.985 | 1.464 | 124.172 / 408.281 | 57.082 / 414.241 | 100 | 1485 |
| 100 | chain-250ms | 16.901 | 5.163 | 1.899 | 112.338 / 385.071 | 67.324 / 681.950 | 42 | 615 |
| 1000 | single-pass | 239.978 | 90.375 | 20.856 | 176.377 / 604.453 | 66.080 / 941.653 | 1000 | 14985 |
| 1000 | chain-250ms | 285.988 | 89.739 | 22.571 | 198.448 / 711.093 | 84.256 / 1607.802 | 607 | 9090 |

One completed trial per case; p50/p95 describe observations within that trial, not production percentiles.

| Case | Payload bytes | File bytes | Warm Store read p50 ms | Fresh-connection SQL read p50 ms | Store write p50 ms | Baseline queue p95 ms | Burst max ms |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 100-single | 2209768 | 2048000 | 0.767 | 1.644 | 66.400 | 1.260 | 1033.228 |
| 100-chain | 2209755 | 2048000 | 0.678 | 1.054 | 16.755 | 0.849 | 1151.315 |
| 1000-single | 22097568 | 20480000 | 0.822 | 1.317 | 44.229 | 1.798 | 2310.083 |
| 1000-chain | 22097583 | 20480000 | 0.698 | 1.091 | 37.561 | 1.845 | 9625.107 |

The 1,000-conversation active drains took 240.0 s and 286.0 s, with about
90 s of process CPU in both. These separate single observations do not show a
reduction in active work or establish a causal timing difference. Both drivers
still perform 1,000 full-list cleanup passes. The earlier 100-conversation pilot
also varied substantially (56.4 s and 26.3 s versus 16.8 s and 16.9 s in the
final cases), reinforcing the shared-host/cache limitations.

The scheduling arithmetic is distinct: the 1,000 single passes span 14,985 s
between first and last starts at a quiet 15-second cadence; the prototype's
607 groups would span 9,090 s (2 h 31 min 30 s) under the same assumption.
All observed groups finished in less than 15 s, but no 15-second waits were
actually executed. Initial phase and final completion time are additional;
notifications, remote delays and missed ticks change real worker behavior.

The 250 ms parameter is demonstrably soft: the largest group took 9.625 s,
because the check occurs only after a pass finishes. Writer probe queue p95
rose from 1.80 ms before the reference drain to 66.1 ms during it, and from
1.85 ms to 84.3 ms in the chained case; maxima were 942 ms and 1,608 ms.
Those are observed queue/scheduling delays during saturated local drain with
its probe, not worst-case production writer latencies or exact writer wall
occupancy. CPU totals include probe work and do not account for actor I/O waits.

**Decision:** deliver the measured prototype and retain production scheduling.
The experiment establishes a cadence cost and a possible scheduling gain, but
neither removes the scan/deletion work nor justifies a production value of X.
It cannot make an individual purge time-bounded. A new default or broader batch
redesign is not justified by this workload alone without an accepted maximum
backlog, completion objective and writer-interference budget, plus representative
node/S3 and large-history costs. No new batching abstraction or cancellation
protocol is introduced.

This does **not** endorse four hours as an acceptable backlog duration or claim
the audit's acceptance stop criterion was met. Reopen implementation once those
operational objectives and representative costs are available; compare repeated
passes against a single-scan batch while retaining per-conversation claims,
locks, durable retries and cancellation/recovery safety. The current change
finishes the discriminating experiment and records that policy decision honestly.

## Compatibility and scope

The experiment changes no production behavior, UI, persisted format, schema,
retry state or worker cadence. Epoch 5 and legacy tree
`a6aae371f1d76cceb2f510180531f0ef17ec3e60` remain unchanged under
[the production compatibility contract](PRODUCTION-CAIRN.md#installation-data-compatibility).
No Claude UI task or browser/device qualification applies to this backend-only
measurement harness and report. No production data or remote latency is measured.

## Validation limits recorded during the experiment

The ordinary `pnpm check` invocation passed lint but failed JavaScript tests:
571 passed, 21 failed in six files (68 files total). Consequently its chained
production build did not run. A separate `pnpm build` passed typechecking and
the Vite production build. The full check is **not green**.

One diagnostic rerun selected only the recorded failed case names from those
six files, with `--maxWorkers=1`, no active compiler, and unchanged test/hook
timeouts and assertions. It passed 19 cases and still failed three; 53 cases
were skipped (one selected parameterized name also matches its second variant).
The remaining failures are:

* Historical Node `codex-accounts` suite (since removed): “waits on exhaustion when all accounts
  are unavailable and remains cancellable” (`toMatch` receives a non-string
  while polling).
* Historical Node `restart` suite (since removed): “recovers after a real manager process receives
  SIGTERM” (the expected `preserved before restart` file content stays empty).
* Historical Node `restart` suite (since removed): “preserves an incomplete workspace preparation
  and uses a new generation” (the run remains `running` instead of `succeeded`).

These tests and their TypeScript legacy fixture inputs are unchanged by this
diff and do not import the Rust measurement harness. The isolated reproduction
does not establish the root cause or a green baseline, and the initial failures
must not be erased by the partial rerun. No further blind rerun, increased
deadline or unrelated source correction is part of C5. Raw logs and the exact
rerun selection accompany the private handoff; remote quality remains a
coordinator gate. The initial library-only `cargo check --locked -p
cairn-installation --lib` passed with the pinned Rust toolchain and normal
unoptimized/debuginfo profile; it is not validation of the new integration test.

The first integration-test compilation also failed in four newly added queries:
`rusqlite` has no `FromSql` implementation for `u64`. Reading SQLite counts
and byte sums as `i64` fixed those errors. The targeted rebuild passed in
1 min 17 s, and the individual-case driver then rebuilt successfully in 40.23 s.
Both the original compiler diagnostics and successful rebuild logs are retained.
The interrupted pilot's exit 101 records intentional SIGTERM, not a successful
full benchmark or a latency assertion failure.

Final individual-case measurements passed all four selected cases. Targeted
backend regression validation through the isolated launcher passed 20 lifecycle
tests (the pre-existing retention benchmark remained ignored) and the one
storage-lock test, with `--test-threads=1` and persistent per-command `TMPDIR`.
The full backend workspace suite was not run locally; the delivery retains
that coverage limit and the remote quality/integration gates.

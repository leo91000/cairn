# Durable mission waits

Missions and conversations share `waits`: input validation, idempotency, run dependency
cycle detection, authorization and plugin observation. The owner stores the wakeup
on the conversation or the mission run. Existing conversation behavior is unchanged.

```text
running --successful turn with wakeup--> waiting
waiting --time/result/timeout--> queued --> running (same session and workspace)
running/waiting --stop--> cancelled
running --terminal failure--> failed (wait cleared)
```

`waiting` holds the task's unique active slot but no worker process/account slot.
The worker consumes a wakeup only after its preceding execution has finished
release and fencing. Restart recovery preserves it and finishes any outstanding
execution cleanup before the wakeup deadline, including when the wait is cancelled.
Consumption and checkpoint
reset commit together; the remaining execution budget is retained. A unique
`missionTurnId` fences MCP grants, artifact/outcome writes and exclusivity against
the previous turn, and identifies a new provider turn instead of replaying its
completed predecessor. Wakeup observations are data, never additional authority.

The run APIs and both installation tool interfaces expose the stored `wakeup`
and `waiting` state. Existing cancellation handles the wait. Web/Android show a
live wait card and stop action. Mission schedule pause affects future runs only.
Mission waits and explicit node moves exclude each other transactionally. Finish
the current move before registering a wait, or finish the wait before requesting
a move; a rejected move releases its provisional destination reservation. Recovery
may still relocate an interrupted, unfinished turn that registered a wait before
it stopped; this preserves that wait until the turn completes successfully.

## Local storage measurement

Reproduce with `python3 scripts/measure-mission-waits.py`. The reference models the
base revision `8bfa03d6b0f342700c2598bc9d3740a4d8f5dbee`: its active index includes
queued/running, and the equivalent pending records are queued without mission
wakeups. The candidate includes waiting in the index and stores one small timer
wakeup on each of those records. This isolates the SQLite/JSON storage cost; it
is not an end-to-end service or production network benchmark.

Measured 2026-10-22, Linux 6.12.109 x86_64/glibc 2.36, Python SQLite 3.53.1,
12 visible CPUs, shared agent VM with concurrent compile/test work, local overlay
filesystem, no network. 10,000 synthetic runs, 1,000 active; 50 reads per mode,
300 committed changing JSON writes; WAL and synchronous FULL. Each read fetches,
orders and decodes all 1,000 active documents. “Cold” means a new connection with
cold SQLite pager/statement caches; the OS page cache remains warm. No kernel
cache drop was performed. Timings vary under this shared workload.

| Metric | Reference | Candidate |
| --- | ---: | ---: |
| Active document JSON bytes | 132 | 330 |
| Total encoded JSON bytes | 1,433,670 | 1,631,670 |
| SQLite database bytes after checkpoint | 2,310,144 | 2,486,272 |
| Cold pager read, p50 ms | 2.1203 | 3.0925 |
| Cold pager read, p95 ms | 4.025 | 5.6741 |
| Cold pager read, mean CPU ms | 2.5946 | 3.4911 |
| Warm read, p50 ms | 1.6089 | 2.9935 |
| Warm read, p95 ms | 2.765 | 3.2446 |
| Warm read, mean CPU ms | 1.8598 | 3.047 |
| Write + commit, p50 ms | 0.0282 | 0.0089 |
| Write + commit, p95 ms | 0.0345 | 0.0099 |
| Write + commit, mean CPU ms | 0.0302 | 0.0094 |

The timer adds 198 encoded bytes in this corpus; reading and decoding the added
JSON costs additional CPU. Write timing differences under this host's cache and
concurrent load do not establish an improvement. Plugin observations can be
larger (bounded by the existing 65,536-byte observation limit); the timer corpus
is not a bound for them. No block/object compression or storage codec changes.
Compatibility and failure recovery follow [automatic migrations](AUTOMATIC-MIGRATIONS.md).

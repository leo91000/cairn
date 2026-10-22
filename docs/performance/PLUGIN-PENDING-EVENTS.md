# Pending plugin event storage observation

Local synthetic Store observation on 2026-10-22. Both cases contain the same50
queued-event keys and payloads, representing one instance's maximum event queue.
The reference omits the new optional `startsWork` hint; the candidate adds true.
It adds18 serialized JSON bytes per opted-in event (900bytes for this workload).
Non-opted-in records keep their previous shape. Both shape workloads retain all50
rows; this measurement does not implement or compare full scheduler execution.

| Shape, 50 rows | JSON bytes | First read ms | Median 5 warm reads ms | Median 6 writes ms | Warm read process CPU ms | Write process CPU ms |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| prior event shape | 23,750 | 1.338 | 0.088 | 0.312 | 0.093 | 0.333 |
| optional startsWork=true | 24,650 | 0.802 | 0.066 | 0.258 | 0.069 | 0.281 |

Each case seeds a separate real Store, closes/reopens it, then executes one
connection-cold and five warm selected-row reads on the same read actor/connection.
OS page caches remain warm. Each of six writes changes the existing13-digit
`nextAttemptAt` value, including local actor scheduling. Read times exclude that
scheduling. JSON bytes exclude keys' SQLite/index/page/WAL allocation. CPU is
process CPU time. No network, Slack, authority scan, admission, complete handler,
whole-scheduler, VM or production storage latency is measured; sequential order,
few samples and host/cache noise prevent any speedup claim.

The test ran alone with one test thread and no concurrent local build/test/manager
job, unoptimized Rust with debug symbols/incremental off and two Cargo build jobs.
The host exposed7 CPUs, Linux6.12.109 and tmpfs for temporary databases. CPU/memory
cgroup quota files were unavailable; no production capacity is inferred. Raw
wall/CPU samples, workload, command, host data and relevant source hashes are in
[the adjacent JSON](PLUGIN-PENDING-EVENTS.local.json).

```sh
mise exec -- pnpm test:backend -p cairn-installation --test slack \
  capacity::measure_pending_event_storage -- --ignored --exact --nocapture --test-threads=1
```

Retention correctness is separately exercised through the actual native manager
and Luau package: the reference drops an ACKed event at its fifth capacity error;
the corrected scheduler retains it through six forced due wakeups, then the actual
adapter/provider process produces its answer after synthetic occupied slots are
released. Busy ingress does not create a duplicate marker. Already submitted
receipts and Slack/Discord controls remain usable under the20-pending-work ceiling.
These boundary fixtures do not seed a completed answer. The waiting event's
existing source/requester/approval proofs still apply; deferral does not authorize
work, reserve capacity or recreate a removed/changed admission. Ordinary handler
failures keep their finite error budget. The optional-field compatibility decision
is recorded in [the inventory](../SLACK-PLUGIN-COMPATIBILITY.md).

# Native continuation delivery overhead

Local observation on 2026-10-22. Reproduce with:

```sh
python3 scripts/bench-continuation-delivery.py > continuation-delivery.json
```

The [raw observation](slack-continuation-delivery.json) records the workload,
host, SQLite pragmas, CPU and samples. Both cases use synthetic, non-secret source
bindings. The reference retains ten source deliveries, without forwarding their
continuations. The candidate reserves ten additional deliveries while those
source turns remain active: twenty total, the unchanged per-instance ceiling.
Once original replies settle, their source rows retire normally. No age-based
deletion silently removes a pending accepted reply.

Each case registers/replaces 100 waits, then performs 1,000 warm pending-list reads
and 50 reads using newly opened SQLite connections. Reference writes update the
chat and append its wait event. Candidate writes also replace its reserved row
and update the activity link in the same immediate transaction. JSON encoding
and SQLite commits are timed; fixture preparation is excluded from write latency.
The candidate's `continuationOf` adds 56 JSON bytes to each reserved delivery.
An acknowledged wakeup's separate event payload is 52 bytes; its execution,
insertion and subsequent history growth are not part of these pending-wait timings.

| Metric | Reference | Candidate |
| --- | ---: | ---: |
| Pending delivery JSON bytes | 9,410 | 19,380 |
| All record JSON bytes | 13,160 | 23,130 |
| SQLite bytes after checkpoint | 69,632 | 86,016 |
| Registration/replacement write p50 / p95, ms | 0.111 / 0.157 | 0.463 / 0.807 |
| Warm pending-list read p50 / p95, ms | 0.176 / 0.279 | 0.537 / 1.143 |
| Fresh-connection list p50 / p95, ms | 0.547 / 0.829 | 1.664 / 2.148 |
| Write process CPU total, ms | 13.785 | 48.185 |
| Warm-read process CPU total, ms | 188.322 | 622.118 |
| Fresh-connection read CPU total, ms | 28.856 | 84.739 |

Host: Linux 6.12.109, Python 3.14.7, SQLite 3.53.1, approximately 19 GiB reported
memory with 18 GiB available. Files were on `/tmp` tmpfs, with WAL, 4 KiB pages
and synchronous FULL. No local compiler, test suite, emulator or PostgreSQL ran
during sampling. The shared host is uncontrolled; raw load averages are included.
The reference ran first. New-connection timing includes opening/closing SQLite;
OS caches were retained. This is connection-cold evidence, not cold physical I/O.

This is a Python JSON/SQLite shape benchmark, not Cairn's Rust serializer, database
actors, authorization, scheduler, full native wait API or production latency.
It measures neither provider calls nor model/VM/S3 restoration. Higher resident
record counts explain a repeatable size cost; these local timings promise no
production response time. Actual-worker recovery and the capacity/replacement
race are tested through the native implementation separately. The whole-PR
[compatibility](../SLACK-PLUGIN-COMPATIBILITY.md) and final qualification gates remain open.

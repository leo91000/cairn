# Reply checkpoint storage observation

Local, synthetic observation on 2026-10-21. The storage change replaces permanent
per-reply plugin keys with a separate native record owned by a pending delivery.
The authority/lifecycle regression and actual-worker Slack journeys separately
verify retirement; these timings measure only the selected record shapes.

The reference seeds 1,000 historical receipts kept by the previous Slack code.
The candidate seeds 50 concurrently pending checkpoints, then deletes those
selected checkpoint records. Fifty is this workload, not a native queue limit.
The reference intentionally retains its historical records. The two datasets
illustrate retention behavior; they are not equivalent workloads or a throughput
A/B, and the results do not claim a speedup or a production storage saving.

| Record shape | Seed rows | Encoded JSON bytes | Cold selected read, ms | Median 5 warm reads, ms | Median 6 replacements, ms | Rows retained after cleanup |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| permanent-plugin-receipts | 1000 | 300000 | 1.083 | 0.073 | 0.230 | 1000 |
| pending-delivery-checkpoints | 50 | 11100 | 0.732 | 0.045 | 0.194 | 0 |

Encoded bytes are the sum of serialized record JSON, excluding SQLite/WAL/index
allocation. Each selected read returns one record (300 reference bytes or 222
checkpoint bytes). A fresh `Store` follows seeding; one read-actor closure runs
all six reads on the same connection. Sample 0 has cold reader/statement caches,
then five reads use that connection's warmed cache. OS caches are retained; these
are not machine-cold disk reads. Per-query read times exclude actor scheduling.
Writes use actual `Store.put` with the same value six times and include its local
actor scheduling. Selected cleanup excludes the complete `work::finish` activity
scan, authorization and any provider/network operation. Per-sample process CPU,
seed/cleanup timings and all raw samples are in the adjacent JSON file.

The test ran alone in its process with one test thread and no concurrent local
build/test job. Rust profiles had debug symbols and incremental compilation off;
Cargo used one build job. The process reported seven available CPUs. CPU and
memory cgroup quota files were unavailable in this environment, so no quota or
production capacity is inferred. None of these measurements establishes HTTP,
Slack, VM, S3 or complete delivery latency.

Reproduce with the pinned repository environment and isolated backend launcher:

```sh
mise exec -- pnpm test:backend -p cairn-installation --test slack \
  checkpoints::measure_reply_checkpoint_storage -- \
  --ignored --exact --nocapture --test-threads=1
```

[Raw local samples and source hashes](PLUGIN-DELIVERY-CHECKPOINTS.local.json).

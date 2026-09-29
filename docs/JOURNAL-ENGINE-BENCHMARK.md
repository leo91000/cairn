# Local journal engine and reclamation benchmark

Date: 2026-09-29. Application baseline: `eb4a701d644eb218e686464fca198c2b3b8d741c`, release v0.46.2.

This is a synthetic, optimized storage benchmark. It does not change application code, stop a conversation, run S3 transfers, or deploy a fix. Subsequent application implementation is documented separately. Findings apply to the measured SQL/file operations; this is not a full guest VM workload or a production latency guarantee.

## Method

- Large fixture: 6 GiB of published-generation payloads in 4 MiB rows, plus **1 GiB of live unpublished payloads** and a 4 KiB live marker. The live working set prevents the misleading “vacuum an almost empty database” shortcut.
- Small fixture: 64 MiB obsolete + 32 MiB live, with the same schema and operations.
- Same logical payloads for every engine. The SQLite full/incremental files include pointer maps. The Turso file has no SQLite auto-vacuum and uses explicit application-managed primary keys, because the existing `AUTOINCREMENT` schema failed under MVCC.
- WAL and `synchronous=FULL` remain enabled. Turso MVCC uses its durable logical log instead of SQLite WAL for transactions. No durability setting is relaxed for timed foreground writes.
- One independent 100 Hz reader and one 100 Hz 4 KiB durable writer. Samples include lock acquisition and retries. Missed 10 ms arrival slots are counted rather than generating a catch-up burst. A one-second warm-up and one-second tail bracket acknowledgement and cleanup.
- Acknowledgement drains publication readers and durably updates the base, receipt, and obsolete-generation cutoff. Deletion follows either inside that transaction (baseline/simple incremental) or in maintenance batches.
- Bounded deletion uses the sealed sequence boundary and removes two 4 MiB rows per transaction, then releases the journal mutex. It does **not** repeatedly scan all BLOBs to discover the batch.
- Adaptive SQLite reclamation starts at 1,024 pages per call, halves above a 5 ms observed call, doubles below 1 ms, and is bounded to 64–8,192 pages. The aggressive comparison fixes 8,192 pages. Page bounds are not a wall-clock guarantee.
- Cleanup has a 180-second budget checked between calls. Incomplete reclamation is explicitly reported, never counted as “space recovered.”
- Copied fixture files and their directory are **fsynced before measurement**. OS cache is not globally dropped; the workload is relatively warm. Physical disk bytes, logical file size, CPU time, I/O counters, RSS, retries, errors, percentiles, and maximum latency are recorded separately.
- Verification checks the durable base/receipt cutoff, newer 4 KiB marker, a newer 4 MiB payload, every acknowledged foreground payload and contiguous sequence, and `quick_check`. Reopening is a separate process. Crash tests use SIGKILL before transaction commit, after durable acknowledgement, and during cleanup, with an external stdout ledger of acknowledged foreground writes.

## Variants

1. `sqlite-full`: existing strategy, BLOB deletion + automatic full reclamation during acknowledgement, then a truncate checkpoint under the publication lock.
2. `sqlite-incremental`: bulk BLOB deletion still in acknowledgement, physical reclamation deferred.
3. `sqlite-batched`: only the base/receipt/cutoff is committed during acknowledgement; old rows and physical pages reclaimed later in bounded batches.
4. `sqlite-batched-wide`: same protocol, deliberately larger vacuum calls to measure the latency/throughput tradeoff.
5. `segments`: SQLite metadata + generation-owned 64 MiB payload files. Every foreground write fsyncs payload and then commits SQLite metadata. Cleanup retires old metadata and unlinks obsolete generation files.
6. `segments-log`: same generation files, plus self-describing, CRC-checked append frames for foreground writes. A durable append updates a memory index; the foreground SQLite index can be rebuilt from frames. This removes the second foreground fsync. The receipt database remains FULL. Writes are read back through the memory index, and recovery reconstructs SQLite locations before checking all payloads.
7. `turso-wal`: new cutoff/deferred deletion protocol, independent foreground/maintenance connections.
8. `turso-mvcc`: `BEGIN CONCURRENT`, default checkpoint behavior.
9. `turso-mvcc-passive`: same with the experimental passive checkpoint feature enabled.
10. `sqlite-split-vacuum`: independent SQLite writer/reader/maintenance connections; two-row deletion batches then full VACUUM, giving SQLite the same connection separation as Turso.

Turso maintenance attempts space reclamation with VACUUM because incremental vacuum is unavailable. If concurrent VACUUM refuses due to outstanding MVCC changes, that refusal and retained space are reported. A separate quiescent truncate-checkpoint + VACUUM is timed after the foreground has stopped; it is **not** disguised as concurrent cleanup or included in the favorable foreground-latency figure.

## Environment and artifacts

- Intel Core i9-14900K, 32 logical CPUs, approximately 94 GiB RAM.
- Local `/dev/nvme1n1p2`, ext4, approximately 733 GiB initially available.
- Optimized Rust release build, cargo/rustc nightly 1.99.0-nightly (2026-07-17).
- rusqlite 0.40.0 / bundled SQLite 3.53.2.
- Turso binding 0.8.0, resolved engine/SDK 0.8.1; defaults `mimalloc` and `fts` disabled for this standalone experiment.
- Measured benchmark binary SHA-256: `8f499dbc06178f0b55a22179498f735e363f24dcd72f328401dfbb824ccf6998`.
- `bench.rs`: standalone cargo nightly script, `run.mjs`: fixture preparation, sampling and crash orchestration; `analyze.mjs`: aggregation.
- `logs/*.jsonl`: phases and externally observed durable write ledger; `*.resources.json`: 100 ms process samples; `*.outcome.json`: exit status and events; each run has `latencies.csv`.
- `smoke-final.log`, `large-final.log`, `crash-final.log` identify the final runs. Earlier exploratory logs are retained but **excluded from final comparisons**: they include initial harness API errors, incomplete stepping of an SQLite pragma, repeated full-table discovery scans, and fixture copy writeback charged to maintenance.

## Compatibility observations

The original SQLite fixture with `AUTOINCREMENT` produced Turso MVCC errors:

```
Internal error: missing backing table for sequence "__turso_internal_autoincrement_writes"
```

The performance comparison therefore uses `INTEGER PRIMARY KEY` with explicit sequence allocation for Turso. This is an adaptation, not evidence that the existing journal can be migrated unchanged.

Turso MVCC space reclamation can report:

```
Transaction error: cannot VACUUM an MVCC database with uncheckpointed changes; run PRAGMA wal_checkpoint(TRUNCATE) first
```

Passive checkpoints require the explicitly enabled experimental feature. The baseline MVCC comparison uses FULL checkpoints instead. SQLite's `incremental_vacuum` yields rows; the benchmark drains the statement fully to execute the requested page budget.

## Reproduction

```sh
CARGO_TARGET_DIR=/var/tmp/leo-journal-engine-bench/target cargo +nightly -Zscript build --release --manifest-path /var/tmp/leo-journal-engine-bench/bench.rs
node /var/tmp/leo-journal-engine-bench/run.mjs smoke
node /var/tmp/leo-journal-engine-bench/run.mjs large
node /var/tmp/leo-journal-engine-bench/run.mjs crash
node /var/tmp/leo-journal-engine-bench/analyze.mjs smoke-final.log large-final.log
```

Optional mode arguments select a subset, e.g. `large segments-log turso-mvcc-passive`. The large fixture takes about 21 GiB; each run starts from a new copy. Existing runs and source seeds are not overwritten.

## Sources

- SQLite vacuum pragmas: https://www.sqlite.org/pragma.html#pragma_auto_vacuum
- Turso 0.7 CPU yielding and passive checkpoints: https://turso.tech/blog/turso-0.7.0
- Turso 0.8 release benchmarks: https://turso.tech/blog/turso-0.8.0
- Turso compatibility matrix: https://github.com/tursodatabase/turso/blob/main/COMPAT.md

## Results

| Run | Ack ms | Cleanup s | Read p99 / max ms | Write p99 / max ms | CPU cores | RSS MiB | Disk writes MiB | Remaining MiB | Reopen |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---|
| large-sqlite-full-1790711223045 | 18628.92 | 0.00 | 8.12 / 18618.98 | 21.72 / 18637.59 | 0.57 | 53.91 | 8075.22 | 1038.87 | true |
| large-sqlite-incremental-1790711269011 | 198.21 | 180.04 | 62.17 / 534.36 | 65.17 / 534.29 | 0.74 | 39.50 | 2643.19 | 6215.66 | true |
| large-sqlite-batched-1790711460604 | 6.32 | 180.07 | 55.32 / 525.39 | 65.30 / 589.61 | 0.68 | 40.45 | 2487.48 | 6291.09 | true |
| large-sqlite-batched-wide-1790711654291 | 8.51 | 180.04 | 5388.28 / 8705.80 | 5411.35 / 8705.76 | 0.87 | 41.96 | 2547.75 | 4559.86 | true |
| large-segments-1790711845744 | 4.20 | 0.28 | 15.15 / 15.74 | 29.37 / 29.79 | 0.09 | 14.74 | 3.04 | 1025.82 | true |
| large-segments-log-1790711867416 | 8.20 | 0.29 | 1.43 / 8.16 | 5.05 / 11.83 | 0.10 | 14.62 | 2.16 | 1024.89 | true |
| large-turso-wal-1790711877039 | 5.79 | 15.19 | 0.25 / 6730.83 | 1031.79 / 7838.47 | 1.74 | 2262.95 | 2151.75 | 1026.29 | true |
| large-turso-mvcc-1790711915566 | 6.78 | 19.56 | 0.26 / 9330.79 | 25.10 / 9336.02 | 1.39 | 6208.69 | 2147.42 | 1029.32 | true |
| large-turso-mvcc-passive-1790711961082 | FAILED | | | | | | | | true |
| large-sqlite-split-vacuum-1790713247856 | 5.95 | 12.72 | 0.11 / 5.85 | 185.10 / 4578.97 | 0.16 | 108.65 | 3309.63 | 8225.11 | true |
| large-segments-log-1790713268061 | 26.15 | 0.30 | 11.22 / 16.29 | 13.78 / 24.17 | 0.08 | 11.55 | 2.03 | 1024.85 | true |


The framed-segment run was repeated: acknowledgement 8.20–26.15 ms, reclaiming 6 GiB in 0.29–0.30 s, maximum foreground write 11.83–24.17 ms. These are two measurements, not a statistical confidence interval.

Incremental and batched SQLite reclamation did not finish within 180 seconds. Their remaining bytes must not be described as reclaimed. Wide batches caused an 8.7-second stall. Separate connections keep SQLite reads fast, but full VACUUM blocked writers for 4.58 seconds; WAL retained temporary space until closing/checkpointing (about 1 GiB after reopen).

Turso WAL and default MVCC had foreground stalls of 7.84 and 9.34 seconds during cleanup. Peak RSS reached 2.21 GiB and 6.06 GiB. The experimental passive-checkpoint MVCC run panicked during concurrent VACUUM: "MVCC vacuum gate acquired while transactions are still active". Exit 101; reopening verified all 1,032 externally acknowledged foreground writes and the published receipt. This is a failure of the tested combination, not evidence of payload corruption or a claim about all Turso workloads.

32 SIGKILL injections reached their requested point; 32 passed separate-process recovery and a subsequent new durable append/read-back. Stages: uncommitted publication transaction, durable publication, and partial cleanup. SIGKILL proves process recovery, not physical power-cut survival.

Recommendation: retain SQLite for small publication/control metadata and move payloads to generation-owned, checksummed append segments with a rebuildable memory index. Sync each frame before acknowledging it; persist segment ownership before its first append; atomically publish receipt/cutoff before unlinking; protect current and sealed-unpublished generations; validate recovery and partial tails. Turso does not remove the cost of physical compaction.

The application controller’s one-second health/pause budgets amplify stalls. These engine tests do not prove absence of VM restarts: application validation must remove maintenance from safety probes, reconcile unknown pause transitions, retain lease fencing and admission, and run repeated real VM checkpoints under load.

# Storage performance

S3-backed disks keep a durable local write journal and a bounded clean-block cache.
The logical VM disk size is not its physical host allocation. Old `data.ext4`
images remain separate; switching to S3 does not delete them.

Run the read-only inventory inside the runner using its shipped Node runtime:

```sh
docker exec -i RUNNER node --input-type=module < scripts/storage-report.mjs
```

It reports allocated journal/cache bytes, unused SQLite pages, dirty bytes and
age, synchronization backpressure, and cumulative disk counters. `withoutChatBytes`
is an inventory hint, **not** authorization to delete a disk: task runs and retained
work can still reference it. Each journal is read in a short SQLite transaction;
cache allocation and manager status can change during collection.

## Local diagnostics

The default log filter is `warn,cairn_performance=info`. If `RUST_LOG` is explicitly
set, include `cairn_performance=info` to enable the timing events. No diagnostics are
sent to an external telemetry service. Logs contain operation names, run/VM IDs,
durations and counters, never credentials, object keys or conversation content.

Manager `disk_publication` phases distinguish the publication queue,
cleanup, snapshot reconstruction, block transfers, manifest publication and the
journal acknowledgement. `disk_collection` distinguishes reader-lock contention
from remote deletion. `s3_totals` reports cumulative GET, PUT and exact-key purge
counts, bytes, errors and latency buckets since manager startup. PUT timing ends
at the upload response; its verification GET is included in GET counters. Purge
counts refer to entire exact-key purges, not individual S3 list/delete requests.

Runner events distinguish disk preparation, journal integrity scanning, mount and
network setup, guest boot, imports, freeze/seal/thaw, snapshot reconstruction,
journal reclamation, guest shutdown, VMM exit, unmount and network cleanup.
`storage_backpressure` records pause/resume transitions from storage enforcement.
Each phase logs its start as well as completion, so an operation still blocked is
visible. An `incomplete` event means an error, cancellation or dropped operation;
correlate it with the ordinary error logs.

Storage status includes `performance` counters for guest reads/writes/syncs,
remote block fetches, verified disk-cache hits, memory-cache hits and visited
journal rows. The runner also logs the final counters during VM shutdown. Counters
reset when a journal instance closes and reopens; compare samples from the same
instance. Remote fetches and journal row visits include snapshot reconstruction,
whereas read/write/sync counters measure the `Disk` interface. Duration totals are
summed operation time, not wall time, and can overlap. Latency buckets are cumulative
(>=10 ms, >=100 ms, >=1 s). No per-I/O logs or unbounded sample buffers are used.

## September 29, 2026 baseline

Production was running `7007ce14537b7b56c4832e902e799924b5028379` (`0.42.1`).
At 07:51 UTC the read-only inventory found:

| Allocation | Observed value |
| --- | ---: |
| Legacy local images | 40 images, 249.33 GiB allocated |
| S3-backed disks | 4 disks, 4.60 GiB allocated locally |
| Logical capacity of those four disks | 128 GiB |
| Unused pages retained in their journals | 1.06 GiB |
| Configured clean-cache budget for the node | 100 GiB |

This is not a measured before/after saving: legacy ext4 files are sparse and the
four new conversations have different working sets. It establishes that full
32 GiB images are not locally materialized for the new conversations, while old
images still dominate allocation. Cache growth is expected up to its node budget;
dirty journals are additional and cannot be evicted before durable publication.

Six sequential 4 MiB read probes through the real runner-to-manager-to-S3 path
took 293–1541 ms each. Recent event timestamps showed 14–35 seconds from workspace
preparation to a Codex turn, and 1–9 seconds from turn completion to run success.
These totals include non-storage work and are not paired local-vs-S3 benchmarks.
An active conversation was observed paused for `backup-lag`, with its oldest
dirty write 456 seconds old against a 300-second limit; it later recovered.
Publication is globally serialized, so one slow capture/upload/collection can
delay protection of other conversations. The phase timings quantify that delay.

All four journals reported `auto_vacuum=0`: enabling it after entering WAL mode
had silently left automatic reclamation disabled. The regression test reproduced
a published 4.2 MB journal retaining every page. New journals now enable reclamation
before WAL. Existing journals convert only when all writes have been published,
or when reopened already clean. Unpublished writes are retained and never vacuumed
by the compatibility conversion. This repairs retained free pages; it does not
delete legacy disks or reduce the configured clean-cache budget.

At 08:01 UTC, a separate cache inventory also found about 2.5 GiB of clean blocks
not referenced by their disk's current base manifest. Publication now retires
these obsolete cache files after draining old-base readers. It keeps current-base
cache blocks and updates the node cache index under its admission lock. Dirty
journals never participate in this eviction. Cleanup failures are logged without
invalidating an already durable publication.

## Batched snapshot reads

Stopped conversations do not retain an open journal. Previously every requested
4 MiB block reopened the journal and verified all outstanding writes. A publication
now requests up to eight missing blocks at once; the runner owns one journal for
that response and streams blocks individually. The relay also streams this route.
The response is bounded to 32 MiB, with at most one 4 MiB reconstruction buffer at
a time on the runner. Existing transport buffers and manager upload concurrency
remain unchanged. Disconnecting cancels stopped reads; a mounted VM's shared
journal remains usable. Generation checks and end-to-end block hashes still apply.

Older runners return 404/405 for the new route; older relays translate unsupported
operations to 503. These responses trigger one fallback to individual reads for
that publication. Truncated, oversized or corrupt successful responses fail the
publication. No writes are acknowledged until the normal durable S3 publication
and verification complete.

A local Firecracker comparison used a 128 MiB random-write fixture and fetched
the same eight 4 MiB blocks before and after stopping the VM. With the same
unoptimized candidate binary, stopped individual reads took 24.554 s in total;
a single batch took 3.435 s including client hash verification (7.15x faster).
Running reads took 0.434 s individually and 0.191 s batched. These are local
controller timings without S3, not production startup/shutdown estimates. The
unoptimized build makes absolute timings unsuitable for comparison with release
builds. The deterministic regression checks journal identity across streamed
blocks, cancellation, mounted-reader survival and preservation of newer writes.

This reduces stopped-publication overhead. Cold S3 fetch latency and guest boot
remain separate costs.

## Independent synchronization and shared cold reads

Synchronization, moves and deletion now serialize per conversation. A stalled
transfer no longer owns a manager-wide operation lock. Two synchronizations may
run at once, with the existing global limit of four S3 block uploads. Background
work uses two slots and schedules the least recently attempted conversations
first; slow cleanup cannot hold up the entire scheduler. Local cache admission
remains serialized, and cache eviction excludes disk mutations.

Collection drains readers of the affected conversation only. A read acquires its
conversation guard before rechecking authorization, so deletion cannot bypass
the reader fence. Versions still in use stay protected; this adds no archives or
restoration history. Tests hold a controller response open to verify independent
progress, same-disk exclusion, bounded scheduling and cancellation.

Concurrent reads of the same cold block share one verified transfer and cache
fill. Different hashes still download concurrently. A reproducer with eight
overlapping reads made eight transfers before this change and one afterward.
`performance.coalescedReads` counts reads served from memory after waiting behind
another reader of that block. No speculative prefetch is enabled, so this change
does not download additional blocks or increase the configured cache budget.

The real Firecracker storage suite passed with the candidate controller: the
cold-restored fixture needed four of thirteen published blocks before becoming
usable. Cancellation during an origin outage, preservation of local writes and
resumption after disk pressure also passed. The loopback fixture is not a
production S3 latency benchmark.

An optimized SHA-256 comparison on this host took about 399 ms/GiB for the
existing implementation and 397–407 ms/GiB for the proposed alternative. The
alternative was discarded because it did not improve optimized performance.

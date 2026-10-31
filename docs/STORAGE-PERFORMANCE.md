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

## Batched snapshot reads

Stopped conversations do not retain an open journal. Previously every requested
4 MiB block reopened the journal and verified all outstanding writes. A publication
now requests up to eight missing blocks at once; the runner owns one journal for
that response and streams blocks individually. The relay also streams this route.
The response is bounded to 32 MiB, with at most one 4 MiB reconstruction buffer at
a time on the runner. Existing transport buffers and manager upload concurrency
remain unchanged. Disconnecting cancels stopped reads; a mounted VM's shared
journal remains usable. Generation checks and end-to-end block hashes still apply.

All controllers provide the publication stream. Truncated, oversized or corrupt
successful responses fail the publication. No writes are acknowledged until the
normal durable S3 publication and verification complete.

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

## Persistent storage observations

On the installation host, install and inspect the optional collector from a
trusted checkout:

```sh
sudo bash scripts/install-storage-observer.sh
sudo systemctl status cairn-storage-observer.timer
sudo journalctl -u cairn-storage-observer.service
```

It samples every five minutes with bounded CPU, memory and runtime, without
restarting application containers. Docker access remains privileged operator
access. Private JSONL output lives in
`/var/lib/cairn-installation/observations/storage/` (directory 0700, files 0600),
rotated at 8 MiB and retained for at most 30 days / 128 MiB plus a checkpoint.
To stop collection while retaining its output:

```sh
sudo systemctl disable --now cairn-storage-observer.timer
sudo systemctl stop cairn-storage-observer.service
```

Records contain aggregate inventory and allowlisted numeric counters, not raw
logs, object keys, conversation content or credentials. Check snapshot age and
container/start epochs: missing samples do not prove zero I/O, and counter resets
suppress invalid deltas. Manager CPU includes all manager work; worker timings
are wall time. These samples do not classify cold/warm reads or establish precise
latency percentiles. Keep observations outside Git.

## Run JSON projections

SQLite 3.53.2 computes generated columns during updates, including VIRTUAL
columns whose inputs did not change. The former `runs.data` and
`runs.summary_data` columns therefore rebuilt JSON during scalar lifecycle
updates. On a busy v0.74.2 installation, a 15-second user CPU profile attributed
about 25% of sampled manager CPU to `jsonTranslateTextToBlob`. This identifies a
hot path; the sampling window alone does not measure a particular write's cost.

Database v19 moves these two expressions, unchanged, into the `run_views` SQL
view. Scalar writes target `runs`; wire readers use `run_views`. The three STORED
display fields remain on `runs`, so list summaries still avoid loading snapshot
overflow pages. Stored display fields and JSON validity checks can still parse
the snapshot on a write; this change removes the two wire reconstructions.

Run list pagination also keeps its prepared query plan. SQLite reparses a query
after rebinding a bare `LIMIT`, even when its integer value is unchanged, including
reuse through the statement cache.
The run reader already supplies signed 64-bit integers, so explicit integer casts
around `LIMIT` and `OFFSET` preserve their values while avoiding those repeated
parses. A regression checks SQLite's reprepare counter along with result ordering,
summary/full reads, filters, zero, negative and maximum integer pagination values.

The v18 -> v19 migration drops only VIRTUAL columns and creates the view in one
immediate transaction. The pinned SQLite implementation does not rewrite table
rows when dropping a VIRTUAL column. Failed DDL rolls back the drops and version;
concurrent starters recheck the version under the writer lock. Older binaries
reject v19. Retain a fresh consistent backup before upgrading and use a compatible
runtime afterward; an old binary alone cannot roll this migration back.

Reproduce the synthetic real-Store write and wire-read measurements against both
the unchanged reference and candidate, using the same test source and build flags:

```sh
TMPDIR=/var/tmp pnpm test:backend -p cairn-installation \
  --test run_write_performance -- --ignored --nocapture --test-threads=1
```

Each case seeds one run with a frozen 0, 128 or 512 KiB instruction string.
Write cases perform 32 changed/unchanged boundary or summary operations and
record process CPU, individual wall times, snapshot bytes, WAL bytes, SQLite
settings and subscriber revisions. Read cases exercise the actual summary/full
list reader 32 times using a reused or freshly opened connection. Read output
is checked on every iteration; full reads preserve the snapshot and summaries
exclude it. Connection-cold timing includes open/close; the operating system
cache remains warm. These local measurements do not include production network
latency and do not predict a whole-server speedup.

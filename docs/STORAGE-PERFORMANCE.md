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


### Paired local reference

The reference is `3bcf1a052b0d2a17cca82351ab64816731e5eb8b`; candidate Rust inputs
match `4ed41c6c0c73bfeca26f1d4400cb72c38a9c2fce` (crate tree
`3939a47283efb0d5b58726ec6ce851478a04ca8b`). Both executables use the same
benchmark source, SHA-256
`11fc6d541328a65f0748fb45155865fc462b8d7c7c0b088b8700f926cbb36c42`.
Copy that test file into the reference checkout to measure the same workload;
run only its ignored measurements there because the new write-plan guard
intentionally fails on the old implementation.

Measured on 2026-10-31 on a local Linux x64 workstation with
Intel(R) Core(TM) i9-14900K, 32 logical CPUs and 94.1 GiB RAM
(kernel 7.2.11-arch1-1), disk-backed `/var/tmp`
(ext-family filesystem), bundled SQLite 3.53.2, WAL/FULL and 4,096-byte pages.
Rust 1.97.1 uses the development profile with debug information and incremental
compilation disabled, and `cairn-installation` (including its integration test)
optimized at level 2. Dependencies retain their development defaults: this is
**not a release-profile or production measurement**. Add
`--config 'profile.dev.package.cairn-installation.opt-level=2'` before `--test`
in the command above, with `CARGO_PROFILE_DEV_DEBUG=0 CARGO_INCREMENTAL=0`.
Both planned qualification/build commands had returned before pairing; unrelated
workstation processes remained active.
Four alternating-order reference/candidate pairs produced 192 case observations.
Each table cell is the median across the four runs; wall time is the median
individual call within each run. Process CPU includes fixture assertions.

| Input KiB | Write operation | CPU / 32 calls, before → after (ms) | Median wall / call, before → after (µs) | WAL bytes |
| ---: | --- | ---: | ---: | ---: |
| 0 | boundary changed | 10.633 → 8.940 | 3695.75 → 3733.50 | 131,872 |
| 0 | boundary unchanged | 2.916 → 3.001 | 85.25 → 85.50 | 0 |
| 0 | state changed | 13.327 → 12.222 | 3792.50 → 3855.50 | 527,392 |
| 0 | state unchanged | 3.656 → 4.559 | 105.00 → 135.50 | 0 |
| 128 | boundary changed | 25.934 → 24.773 | 4123.50 → 4163.25 | 131,872 |
| 128 | boundary unchanged | 9.069 → 13.906 | 270.75 → 473.00 | 0 |
| 128 | state changed | 22.040 → 16.549 | 4063.25 → 3989.00 | 527,392 |
| 128 | state unchanged | 4.753 → 3.930 | 136.25 → 107.00 | 0 |
| 512 | boundary changed | 76.675 → 49.205 | 5511.75 → 4980.50 | 131,872 |
| 512 | boundary unchanged | 30.994 → 32.285 | 926.75 → 979.50 | 0 |
| 512 | state changed | 43.873 → 25.305 | 4721.75 → 4245.75 | 527,392 |
| 512 | state unchanged | 3.653 → 3.799 | 103.50 → 104.25 | 0 |

| Input KiB | Read projection | Connection | CPU / 32 reads, before → after (ms) | Median wall / read, before → after (µs) | Encoded bytes |
| ---: | --- | --- | ---: | ---: | ---: |
| 0 | Summary | Reused | 2.224 → 0.885 | 66.00 → 25.00 | 531 |
| 0 | Summary | New | 67.907 → 73.261 | 2097.75 → 2190.75 | 531 |
| 0 | Full | Reused | 2.797 → 1.431 | 82.25 → 41.00 | 1,626 |
| 0 | Full | New | 67.959 → 71.013 | 2125.50 → 2203.75 | 1,626 |
| 128 | Summary | Reused | 2.339 → 0.947 | 68.50 → 28.00 | 531 |
| 128 | Summary | New | 68.951 → 73.636 | 2138.75 → 2183.00 | 531 |
| 128 | Full | Reused | 14.273 → 13.138 | 431.00 → 393.50 | 132,698 |
| 128 | Full | New | 82.672 → 91.451 | 2550.75 → 2685.75 | 132,698 |
| 512 | Summary | Reused | 2.473 → 1.261 | 73.75 → 34.00 | 531 |
| 512 | Summary | New | 74.930 → 74.942 | 2253.00 → 2306.75 | 531 |
| 512 | Full | Reused | 52.629 → 49.471 | 1566.75 → 1497.00 | 525,914 |
| 512 | Full | New | 124.897 → 128.871 | 3859.00 → 3930.50 | 525,914 |

Encoded frozen write snapshots were 1,132 bytes at 0 KiB, 132,204 bytes at 128 KiB, 525,420 bytes at 512 KiB.
WAL bytes, snapshot bytes, wire byte counts and subscriber revisions matched
between variants in every case. Changed writes notify 32 times; unchanged writes
produce neither WAL growth nor notifications. Unchanged boundary calls at 128 KiB, several zero-input operations and
new-connection reads were slower in this sample. Keep those tradeoffs visible;
these medians do not establish statistical significance or whole-server gains.
The connection-cold cases include open/close and keep the OS cache warm.

Public executable SHA-256 values:

- Reference: `31a12b9759908e44058e905544ac2276eeffaf30af7d92dc98d71143eba3a158`.
- Candidate: `7a5faa21422dae56cd119d9b80a0be58afabcdc2ecdec34e11fe4eac9f39fb86`.

Use post-release actor queue/CPU measurements and the actual workload to assess
production impact. Retain raw numeric observations outside the repository.

## Publication candidate selection

The publisher and disk-status monitor share one candidate query. Its backup and
storage fields already live in the physical run boundary. Filtering the complete
wire projection reconstructed the frozen snapshot for each predicate, including
rows that were rejected. The query now filters those unchanged boundary fields
and constructs the full wire value only for selected rows. Publication revisions,
dirty-disk protection, legacy envelopes, ownership and scheduling retain their
existing conditions.

A deterministic SQLite plan regression detects wire construction in eligibility
predicates. It fails on the reference with eight `json_set` calls and passes after
the change with none. A differential fixture checks 588 combinations of status,
storage mode, dirty bytes and backup encodings, including nulls and legacy envelope
values, against the frozen reference query. Selected wire strings remain identical.

Reproduce the paired real-Store query and JSON-decoding measurement, alone:

```sh
TMPDIR=/var/tmp CARGO_PROFILE_DEV_DEBUG=0 CARGO_INCREMENTAL=0 \
  pnpm test:backend -p cairn-installation \
  --config 'profile.dev.package.cairn-installation.opt-level=2' \
  --lib nodes::publication::selection_tests::candidate_scan_measurement \
  -- --exact --ignored --nocapture --test-threads=1
```

The reference SQL is frozen from `23dee7bbce3379dc0a9358b06a89ab06d6023d23`.
Both variants execute in the same binary and fixture. The current measurement
uses clean committed source `0d22eff79a495fb2f3b577f0850dea36c4e22611`, including
the runner and concurrent-startup repairs from main, and Rust crate tree
`65eacca5ad9dbf68ab6d1c8cd039fad4b57a0afc`. The sealed executable SHA-256 is
`ffd1c7d3936f30807e6b685985fa90bece590059594dcaecc3d3a82d706a4452`.
Rust 1.97.1 uses the development profile with the installation package optimized
at level 2, development dependency defaults, no debug information and no
incremental compilation. This is not a release-profile or production measurement.

Measured on 2026-10-31 on the same Intel i9-14900K workstation described above,
with disk-backed `/var/tmp`, bundled SQLite 3.53.2 and a 9,355,264-byte database.
The fixture contains 64 runs with 128 KiB instructions; each query selects 32 runs
and returns 4,249,534 wire bytes. Timed reads use the actual `Db::json_rows` JSON
decoder, equality assertions and identical result ordering. Four alternating-order
pairs per connection condition each execute eight reads per variant. CPU cells
are medians of the four process-CPU batches. Wall cells are the median of each
batch's elapsed time divided by eight, rather than individual-call percentiles.
Task-owned compilation and tests finished before execution. No Rust compiler
process was observed immediately before invocation; this remains a shared workstation.

| Connection | CPU / eight reads, reference → candidate (ms) | Wall / read, reference → candidate (ms) | CPU reduction |
| --- | ---: | ---: | ---: |
| Reused | 271.544 → 116.046 | 34.033 → 14.542 | 57.3% |
| New | 285.945 → 136.670 | 35.871 → 17.127 | 52.2% |

New-connection timings include opening, read-only/foreign-key setup and closing.
The OS cache stays warm in both conditions; no host cache was evicted. Both
variants share the unchanged fixture write path: its 64-row construction and
transaction took 214.224 ms wall / 153.897 ms CPU, before timed reads. This control
includes fixture preparation and is not a separate before/after write comparison.
The read-only query connection leaves WAL size unchanged in every case. Database,
snapshot and output formats do not change.

An earlier run of the same query/fixture source, before those main integrations,
used Rust crate tree `9633411a99a706f72cf1bbd7d31d5bf6e2c60446` and sealed binary
`e9083c344d97c462879af472acf64ed8a9f9dc40008cb293db78c77bcf777213`.
It reported 55.9% lower reused-connection CPU and 49.2% lower new-connection CPU;
its shared fixture write control was 178.449 ms wall / 156.614 ms CPU. Those
observations remain prior-source evidence, separate from the current table.

An additional exploratory check on a private offline production snapshot used
Node SQLite 3.53.4, a 0.5-CPU/256-MiB network-isolated container and warm OS cache.
Both queries selected the same 140 rows and 11,782,175 wire bytes; median SQL-only
CPU fell from 1,381.122 ms to 161.821 ms across four alternating pairs. Its workload,
SQLite version and decoding boundary differ from the real-Store measurement, so
those figures are not interchangeable. No live database query or service was
started for that experiment. Production effect still requires post-release
CPU, actor-queue and synchronization observations under the actual workload.

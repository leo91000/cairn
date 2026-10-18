# Storage follow-up: discard, deltas and persistent observations

This is an offline format study and an optional host-side observer. Production
continues to use the compressed immutable v2 objects from the
[immutable-disk-object compression decision](adr/0036-compressed-immutable-disk-objects.md). No new disk
format, discard command or delta dependency is introduced into the application.

## Persistent observations

From a trusted checkout on the one-command installation host:

```sh
sudo bash scripts/install-storage-observer.sh
sudo systemctl status cairn-storage-observer.timer
sudo journalctl -u cairn-storage-observer.service
```

The installer atomically installs the collector and two systemd units. It does
not restart application containers. The collector runs every five minutes with
up to fifteen seconds of jitter, with low CPU/I/O priority, a quarter-core CPU
quota, a 192 MiB memory limit and a 75-second deadline. Its Docker access is
privileged operator access; the systemd restrictions do not sandbox the Docker
daemon. No new network listener or VM Docker access is added.

Private observations live outside containers in
`/var/lib/cairn-installation/observations/storage/`. The directory is mode 0700
and files are 0600. Daily JSONL segments rotate at 8 MiB; retention is at most
thirty days and 128 MiB, plus one bounded checkpoint. Stopping the timer retains
the measurements:

```sh
sudo systemctl disable --now cairn-storage-observer.timer
sudo systemctl stop cairn-storage-observer.service
```

Records contain aggregate inventory, rounded Garage bucket size, host load,
manager process CPU, and an explicit allowlist of numeric codec/GET/PUT/purge
counters from `cairn_performance / s3_totals`. The latest snapshot's timestamp
and age expose stale/missing snapshots; they do not prove zero I/O. No I/O
delta is emitted without a new counter snapshot, and each delta records its
counter timestamps separately from the host CPU sampling interval. Command runtime,
captured output and SQLite query work are bounded. Raw logs, object keys,
content hashes, conversation IDs, environment values and credentials are never
stored in the observations.

Each record identifies the container/start epoch. Counter resets suppress
invalid deltas; cumulative maxima and in-flight gauges are never subtracted.
Snapshots older than the current container start are excluded, including
checkpoints from earlier collectors: Docker retains logs across container restarts.
The manager CPU interval uses the actual `cairn serve` process's start ticks,
and excludes intervals shorter than thirty seconds. CPU includes all manager
work and must not be described as compression-only CPU. Worker timings are wall
times rather than CPU clocks. This does not add a warm/cold read classification
or make the slow-operation buckets into precise latency percentiles.

The append is synced before atomically replacing and syncing `state.json`.
Recovery uses the last valid sequenced JSONL record if the process died between
those steps; torn final lines in retained segments are repaired before choosing
the next append destination, including date and size rollover. This preserves
measurement continuity across collector crashes, manager restarts and ordinary
container updates. Missing observations and epoch boundaries remain visible.

## Read-only production sample

Use an operator-owned private directory on the execution node. The sampler only
reads immutable clean cache files and the published-base manifest in each local
journal's read-only SQLite connection. It does not reconstruct or alter the
live dirty journal, force a publication, fetch S3 objects, mount or trim a disk.

```sh
sudo python3 scripts/sample-storage-cache.py \
  --root /var/lib/cairn-installation/runner-state \
  --output /private/storage-sample --phase first \
  --disks 9 --regions 6 --metadata-disks 3 --max-mib 1024

# After a recorded interval, sample the same physical regions again.
sudo python3 scripts/sample-storage-cache.py \
  --root /var/lib/cairn-installation/runner-state \
  --output /private/storage-sample --phase second \
  --metadata-disks 0 --max-mib 2048
```

The source supports both legacy `disks` and physical `environments` paths. It
rejects symlinked cache/source paths, bounds copied bytes and skips regions that
became unavailable. The second capture uses the first selection; only regions
available at both phases enter the experiment. Labels are anonymous. The sampler
skips disks whose journals disappeared and reports the number skipped. If no
selected region remains available, it fails without publishing an empty phase.
Sampling prioritizes recently changed manifests and then systematically spaced cached
regions. It is cache-biased and excludes uncached data, so it is not a random
estimate of the installation's entire storage or future workloads.

Raw samples and private descriptors may contain credentials or other workload
data. Keep them private, transfer them only through the operator's protected
channel, and never upload them as GitHub or chat artifacts. Publish only the
aggregate result JSON and report.

## Offline discard preparation

```sh
uv run --python 3.11 scripts/prepare-storage-sample.py \
  --corpus /private/storage-sample --output /private/experiment-inputs
```

Preparation verifies every exported object against its original SHA-256/BLAKE3
identity, declared size and transfer checksum. Corrupt input fails before
filesystem interpretation. Filesystem tools operate only on newly created
private sparse copies. Those copies deliberately omit unselected file data;
they are not bootable restored filesystems or complete recovery backups.

Allocation is considered only when the complete ext4 journal and required
metadata are available. The copy undergoes `e2fsck -p -E journal_only`; failed
replay, an error-marked filesystem or remaining `needs_recovery` makes discard
unknown. Unsupported metadata layouts and uninitialized or unavailable bitmap
groups are excluded. `debugfs testb` checks the selected pages with metadata
checksum verification enabled. Block zero is always retained. Missing answers
or checksum errors invalidate the estimate; no byte-pattern heuristic infers
free space.

Only pages positively reported free are zeroed in a second copy. Every allocated
or unknown page stays unchanged. Both baseline and trimmed samples come from
the same replayed copy, so journal replay itself is not counted as a discard
saving. Synthetic tests preserve a real retained file and the original source
image while checking that deleted-file bytes disappear from the copy.

This models storage headroom. It does not qualify a live TRIM path: guest,
transport, local journal, publication cutoff and crash recovery still need one
end-to-end durability contract before enabling discard in Cairn.

## Comparative measurements

```sh
umask 077
uv run --python 3.11 scripts/benchmark-storage-formats.py \
  --output /private/new-trial \
  --corpus /private/experiment-inputs/production.private.json \
  --corpus /private/experiment-inputs/discard.private.json \
  --policies current-v2 delta-4m cdc-64k cdc-delta-64k

# Keep all owners live while trimming only the allocation-validated subset.
uv run --python 3.11 scripts/benchmark-storage-formats.py \
  --output /private/new-cohort-trial \
  --corpus /private/experiment-inputs/discard-cohort.private.json \
  --policies current-v2
```

Generate the controlled ext4 corpus by omitting `--corpus`, then reuse its
`corpus.private.json` for repeated trials over identical bytes. Repeat at least
three times with the same inputs and source checksum. Record host, CPU quota,
load, Python/library versions and concurrent work.

The reference is today's 4 MiB exact-deduplication/zstd/raw-fallback policy. Its
data envelopes have the same 41-byte overhead as v2. Metadata serialization,
AAD and SHA-256 hashing remain experimental; these are not native Cairn CPU
benchmarks or a production metadata/index implementation. All policies use real
AES-256-GCM, zstd and local files. No S3 WAN latency, upload retry/grace period,
fsync durability or deployment migration is modeled. Packed formats assume
ideal immediate compaction; upload and compaction record bytes are reported.

Delta admission tries at most four full bases using bounded similarity hints,
compressed XOR and zstd raw dictionaries. Dependency depth is one. Base costs
are retained even after the original owner disappears; uneconomical deltas are
materialized. Every generation, owner deletion and final-owner collection is
verified against exact source bytes. This is a feasible bounded candidate
search, not an optimal or exhaustive delta search.

Cold reads start with an empty decoded cache and use identical source offsets
across policies; the host page cache is not flushed. Warm reads reuse that
decoded data. Probe-position fingerprints verify that workloads match. Results
include read wall/CPU, fetched/decoded bytes and record GET counts, generation
write/GC wall and process CPU, encoded bytes, metadata, retained bases and
post-discard write/read measurements. Latency percentiles are per trial.

The fine-chunk read model directly addresses the requested 4 KiB range. Cairn's
current source/cache protocol reconstructs a whole 4 MiB region; those fine-read
latencies therefore require a separate protocol change and are optimistic for
an unchanged application. The 4 MiB delta comparison retains the source's
existing granularity. Consult the [native current-format measurements](STORAGE-COMPRESSION.md)
for actual codec and loopback-S3/LazyDisk timings.

## Qualification

```sh
python3 scripts/tests/storage_observer_test.py
uv run --python 3.11 --with cryptography==46.0.3 --with fastcdc==1.7.0 \
  --with zstandard==0.25.0 python scripts/tests/storage_formats_test.py
uv run --python 3.11 --with blake3==1.0.8 python scripts/tests/storage_sample_test.py
```

The isolated CI job uses synthetic data and has no production/agent secrets.
It checks counter epochs, stale samples, partial writes/checkpoint recovery,
retention limits, output allowlisting, integrity, base retention/materialization,
matching read workloads and ext4 discard on copies.

References: [ext4 journal recovery](https://www.kernel.org/doc/html/latest/filesystems/ext4/journal.html),
[debugfs checksum verification](https://man7.org/linux/man-pages/man8/debugfs.8.html),
[e2fsck journal-only mode](https://man7.org/linux/man-pages/man8/e2fsck.8.html),
[zstd patch-from](https://github.com/facebook/zstd/blob/dev/programs/zstd.1.md).

## Reference results: 2026-10-16

Three repeated trials used identical verified input bytes and benchmark source.
The reference source is commit `637bed84`. The current encoding and timed
read/write logic are unchanged; additional descriptor validation runs before
measurement and rejects unsafe, duplicate or unknown discard owners.
The production sample contains 54 regions from nine disks (216 MiB per phase),
with four regions changing over 504–508 seconds. This is a small cache-biased
sample, not an estimate of the whole installation. Complete journal/bitmap
metadata qualified discard on two disks, covering 48 MiB of selected regions;
allocation on the other seven disks remains unknown.

| Production cache policy | Stored MiB | Saving versus current | Write CPU ms | Cold 4 KiB p50 ms | Record GETs/read |
| --- | ---: | ---: | ---: | ---: | ---: |
| Current v2 model | 31.482 | reference | 6,950 | 34.673 | 1.000 |
| 4 MiB delta | 31.285 | 0.63% | 17,281 | 38.863 | 1.201 |
| 64 KiB CDC | 35.090 | −11.46% | 5,781 | 1.223* | — |
| 64 KiB CDC + delta | 34.943 | −10.99% | 8,261 | 1.402* | — |

Values are medians of three per-trial measurements, including per-trial latency
percentiles, not pooled latency samples. *Fine-chunk read timings assume direct
range access requiring a new protocol; they do not predict current Cairn reads.

For discard, keeping all nine owners live and changing only the two validated
owners reduces the current-format model from **31.451 to 28.987 MiB (7.83%)**.
Unknown owners keep their original references, so other owners retaining shared
blocks cannot be silently dropped from this comparison. This cohort uses one
phase; the cache/delta comparison above retains two generations.

Considering only the two eligible owners gives **9.186 to 6.722 MiB (26.82%)**.
On that subset, the discard transition uploads 1,845,382 encoded bytes and takes
510 ms wall/process CPU. Cold read p50 changes from 28.586 to 29.420 ms; post-trim
warm p50 is 0.0255 ms. These are local model measurements, with no production
network latency or durability flush. The 26.82% figure must not be extrapolated
to the seven unknown filesystems or the entire bucket.

A controlled four-filesystem workload with repeated small edits shows much
larger extra savings: 12.23% for 4 MiB deltas and 49.76% for CDC plus deltas.
The contrast confirms workload dependence; it does not justify adopting those
formats for the observed production workload.

Measurements ran offline on a shared Linux 6.12.109/glibc 2.36 VM with seven
reported vCPUs, no exposed CPU quota and Python 3.11.2. Dependencies are pinned
in the script/CI: cryptography 46.0.3, fastcdc 1.7.0, zstandard 0.25.0 and blake3
1.0.8. CPU is process time; host load and conditions are recorded in each JSON
trial. Decoded caches start empty for cold probes, but the OS cache is not
flushed. Absolute Python CPU/latency values must not be treated as native Rust
or production S3 performance. Aggregate trial JSON is delivered separately;
private workload bytes, hash inventories and paths are never published.

The next implementation to qualify is durable end-to-end discard, including
crash/restart and publication races. The observed 4 MiB delta tradeoff (0.63%
extra space for roughly 2.5× write CPU and 20% more fetched records) does not
support making it the production default. Keep the observer running across
releases to measure storage growth, errors and counter epochs before revisiting
this choice with a larger workload.

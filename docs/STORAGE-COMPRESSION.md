# Compressed immutable disk objects

New disk objects use zstd level 3 before AES-256-GCM encryption when the compressed
payload is smaller than the raw payload. Exact deduplication still identifies the
original plaintext, with the identity's declared SHA-256 or BLAKE3 algorithm.
Each object remains independent, immutable and bound to its unique S3 locator.
Compression introduces no base dependencies, additional GETs, pack compaction or
changes to publication ownership and garbage collection.

This implements the first recommendation from the storage-efficiency experiment.
That study's small, cache-biased production sample saved about 68% with compression;
it is not a forecast for the installation or the purged historical workload.
Delta compression provided little additional benefit on that sample and requires
durable dependency references and replacement materialization. Content-defined
chunking and discard propagation need separate changes and qualification.

## Format and integrity

| Bytes | Meaning |
| --- | --- |
| 8 | `CAIRNB`, byte `2`, byte `0` |
| 12 | Random AES-GCM nonce |
| 16 | AES-GCM authentication tag |
| Remaining | Encrypted codec byte, little-endian u32 plaintext length, payload |

Codec `0` stores raw bytes; codec `1` stores one ordinary zstd frame. The AAD is
`disk-block:v2:` followed by the exact immutable object key. This binds the outer
version to the object. The codec and declared length are encrypted and
authenticated. Altering the version cannot downgrade the authenticated content
to the older binary interpretation.

The maximum encoded object size is 4 MiB + 41 bytes. Raw fallback adds five bytes
relative to the previous binary envelope; it avoids compressed-size expansion,
but still pays the compression attempt's CPU cost. Empty blocks are omitted by
the existing manifest logic. Partial final blocks use their actual length.

Readers authenticate before decompressing, validate the declared length against
the authenticated manifest, bound output to 4 MiB, and verify the original content
hash. The decoder accepts exactly one frame with a matching known content length;
it rejects skippable frames, concatenated frames, trailing bytes, unknown codecs,
truncation and oversized output. Bulk decoding uses the bounded output buffer as
history instead of allocating a streaming window from the frame's advertised size.
Invalid objects return a terminal integrity conflict and trigger the existing
invalidation/baseline repair path; network outages retain their transient status.

Normal recovery accepts only the v2 envelope. Binary v1, JSON/base64 and prior v2
objects are accepted only by the blocking startup migrator, with their original
AAD and identity rules. Shared manifests still use `shared-v1` because their
hash/object-ID schema is unchanged. Shared object keys now use
`shared-blocks/v2/{hash}/{object}`; run-scoped keys use
`node-backups-v2/{run}/blocks/{hash}`. Normal reads never fall back to old keys or
envelopes. The separate top-level run namespace also protects replacements from
whole-run prefix deletions queued by an old executable. New conversation purges
queue both the manifest/old-data prefix and the v2 block prefix.

## Rollout and rollback

The manager database version advances from 5 or 6 to 7, fencing older binaries.
The manager must be stopped before upgrading; this is a downtime deployment.
Before `Service::new` returns, and before a listener, relay or publication worker
starts, migration completes these steps:

1. Authenticate every published manifest, including points retained by disk grants
   and conversations in trash. Journal unique referenced objects, reusable ready
   objects and local encoded caches in SQLite. Known obsolete shared objects and
   abandoned run-scoped upload intents are included for old-key cleanup.
2. Download an old object, authenticate/decode it and verify its original content
   hash and declared size. Encode it under its new locator, upload, download and
   verify the replacement. A replacement already uploaded by an interrupted
   attempt is verified and reused, without overwriting it.
3. Checkpoint the verified replacement before purging the old key and its versions.
   Checkpoint deletion separately. Local caches use a synced atomic rename and
   recognize an already-converted cache after a crash before its checkpoint.
4. Record completion atomically. Only then can normal service operation begin;
   normal reads immediately reject old envelopes. Later starts skip conversion.

Publication IDs, object incarnation IDs, plaintext hashes, authenticated manifests
and disk-grant dependencies stay unchanged. New keys disjoin replacements from
old objects and queued deletions, preserving immutability and preventing delayed
cleanup from deleting migrated data. A process lock prevents concurrent upgraded
manager processes. This does not coordinate with an old executable: stop the old
manager before installing the upgrade.

Refresh the host installer/supervisor before enabling this image for automatic
updates: previously installed host scripts are not refreshed by a container
update. The revised supervisor records the pre-update database version, waits
while migration is pending, and resumes the candidate after interruption. Once
storage advances, it refuses automatic rollback to the prior binary. The systemd
update service allows the one-time migration to outlast its former 50-minute
limit. Custom deployment controllers must also allow startup downtime and retain
the candidate after migration begins; a health timeout must not trigger rollback.

Migration processes one object at a time with bounded downloads and existing CPU
admission. It adds S3 downloads, verified uploads, deletion/version-list requests
and temporary space for the current replacement; downtime depends on the object
count, throughput and provider latency. Progress logs contain aggregate counts
and elapsed time. The durable `migration:disk-blocks-v2` summary records object
count, old/new encoded bytes and the last attempt's elapsed time. Separate S3
byte totals exclude local cache copies and unknown obsolete-object sizes. Failures
leave the service offline and resume on the next start; there is no silent data reset.
Restore any required previous storage configuration before restarting. Unknown
remote orphans are outside the authenticated inventory and are not bucket-scanned.

Rollback to a binary that understands database version 7, v2 keys and v2 envelopes.
Using an older binary requires restoring a consistent pre-upgrade database and
its referenced objects; the migrator deletes the old copies after verification.
Do not lower `user_version` on a live installation.

The manager compresses/decompresses S3 objects. The node publication stream and
manager-to-node restored bytes remain plaintext 4 MiB regions over their existing
authenticated transport. This saves S3 transfer bytes, not node-link bytes.
Verified node caches continue to retain decoded blocks, so warm disk reads avoid
the codec. The upload completion, manifest publication and receipt ordering are
unchanged: a failed upload retains the durable local work.

## Measurements and operational reference

Four CPU jobs at most run per manager through `spawn_blocking`. Owned permits stay
with the blocking jobs if callers cancel. Codec work does not block Tokio's
executor. This bounds concurrent codec contexts; existing request and upload
limits still govern transfers. Recovery additionally reserves one of sixteen
read slots before local/S3 fetch and retains it through decoding, including the
CPU queue. Completed downloads therefore cannot accumulate unbounded payloads
behind the CPU limiter. Waiting network reads do not occupy CPU slots, so an S3
read outage cannot stop encoding for a healthy upload path. Codec callers that
already own input buffers retain those buffers while waiting for admission.

`cairn_performance` / `s3_totals` now includes `metrics.blockCodec`. Its counters are
cumulative since process start, not per-conversation values. Compare snapshots
over the same interval and record workload, process restarts and concurrency.

- `encode` and `decode`: successful plaintext bytes, operation count, total/max
  worker wall-time microseconds, slow-operation buckets, errors and concurrency.
  These are execution timings, not thread CPU clocks; the benchmark separately
  measures process CPU, and production CPU usage needs the host's profiler.
- `encodedBytes` and `decodedInputBytes`: successful envelope bytes produced or
  consumed; compare these with the respective plaintext bytes for storage ratio.
- `rawWrites`, `zstdWrites`, `migrationLegacyReads`, `rawReads`, `zstdReads`: format counts.
- `admission`: wait to acquire a codec permit. Execution counters start in the
  blocking worker; blocking-pool scheduling delay is not in those counters.
- `readAdmission`: wait for the recovery fetch/decode budget, before fetching
  an object in the publication read path.
- Existing `get` and `put`: S3 transfer bytes, latency and errors, including their
  existing transfer admission behavior. These also include manifest operations.

Counters contain no block contents, keys, hashes or credentials and emit no
per-block log. The existing publication `uploadedBytes` field keeps its plaintext
meaning, preserving its contract. Use PUT counters for actual encoded transfer
volume; retries and manifest traffic can make it larger than codec output.

## Reproduce the reference

Run the pinned project tools. The launcher isolates live agent credentials and
uses only synthetic AWS credentials against loopback Moto:

```sh
uv run --python /usr/bin/python3.11 --with 'moto[server]==5.2.3' \
  python scripts/tests/node_s3_test.py --compression-benchmark
```

The release benchmark emits `BLOCK_CODEC_PERF` JSON rows for four deterministic
4 MiB inputs: mostly zero extents with a nonzero prefix, repeated repository source,
half source / half random bytes, and incompressible random bytes. The source
fixture is frozen from `storage/digest.rs`. Each row records its plaintext content
hash, so future comparisons can confirm that the inputs are identical.

For each input it runs two warmups and sixteen measured trials, alternating the
v1 reference and v2 policy order. Native measurements include offload, encryption
and verified decode. S3 write measurements include encoding and the shipped
checksum-verified PUT. Cold 4 KiB reads traverse a fresh `LazyDisk`, the shipped
S3 GET, manager decode/verification, node verification and clean-cache admission.
Every cold trial has empty local/decoded caches and an already-loaded manifest.
Warm reads average 256 reads from the verified node cache. The benchmark reports
p50/p95/mean wall time and process CPU time. CPU values for loopback trials exclude
the separate Moto process. Input cloning, corpus creation, local disk setup,
cleanup and equality checks after native decode are outside timed regions.

This is a local reference, not production cold-start latency: the provider's page
cache, DNS/TLS/WAN, VM boot, metadata bootstrap and production contention are not
measured. Small compressed objects can improve S3 transfer time while adding
codec work; incompressible data adds a compression attempt without network savings.
The loopback source also omits manager-to-node plaintext transport.
Record comparable workload and host conditions for future changes. Do not turn
these timings into flaky CI thresholds.

The `Storage compression reference` workflow runs this benchmark for storage PRs
and on manual dispatch. Its job summary compares both policies and its artifact
retains the sixteen JSON rows, input hashes, revision, host details and log for
thirty days. It also records three `STORAGE_MIGRATION_PERF` trials. Each trial
compares `Service::new` with a completed migration marker against startup that
must convert four old raw 4 MiB S3 objects from the same corpus. Migration timing
includes manifest authentication, durable planning/checkpoints, verified copy,
old-key/version cleanup and ordinary manager initialization. Fixture construction
and final cleanup are outside the timed regions. Wall time and manager process
CPU are recorded separately; Moto CPU and WAN latency are excluded. These small
trials establish a reproducible startup reference, not a downtime prediction for
a large installation.

For correctness against a versioned bucket, run the same launcher with
`--migration-tests`. It uploads multiple old source versions and checks that
migration and test cleanup leave neither versions nor delete markers.

These references avoid a latency gate tied to the timing of a shared CI runner.

Production deployment should preserve these counters and compare equivalent
intervals for S3 bytes, codec execution time, manager CPU usage, cold-fetch latency
and synchronization lag.
This PR does not deploy to production or generate benchmark writes there.

## Recorded reference

The [release run](https://github.com/leo91000/cairn/actions/runs/38141454326)
measured the implementation at PR head `61112373` on a four-CPU Linux x86-64
Azure runner (kernel 6.17, glibc 2.39, Rust 1.97.1), using the repository's
release settings with thin LTO. Two warmups precede sixteen alternating measured
trials per policy and input. The [full JSON reference](benchmarks/storage-compression-reference.json)
preserves input hashes, the tested merge revision, host metadata, byte counts,
wall/process-CPU p50/p95/mean and warm-cache timings.

Every input is 4 MiB; the old raw envelope is 4,194,340 bytes. These deliberately
constructed inputs establish a comparison, not a workload-weighted savings forecast.
The following medians include encoding plus loopback S3 PUT for writes and a
fresh LazyDisk/S3 fetch plus verified decode/cache admission for cold 4 KiB reads:

| Input | New envelope bytes | Write ms, raw → v2 | Cold read ms, raw → v2 |
| --- | ---: | ---: | ---: |
| Sparse | 192 | 28.583 → 4.043 | 12.842 → 6.447 |
| Repeated source | 3,290 | 28.047 → 3.991 | 12.614 → 6.221 |
| Half random / half source | 2,099,126 | 28.283 → 17.109 | 12.536 → 9.989 |
| Incompressible | 4,194,345 | 27.938 → 30.476 | 12.675 → 14.152 |

Compressible inputs reduced write medians by 40–86% and cold-read medians by
20–51% in this loopback reference. Incompressible input added five stored bytes,
about 9% write time and 12% cold-read time. Warm 4 KiB read medians across both
policies were 0.34–0.36 microseconds; every trial asserted exactly one cold fetch
and no warm fetches. Production plaintext transport, WAN and startup costs are
outside these measurements.

Native codec process-CPU medians, including encryption and verified decode:

| Input | Encode CPU ms, raw → v2 | Decode CPU ms, raw → v2 |
| --- | ---: | ---: |
| Sparse | 4.273 → 1.087 | 4.923 → 1.248 |
| Repeated source | 4.275 → 1.254 | 4.929 → 1.303 |
| Half random / half source | 4.278 → 3.373 | 4.945 → 3.198 |
| Incompressible | 4.387 → 5.590 | 4.955 → 5.529 |

## Blocking-startup reference

The [startup qualification run](https://github.com/leo91000/cairn/actions/runs/38161779663)
measured PR head `cd6741ca` on a four-CPU Linux x86-64 Azure runner (kernel
6.17, glibc 2.39, Rust 1.97.1), with the same release profile, corpus and input
hashes as the original reference. The
[complete JSON](benchmarks/storage-compression-startup-reference.json) records
all sixteen codec/read/write rows and three startup trials. Native and loopback
process CPU, wall p50/p95/mean and warm reads remain available for comparison.

Fresh loopback medians against the raw reference in that same run:

| Input | New bytes | Write ms, raw → v2 | Cold read ms, raw → v2 |
| --- | ---: | ---: | ---: |
| Sparse | 192 | 23.675 → 3.072 | 10.084 → 4.503 |
| Repeated source | 3,290 | 23.976 → 3.256 | 9.845 → 4.578 |
| Half random / half source | 2,099,126 | 23.978 → 13.863 | 9.808 → 7.625 |
| Incompressible | 4,194,345 | 23.361 → 25.598 | 9.800 → 11.187 |

Warm 4 KiB medians were 0.26–0.29 microseconds, with no warm GETs. Native
process-CPU medians for the same run:

| Input | Encode CPU ms, raw → v2 | Decode CPU ms, raw → v2 |
| --- | ---: | ---: |
| Sparse | 3.371 → 0.885 | 3.775 → 0.789 |
| Repeated source | 3.373 → 0.957 | 3.781 → 0.850 |
| Half random / half source | 3.374 → 2.492 | 3.782 → 2.398 |
| Incompressible | 3.332 → 4.168 | 3.763 → 4.103 |

Each startup trial converted one object from each of those four cases, totaling
16 MiB plaintext. Stored bytes went from 16,777,360 to 6,296,953 in every trial.
This equal-weight synthetic mix is not a production savings forecast.

| Trial | Startup without migration ms | Startup with migration ms | CPU without migration ms | CPU with migration ms |
| ---: | ---: | ---: | ---: | ---: |
| 0 | 10.111 | 322.520 | 9.585 | 89.704 |
| 1 | 9.732 | 472.226 | 9.232 | 83.576 |
| 2 | 9.697 | 479.419 | 9.244 | 82.372 |

The startup median was 472 ms with conversion versus 9.7 ms with a completed
marker. Conversion includes verification and old-key/version purge. This is a
small loopback reference; it excludes WAN, Moto CPU and production contention,
and must not be extrapolated into installation downtime.

## Qualification

The PR records the reference workflow run and measured comparison. Preserve its
JSON artifact with the input hashes and host metadata when making future
comparisons; benchmark results from a different workload or build profile are
not interchangeable.

Required checks are `pnpm lint:fix`, `pnpm check`, `pnpm test:backend` and
`cargo clippy --locked --workspace --all-targets -- -D warnings`. The normal S3
fixture suite also exercises compressed publication, deduplication, full restore,
corruption repair with a replacement incarnation, and ownership-aware collection.
Startup tests cover blocking migration, partial failure, restart checkpoints,
shared ownership, versioned source deletion and strict normal-format rejection.
Codec tests include independently produced migration-input/v2 fixtures, authenticated
malformed frames and cancellation while a blocking decode is queued.

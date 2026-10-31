# Compressed immutable disk objects

New disk objects use zstd level 3 before AES-256-GCM encryption when the compressed
payload is smaller than the raw payload. Exact deduplication still identifies the
original plaintext, with BLAKE3 identities.
Each object remains independent, immutable and bound to its unique S3 locator.
Compression introduces no base dependencies, additional GETs, pack compaction or
changes to publication ownership and garbage collection.

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
- `rawWrites`, `zstdWrites`, `rawReads`, `zstdReads`: format counts.
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

These references avoid a latency gate tied to the timing of a shared CI runner.

Production deployment should preserve these counters and compare equivalent
intervals for S3 bytes, codec execution time, manager CPU usage, cold-fetch latency
and synchronization lag.
This PR does not deploy to production or generate benchmark writes there.

## Qualification

Record reproducible measurements in the PR description: reference and candidate
SHAs, exact command, encoded bytes, cold/warm read latency, write latency, CPU,
workload, build profile and host conditions. Keep raw JSON/logs in temporary
files or CI artifacts; commit no measurement artifacts. Distinguish local costs
from production network latency. Results from different workloads or build
profiles are not interchangeable.

Required checks are `pnpm lint:fix`, `pnpm check`, `pnpm test:backend` and
`cargo clippy --locked --workspace --all-targets -- -D warnings`. The normal S3
fixture suite also exercises compressed publication, deduplication, full restore,
corruption repair with a replacement incarnation, and ownership-aware collection.
Ownership checks and strict format rejection remain covered.
Codec tests include independently produced v2 fixtures, authenticated
malformed frames and cancellation while a blocking decode is queued.

The bounded direct node-publication scheduling fixture, resource bounds and
controlled reference/candidate measurements are described in
[Direct publication scheduling reference](DIRECT-PUBLICATION-PERFORMANCE.md).

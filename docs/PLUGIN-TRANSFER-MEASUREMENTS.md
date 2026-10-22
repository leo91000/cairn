# Private plugin transfer measurements

Local reference run on 2026-10-21. This measures the generic transfer-file path,
not Gmail throughput, provider limits or production network latency.

Host: Intel Core i9-14900K (32 logical CPUs), Linux 7.2.9-arch1-1, Rust 1.97.1.
The measured files were on the NVMe-backed ext4 worktree filesystem. Rust used the
unoptimized test profile. Other build processes were active; this is a local
engineering check rather than a controlled performance benchmark.

## Workload and reference

Twenty 3 MiB binary files with deterministic nonzero data, one at a time. Each
iteration creates/writes a private file, syncs it, advises the kernel to discard
its page-cache contents, performs the first read and an immediate warm read,
checks byte equality and deletes it. The reference is an inline base64url encode
and decode of the same bytes. CPU is measured with process CPU time and includes
both paths and verification. Kernel eviction advice is best effort and does not
flush storage-controller caches.

| Quantity | Observation |
| --- | ---: |
| Binary payload | 3,145,728 bytes |
| Inline JSON/base64url reference | 4,194,315 bytes |
| File-handle JSON delivered to Luau | 145 bytes |
| Median create/write | 0.566 ms |
| Median first read after eviction advice | 1.577 ms |
| Median warm read | 0.442 ms |
| Median inline base64url round trip | 21.223 ms |
| Process CPU, all 20 iterations and both paths | 478.076 ms |

The payload remains 3 MiB on the temporary filesystem; handles do not compress it.
Chunked CLI transfer still pays base64/network overhead. The useful difference is
that a multi-megabyte provider payload can stay outside Luau's 1 MiB frame and
16 MiB worker memory limits. These timings do not establish end-to-end speedups.

The default `/tmp` on this host is tmpfs. An additional reference run there gave
0.663 ms writes, 0.472 ms first reads, 0.300 ms warm reads and 455.535 ms total CPU.
Eviction advice on tmpfs cannot demonstrate a physical cold disk read, so those
figures are not the cold-storage result above.

## Reproduce

Use a private directory on the intended temporary-file filesystem:

```sh
mkdir -p target/plugin-transfer-measure
TMPDIR="$PWD/target/plugin-transfer-measure" node scripts/test-backend.mjs \
  -p cairn-installation --lib plugin_transfer_local_measurement \
  -- --ignored --nocapture --test-threads=1
```

The ignored measurement lives beside `Files` and emits a JSON observation. It
uses synthetic bytes and no credentials. The backend launcher isolates any live
agent authentication environment. Ordinary tests skip the timing workload.

Durable HTTP receipts use the existing SQLite record format without a migration.
Their payload is bounded to 64 KiB per response and 10,000 records per instance;
the host does not add request bodies, OAuth secrets or transfer-file bytes. A
provider response can itself echo request content and is retained within that
bound. Transfer files have mode 0600 and are unlinked before writing their payload;
no named file survives a process crash. Handles are ephemeral, regularly expired
by the plugin maintenance loop and removed on disconnect/deletion; no transfer
index is persisted.

## Native authority and mutation-receipt record observation

Local run on 2026-10-22, after the selected native suites completed, with no
concurrent build or other qualification process. The unoptimized test profile
uses the real Store with two separate temporary databases. Both cases contain
1,000 representative completed receipt records with identical keys, response
values and field lengths. The reference hashes the previous intent input; the
new case hashes input with native authorization proof. Only the SHA-256 value
changes: authority details and secrets are not added to the persisted record.

Each case seeds its records, closes and reopens Store, then reads the same row
once with the new connection and five more times. It performs six same-value
writes. The first read is connection-cold; the OS cache is retained. Read timings
exclude actor scheduling; writes include it. CPU is process CPU time. Encoded
key-plus-JSON bytes exclude SQLite pages, WAL, indices and the kind string.

| Case | Encoded bytes, 1,000 rows | First read ms | Median warm read ms | Median write ms | Warm read CPU ms | Write CPU ms |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Prior fingerprint | 453000 | 0.762 | 0.055 | 0.267 | 0.058 | 0.285 |
| Authority-bound fingerprint | 453000 | 0.793 | 0.056 | 0.244 | 0.063 | 0.259 |

The VM runs Linux 6.12.109 and reported 7 available CPUs. Its cgroup CPU and
memory limit files were unavailable to the observation. Temporary databases were
on `/tmp` (tmpfs), so these are memory-backed I/O observations, not physical cold
disk latency.

This checks storage shape and local I/O only. It does not measure authorization
lookups, hashing, provider requests or complete plugin calls, and establishes no
throughput or production latency improvement. Raw wall/CPU samples, limits,
command and relevant source hashes are in
[the receipt observation](performance/PLUGIN-RECEIPT-AUTHORITY.json).

Reproduce alone with the credential-isolating launcher and the pinned toolchain:

```sh
CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 \
  pnpm test:backend -p cairn-installation --test plugin_oauth \
  receipts::measure_authority_receipt_storage_shape \
  -- --ignored --exact --nocapture --test-threads=1
```

The existing instance/key namespace and stored record shape remain unchanged.
The stronger fingerprint is part of the unreleased epoch-10 compatibility gate.
An older receipt conflicts rather than replaying a response without current
authority or silently repeating an uncertain write. No deployment or downgrade
is authorized by this observation.

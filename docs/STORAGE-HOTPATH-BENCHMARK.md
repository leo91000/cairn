# Storage hot path: first optimization cycle

Measured on 2026-09-30, against the implementation in PR #25. The harness is
`backend/examples/storage-benchmark.rs`; run it in release mode. Integrity
assertions check all returned bytes. No S3 or FUSE is involved in this microbenchmark.

## Local working set

Nine distinct 4 MiB blocks, read alternately in 4 KiB requests. The warm case
performs 576 reads (2.25 MiB requested), following one read per block. Both cache
sizes below use the new implementation, isolating the effect of capacity rather
than comparing unrelated binaries.

| Configuration | First pass over nine blocks | Subsequent 576 reads | Verified blocks, both passes |
| --- | ---: | ---: | ---: |
| SHA-256, 32 MiB cache | 23.23 ms | 1276.06 ms | 585 |
| SHA-256, 256 MiB cache | 21.65 ms | 0.377 ms | 9 |
| BLAKE3, 256 MiB cache | 14.33 ms | 0.382 ms | 9 |
| Direct positional disk reads, warm page cache | — | 0.381–0.404 ms | — |

The 32 MiB cache thrashes: it verifies about 2.29 GiB for 2.29 MiB requested.
The larger cache admits 36 MiB once. This deliberately adversarial workload
demonstrates the amplification, not a promised speedup for every conversation.
Warm reads fitting the cache are comparable to the direct disk interface on
this host; the comparison excludes FUSE, remote reads and guest scheduling.

Hashing 1 GiB in 4 MiB chunks took 396.38 ms with the existing AWS-LC SHA-256
and 174.65 ms with BLAKE3, about 2.27 times faster. This uses single-threaded
hash calls; it does not add a background CPU pool. Small journal writes also
retain their per-append durability barrier, whose disk cost hashing cannot remove.

## Production reference before deployment

A newly created, isolated no-project conversation on v0.49.0 was asked to answer
one fixed token without commands. It succeeded. Its first agent message arrived
19.19 seconds after run start; the turn completed after 25.26 seconds. The run
became fully finished after 74.52 seconds, including final synchronization.
Codex initialization took 2471 ms, login setup 131 ms, thread start 714 ms and
turn start 205 ms. The wait from turn start to first message was about 7170 ms.

This single reference separates model response from environment readiness and
finalization. Repeated post-deployment samples and real Firecracker/FUSE soak
results are still necessary before claiming a production startup or I/O gain.
The two older approximately 91-second pauses were observed on other conversations;
their trigger was not recorded, so they are not the baseline of this fresh-chat test.

## Durable writes on persistent storage

`cargo run --locked --release --example storage-write-benchmark` compares the
direct disk interface, the segmented journal, and the controller's journal with
write admission enabled. Each acknowledged write is followed by a checked read.
Three samples rotate their order. Temporary files are created in the current
directory, or `LEO_BENCH_ROOT`, rather than implicitly using `/tmp`.

The recorded run used ext4 on `/var/tmp`. The median of the three samples' write
medians was:

| Write size | Direct disk | Journal | Journal with admission |
| --- | ---: | ---: | ---: |
| 4 KiB | 1.742 ms | 3.401 ms | 3.427 ms |
| 64 KiB | 1.744 ms | 3.596 ms | 3.362 ms |
| 1 MiB | 4.113 ms | 4.367 ms | 4.454 ms |

The first 4 KiB sample was faster across every mode, so these values are evidence
from a shared host, not universal disk-latency constants. Small durable journal
appends still cost about twice direct overwrites in this workload. The admission
checks are not the dominant cost here. This comparison excludes FUSE, VM
scheduling, remote storage and model response.

The first real-VM soak used `/tmp`, which is tmpfs on the test host. It passed
30,080 writes and 48 repeated publications with no restarts, but its 1.7 ms p99
does not establish persistent-disk latency. The sustained qualification now
defaults to `/var/tmp`, matching the nested-Android probe, and accepts
`VM_TEST_ROOT` for an explicit test volume. Final-image persistent-disk and
post-deployment measurements remain necessary.

The fixture also independently verifies every transferred block. Its pure
JavaScript BLAKE3 oracle took 810–850 ms for 14 blocks, versus about 22 ms for
native SHA-256. That test-only work must be separated from runner timings when
comparing total fixture publication time; the production BLAKE3 path is native Rust.

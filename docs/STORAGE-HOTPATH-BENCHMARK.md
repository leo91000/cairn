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

# Native stopped checkpoint measurements

Local measurement on 2026-10-19, Linux 6.12.109 x86-64, overlayfs in a shared
Cairn run. The build uses pinned Rust 1.97.1, debug symbols and incremental
compilation disabled, and one Cargo worker. No Android or other Cargo build ran
concurrently with the sample. VM CPU and memory allocations can change: seven CPUs
were visible when recording host conditions after the sample; the allocation at
each timing point was not captured or pinned. One sample per path: these are
reproducible engineering observations, not production latency or a statistical
performance claim.

```sh
CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=1 \
  pnpm test:backend -p cairn-installation --lib measure_native_stopped_storage \
  -- --ignored --nocapture --test-threads=1
```

The test creates 128 text files of 4 KiB and an 8 MiB random asset seeded with 42.
It compares the existing file-stream transfer written/fsynced directly against
native stopped capture, which writes/fsyncs the same stream and indexes its blocks.
Reads extract the full stream into a new directory. Native reads additionally
materialize and verify blocks through the checkpoint reader. CPU is process user
plus system time from `getrusage`; wall time uses a monotonic clock. The cold case
uses a file-specific `POSIX_FADV_DONTNEED` hint after fsync; it does not flush the
host's or overlay backing storage caches. The next read is warm.

| Quantity | Reference file stream | Native checkpoint |
| --- | ---: | ---: |
| Logical file bytes | 8,912,896 | 8,912,896 |
| Local archive bytes | 11,897,823 | 11,897,823 |
| Manifest bytes | none | 635 |
| Fsynced write wall / CPU ms | 2284.86 / 2248.42 | 2514.23 / 2483.94 |
| Cold hinted read+extract wall / CPU ms | 1655.94 / 1517.91 | 1162.03 / 1174.10 |
| Warm read+extract wall / CPU ms | 1392.89 / 1323.64 | 708.32 / 738.00 |

The existing S3 block codec produces 8,414,091 encoded block bytes from this native
archive. Encoding wall / CPU: 4927.13 / 4931.65 ms in this debug build. That byte
count includes compression and authenticated encryption, excluding the manifest
and transport headers. Read order and variable VM allocation can affect the timing
comparison; repeated measurements with fixed resources are needed for throughput
claims. No S3 requests, production network latency, throughput,
GPU work or production desktop responsiveness are measured.

The framed/base64 archive costs about one third more local staging space than
raw files. Each stopped capture scans and encodes the complete included tree;
block publication can deduplicate equal blocks, but file shifts can change many
blocks. Large build trees can therefore add substantial checkpoint time and
scratch-space use. This implementation deliberately advertises stopped consistency
and reserves free space rather than promising cheap live incremental backups.
Use measured project workloads before choosing native execution as the general
default. A file-inventory/delta checkpoint format would need its own compatibility
and measurement review.

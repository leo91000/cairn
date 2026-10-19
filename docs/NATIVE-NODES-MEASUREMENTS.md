# Native stopped checkpoint measurements

Local measurement rerun after the retained-pool and failed-capture corrections on
2026-10-19, 14:47–14:48 UTC, Linux 6.12.109 x86-64, overlayfs in a shared
Cairn run. The build uses pinned Rust 1.97.1, debug symbols and incremental
compilation disabled, and one Cargo worker. No Android or other Cargo build ran
concurrently with the sample, and the web tests had finished. Seven CPUs and
19,462,272 kB total RAM were visible both before and after; reported available RAM
changed from 3,557,988 to 3,532,036 kB. Resources were not pinned and other host
activity was not controlled. One sample per path: these are
reproducible engineering observations, not production latency or a statistical
performance claim.

```sh
CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=1 \
  pnpm test:backend -p cairn-installation --lib measure_native_stopped_storage \
  -- --ignored --nocapture --test-threads=1
```

The test creates 128 text files of 4 KiB and an 8 MiB random asset seeded with 42.
It compares the existing file-stream transfer written/fsynced directly against
native stopped capture, which invalidates the prior point, writes/fsyncs the same
stream, indexes its blocks and commits the completed pointer. This measures the
capture path directly, with an unrestricted growth allowance; shared-pool admission
and concurrent transfer locking are covered by separate regressions.
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
| Fsynced write wall / CPU ms | 2155.55 / 2138.93 | 4111.43 / 3887.08 |
| Cold hinted read+extract wall / CPU ms | 2042.93 / 1977.01 | 2121.38 / 1973.02 |
| Warm read+extract wall / CPU ms | 2030.55 / 1978.81 | 2322.05 / 2235.27 |

The existing S3 block codec produces 8,414,091 encoded block bytes from this native
archive. Encoding wall / CPU: 7428.23 / 7421.61 ms in this debug build. That byte
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

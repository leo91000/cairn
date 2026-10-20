# Native storage measurements

## Host ublk and lazy S3 resume

Measured on 2026-10-20, 15:52–15:53 UTC, at
`883c6e6ede971414c4c0642307ea045219fcab51` in the successful
[disposable-host qualification](https://github.com/leo91000/cairn/actions/runs/39451413776/job/118729775461).
The [recorded JSON](measurements/native-storage-883c6e6e.json) is the workflow's
`native-storage-evidence/native-storage.json` artifact. Later qualification adds a
distinct second unpublished mutation to make the repeated-restore assertion catch
a skipped restore; these timings remain evidence for the measured source above.

The runner reported Linux `7.0.0-1017-azure`, x86-64, four visible CPUs, AMD EPYC
9V45, and 16,367,524 kB total RAM. `stat -f` reported the host filesystem family as
`ext2/ext3`. Native execution used one CPU, 512 MiB RAM and a 256 MiB ext4 disk on
ublk. The reference ran as host root without those workload cgroup limits. This
is one sequential sample per path on a hosted runner; CPU allocation, cache state
and unrelated host activity were not held constant. It does not isolate the cost
of ublk from cgroup, journal, SDK, encryption or host scheduling costs.

The exact same Python workload writes and fsyncs 128 text files of 4 KiB, then
reads an 8 MiB random asset seeded with 42 twice. The reference accesses the host
filesystem directly. `nativeRetained` uses the initialized journal-backed disk;
`nativeEvicted` runs after acknowledged clean-cache eviction, controller restart
and remount. The controller and Cairn runner use a development build with debug info
and incremental compilation disabled. S3 is Moto 5.2.3 on loopback with synthetic
credentials: these numbers contain no WAN or production S3 latency, model calls,
GPU execution or personal desktop measurements.

| Operation, wall / workload CPU ms | Direct host reference | Native retained | Native after eviction |
| --- | ---: | ---: | ---: |
| 128 fsynced 4 KiB writes | 76.32 / 7.70 | 261.65 / 12.38 | 214.30 / 12.04 |
| Cold-hinted 8 MiB read | 3.08 / 3.08 | 6.98 / 3.76 | 940.76 / 4.05 |
| Following warm 8 MiB read | 3.61 / 3.61 | 3.71 / 3.71 | 3.64 / 3.64 |

Wall time uses `perf_counter`; CPU is `process_time` of the Python workload,
excluding controller/backend, S3 server, encryption/decompression workers and
other processes. It is not total storage CPU. Cold reads use advisory
`POSIX_FADV_DONTNEED` on that file only; neither all host caches nor cold physical
I/O are verified. The warm sample follows the hinted read immediately. After
eviction, the controller restart also clears its userspace caches. The observed
post-eviction hinted read was about 0.94 s even on loopback; repeated realistic
workloads and release builds are needed before making throughput claims.

| Byte accounting | Observed bytes |
| --- | ---: |
| Reference raw payload, asset + 128 text files | 8,912,896 |
| First published point, encoded S3 objects | 8,434,414 across 10 objects |
| Clean local cache reclaimed | 37,748,736 |
| S3 inventory after all scenarios, before purge | 8,566,387 across 29 objects |

The first-point total includes compressed/authenticated-encrypted blocks and its
encrypted manifest record, including filesystem metadata and durable provider/user
state. It excludes HTTP overhead and is not the same content boundary as the raw
payload-only reference. The final inventory covers the whole qualification, not
one checkpoint. Native retained/evicted remote-request deltas were 0/2. Despite the
JSON key `readTriggeredRemoteRequests`, that counter spans both the fsynced writes
and the asset reads: it proves demand requests occurred during that combined
workload, not a read-only request count. The script checks that unrelated
conversation cache is unchanged and that purge leaves the synthetic bucket empty.

This harness manually stages attempt inputs and invokes the real controller. It
covers kernel ublk/ext4, unprivileged execution, secret bindings, legacy migration,
publication/acknowledgement, active/dirty/stale eviction refusals, CPU pause/resume,
restore, lease/guardian/controller fencing, crash recovery and stopped capture.
It does not measure the production remote connector's total startup transfer.
That shared VM connector still transfers the original checkout in attempt staging;
the initialized journal remains authoritative and does not reseed from it.

Reproduce only on a disposable Linux systemd host with cgroup v2, `ublk_drv`,
e2fsprogs and the isolated Python/Moto/AWS CLI environment prepared by the
[workflow](../.github/workflows/ci.yaml) `native-runtime` job:

```sh
CARGO_PROFILE_DEV_DEBUG=0 CARGO_INCREMENTAL=0 \
  cargo build --locked --bin cairn --example native_storage_qualifier --features ublk
sudo env PATH="$PWD/.native-s3-test/bin:$PATH" CAIRN_NATIVE_DISPOSABLE_HOST=1 \
  NATIVE_STORAGE_EVIDENCE_DIR="$PWD/native-storage-evidence" \
  .native-s3-test/bin/python scripts/tests/node_s3_test.py \
  --native-runtime target/debug/cairn \
  --qualifier target/debug/examples/native_storage_qualifier
```

## Legacy file-archive measurements

Historical epoch-6 native file-backend reference, retained for comparison of the
old approach. These numbers do not describe the current ublk implementation and
were collected on a different host with a different workload path.

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

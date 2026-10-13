# Exclusive suspension metadata measurements

Measured locally on 2026-10-13. This compares the existing attempt/VM binding
with the additional durable suspension metadata. It does not compare codecs or
change SQLite, disk journal, compression or encryption policy.

Reproduce:

```sh
CARGO_BUILD_JOBS=1 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_DEV_INCREMENTAL=false \
  pnpm test:backend -p cairn-installation --lib suspension_metadata_measurements \
  -- --ignored --nocapture --test-threads=1
```

Host: Linux 6.12.109, Intel Xeon virtual CPU, 12 available CPU threads,
32,363,544 KiB total RAM, Rust 1.97.1, development build with debug information
and incremental compilation disabled. Files are on the run's tmpfs filesystem.
Compilation finished before timing; no other heavy measurement workload ran.

Workload: 100 atomic, fsynced reference writes; 100 alternating exclusive-reason
mutations while a storage pause stays acknowledged; 500 warm reads and 500
fresh-descriptor reads of each file. One synthetic local Firecracker API pause
precedes timing; zero state requests occur in the measured write loop. CPU is
parent process CPU, measured with `getrusage`.

| Measurement | Existing attempt binding | Suspension state | Private VM-owner binding |
| --- | ---: | ---: | ---: |
| Encoded JSON bytes | 58 | 171 | 21 |
| Write median | 0.095679 ms | 0.134527 ms | Included in binding setup |
| Write p95 | 0.106190 ms | 0.195079 ms | Included in binding setup |
| CPU for 100 writes | 12.350 ms | 17.315 ms | Included in binding setup |
| Warm read median | 0.000233 ms | 0.000233 ms | 0.000229 ms |
| Fresh descriptor read median | 0.000523 ms | 0.000520 ms | 0.000629 ms |

These byte counts describe the reproducible fixture: attempt ID `attempt`, empty
run ID in the reference, and a UUID VM ID. UUID attempt/run IDs add bytes; active
exclusive reasons add their reservation ID and epoch. The final measured state
contains the persistent storage reason after the alternating exclusive reason
has been removed. It is not an estimate of every production record.

The fresh-descriptor read uses advisory `POSIX_FADV_DONTNEED` before opening a
new descriptor. This does **not** prove a cold filesystem cache, particularly on
tmpfs. The measurements include local fsync behavior, not physical disk or S3
durability latency. Real VM pause, reclaim and resume behavior is qualified
separately by the candidate-image smoke test; its logs must supply the evidence
before claiming a working host configuration or a production performance gain.

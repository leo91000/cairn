# Prepared native project bases

Prepared bases extend the [project ZIP cache](PROJECT-ARCHIVES.md). A warm native
conversation on the execution node copies its small private journal metadata and shares an immutable
sealed prefix. It avoids downloading, hashing and unpacking another project ZIP,
creating project inodes, formatting another filesystem and changing each file's
owner. The private volume still validates its journal before mounting and checks
payload integrity on reads.

See [ADR-0058](adr/0058-prepared-native-project-bases.md) for ownership, compatibility,
cache admission, cancellation and recovery. These are disposable caches; the
installation data epoch and authoritative disk formats are unchanged. Existing
conversations preserve their saved files and continue using their pinned tools.

## Reproducible measurements

Run the production archive extraction, base construction, cloning, geometry,
volume-open and journal I/O through the isolated backend launcher:

```sh
mkdir -p "$HOME/.cache/cairn-bases-benchmark"
TMPDIR="$HOME/.cache/cairn-bases-benchmark" pnpm test:backend \
  -p cairn-installation --lib measure_prepared_native_bases -- --ignored --nocapture
```

The workload contains 4,096 text files of 4 KiB and 8 MiB seeded random bytes
(StdRng seed 42), 24 MiB total, in a 256 MiB virtual ext4 filesystem. Three
alternating samples compare cached ZIP extraction/ownership/final synchronization
with durable private metadata cloning, assignment, geometry checking and full
journal open. Base construction is measured separately. Output records encoded
ZIP bytes, allocated base bytes, new private bytes, wall and process CPU time,
advisory cold/warm 4 MiB journal reads and a synchronized 4 KiB raw journal delta.

This benchmark uses a local host filesystem, with no ublk mount or WAN. Its ZIP
`syncfs` can include unrelated host writeback; it does not establish a production
startup speedup. `POSIX_FADV_DONTNEED` is a cold-read hint, not proof of a cold
physical device. The block write is a disposable journal I/O probe, not a complete
application filesystem transaction. Real mounting, workload isolation, S3,
restoration and frozen-workload shutdown are qualified separately by the
disposable native-runtime CI fixture.

On 2026-10-23, local ext4/NVMe on an i9-14900K, Linux 7.2.9, 94 GiB RAM,
with other builds and native workloads running, produced the following
development-profile samples. Host load at completion was 11.49 / 8.30 / 8.02.

| Metric | Cached ZIP | Prepared base |
| --- | ---: | ---: |
| Sample 1 preparation | 2,952 ms | 138 ms |
| Sample 2 preparation | 413 ms | 159 ms |
| Sample 3 preparation | 397 ms | 150 ms |
| Median preparation | 413 ms | 150 ms |
| Median process CPU | 353 ms | 82 ms |
| Project bytes expanded / new private allocated bytes | 24 MiB | 80 KiB |

The ZIP contains 9,102,989 bytes. The reusable journal base allocates 67,194,880
bytes (64.08 MiB) and takes 1,164 ms wall / 476 ms CPU to prepare once. This
trades more bounded per-version cache space for less per-conversation writing.
The median of three advisory cold/warm 4 MiB reads was 3.38 / 0.20 ms; the
synchronized 4 KiB journal delta took 17.84 ms wall / 0.44 ms CPU. The first ZIP
sample shows the variability of host-wide writeback; the three samples and
measurement boundaries are retained instead of treating that sample as a steady
production comparison.

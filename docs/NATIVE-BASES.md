# Prepared native project bases

Prepared bases extend the [project ZIP cache](PROJECT-ARCHIVES.md). A warm native
conversation on the execution node copies its small private journal metadata and shares an immutable
sealed prefix. It avoids downloading, hashing and unpacking another project ZIP,
creating project inodes, formatting another filesystem and changing each file's
owner. The private volume still validates its journal before mounting and checks
payload integrity on reads.

See [ADR-0058](adr/0058-prepared-native-project-bases.md) for ownership,
cache admission, cancellation and recovery. These are disposable caches.
Existing conversations preserve their saved files and resume with current tools.

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
with durable private metadata cloning, atomic journal publication, geometry checking and full
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

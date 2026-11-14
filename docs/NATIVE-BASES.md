# Prepared native project bases

Prepared bases extend the [project ZIP cache](PROJECT-ARCHIVES.md) with verified
plain directory templates. A fresh environment gets its own private file copy,
then publishes its native directory format before execution. It avoids fetching
and decompressing another ZIP; it still creates private files and verifies their
contents. File ownership, links and xattrs are preserved. The cache shares no
mutable working-file inodes with conversations.

[ADR-0085](adr/0085-native-directories-and-direct-vm-images.md) supersedes the
journal-clone format in [ADR-0058](adr/0058-prepared-native-project-bases.md).
A durable environment-qualified initialization pin keeps the verified source
through copying, rename and format publication. Interrupted copies retry their
private partial tree. Renamed data is verified without overwriting it. A committed
format wins over a leftover pin, preserving later user changes. Environment
deletion releases unfinished pins and private data; cache reclamation follows
its existing limits. Existing conversations retain their own files.

## Reproducible measurements

Use a disposable Linux host with root, ext4 project quotas and the pinned native
build dependencies. The isolated launcher creates one private loop filesystem,
executes the actual quota/base admission/recovery cases, then the measurement:

```sh
CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 \
  CAIRN_NATIVE_DISPOSABLE_HOST=1 node scripts/qualify-native-quota.mjs
```

Do not designate a personal or production host. The launcher refuses an unset
flag and retains its owned fixture if unmount cannot be confirmed. It requires
exactly one passing, non-ignored result for each selected case. Logs and JSON
measurement rows belong outside tracked files.

`measure_prepared_native_bases` uses 4,096 text files of 4 KiB and 8 MiB seeded
random bytes (StdRng seed 42), 24 MiB total. Three alternating samples compare
cached ZIP extraction/ownership/final synchronization with verified private
prepared-directory admission, including format publication and initialization.
Both destinations use the same data filesystem and 256 MiB project quotas. Base
construction is measured separately. Output includes encoded ZIP bytes, allocated
base/private bytes, wall time and process plus reaped-child CPU (including rsync),
identical advisory cold/warm 4 MiB file reads and a synchronized new 4 KiB file.

This is local filesystem preparation and IO, not production network latency.
Pool-wide `syncfs` can include other writeback; the cases run serially on their
owned filesystem. `POSIX_FADV_DONTNEED` is advisory, not proof of cold hardware.
Actual execution, Docker, controller crash recovery and expiration are separately
qualified by the full disposable native-runtime fixture. The local legacy-journal
versus direct-file block comparison is documented in [journal qualification](JOURNAL-RECOVERY.md#qualification).

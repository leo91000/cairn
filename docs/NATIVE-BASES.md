# Prepared native project bases

Prepared bases extend the [project ZIP cache](PROJECT-ARCHIVES.md) with verified
plain directory templates. A fresh environment gets its own private file copy,
then publishes its native directory format before execution. It avoids fetching
and decompressing another ZIP; it still creates private files and verifies their
contents. File ownership, links and xattrs are preserved. The cache shares no
mutable working-file inodes with conversations.

The copy does not fsync each file. After the checksum verification, one `syncfs`
of the data pool flushes the copy; it is renamed and its format published only
after that barrier succeeds. The barrier's descriptor is opened before the copy
starts, so a writeback error during the copy fails it even when another
environment's `syncfs` observed that error first. Recent ext4 kernels can return
success from `syncfs` after a shutdown during the flush itself; the data-pool
rename and directory fsync that follow then fail with an I/O error, so the
format is never published. A crash before publication leaves a partial private
tree that initialization retries from the pinned source.

Per-file fsync cost one host journal commit per file on a loop-backed pool: on a
32-thread i9-14900K host, the 5,201-entry / 113 MiB cairn base took 66.7 s with
per-file fsync and 2.0 s without it (0.2 s copy, 0.1 s verification, 1.7 s
`syncfs`). These are local measurements at load average 2–5. `syncfs` also
flushes other writers on the same pool and grows with their dirty data. It runs
under the node storage lock and, unlike the copy, a stop does not interrupt it.

[ADR-0088](adr/0088-native-directories-and-direct-vm-images.md) supersedes the
journal-clone format in [ADR-0058](adr/0058-prepared-native-project-bases.md).
A durable environment-qualified initialization pin keeps the verified source
through copying, rename and format publication. Interrupted copies retry their
private partial tree. Renamed data is verified without overwriting it. A committed
format wins over a leftover pin, preserving later user changes. Environment
deletion releases unfinished pins and private data; cache reclamation follows
its existing limits. Existing conversations retain their own files.
The historical `metadataBytes` response now estimates the complete private
copy, including per-entry/logical allocation and reserve, rather than only journal
metadata or compressed cache allocation. Large templates use the bounded cache
payload limit; a cache on a compressed controller filesystem must not understate
admission on the uncompressed data pool.

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

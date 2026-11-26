# Prepared native project bases

Prepared bases extend the [project ZIP cache](PROJECT-ARCHIVES.md) with verified
plain directory templates. A fresh environment gets its own private file copy,
then publishes its native directory format before execution. It avoids fetching
and decompressing another ZIP; it still creates private files and verifies their
contents. File ownership, links and xattrs are preserved. The cache shares no
mutable working-file inodes with conversations.

## Layout

[ADR-0092](adr/0092-instant-native-environment-start.md) places base contents on
the data pool, beside the environments cloned from them:

```text
state/native-data/.cairn-bases/<key>/            contents (home, output, workspaces)
state/native-data/.cairn-bases/.build-<uuid>/    contents being extracted
state/native-bases/<key>/{pin,contents.digest,ready.json}
```

`.cairn-bases` is a root-owned `0700` directory. Nothing below it is ever bound
into a workload; an environment only receives a private clone under its own run.
Control records stay on the controller filesystem, so attempt pins and durable
initialization intents remain hard links to `pin`. `ready.json` (format 3) binds
the record to the contents directory's inode and records the contents' allocated
bytes and clone footprint, both measured once at publication.

A build extracts into `.build-<uuid>` on the pool, so admission, inode and
physical reserve checks sample the pool. Its control records are sealed in a
private `state/environments/<uuid>` directory. Publication renames the contents
into place and fsyncs `.cairn-bases`, then renames the control record and fsyncs
`native-bases`; a base exists only when both halves agree. Trimming unpublishes
the control record durably before removing the contents. Trimming and cache
budgets add the recorded sizes instead of walking cached trees, and full usage
scans count the contents through the pool's `statvfs`, never twice.

Controller startup, after attempt pins are recovered and before plans are
admitted, removes interrupted builds and probes, contents without a control
record, and unpinned control records without matching contents. Unpinned legacy
records, with inline `contents/` or an older `ready.json` format, are removed as
well; pinned ones remain until a later trim. Older keys never match format 3.

## Verification

A base is hashed in full when it is published. A controller process trusts it
afterwards: the first lookup after a restart hashes it again, and later lookups
check its records and contents directory identity only. A failed verification
removes `ready.json`. Lookups therefore return the recorded clone footprint
without walking the base.

## Clone

The controller probes once per process whether the pool supports `FICLONE`. If
it does, `cp --reflink=always` clones the base: data extents are shared, only
metadata is written, and mode/ACLs, ownership, timestamps, hard links and xattrs
are preserved. A structural `rsync -aHAXn` dry run then verifies names, types,
sizes, times, modes, owners, links, ACLs and xattrs; data needs no checksum
because the clone's extents are the source's. Otherwise, as on ext4 pools, rsync
copies every byte and `rsync -c` verifies it. Both run in a monitored process
group without `CAP_SYS_RESOURCE`. Legacy journal migration copies from another
filesystem and always uses the checksummed copy.

The copy does not fsync each file. After verification, one `syncfs` of the data
pool flushes the copy; it is renamed and its format published only after that
barrier succeeds. The barrier's descriptor is opened before the copy
starts, so a writeback error during the copy fails it even when another
environment's `syncfs` observed that error first. The barrier then fsyncs the
target directory: ext4 refuses that fsync after a shutdown or an emergency
read-only remount during the flush, which `syncfs` and `statvfs` may not report.
A crash before the rename leaves a partial private tree that initialization
deletes and copies again from the pinned source.

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
The historical `metadataBytes` response estimates the complete private copy,
including per-entry logical allocation and reserve. For reflink, the base also
records a separate physical estimate based on directory entries and data extent
counts. Both estimates are measured at publication. XFS accounts the shared
blocks against the conversation's project quota, while node-wide reservations
use the physical clone estimate.

## Reproducible measurements

Use a disposable Linux host with root, XFS and ext4 project quotas, and the
pinned native build dependencies. The launcher creates private ext4 and XFS
filesystems for the actual quota, base and identity admission cases:

```sh
CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 \
  CAIRN_NATIVE_DISPOSABLE_HOST=1 node scripts/ci/qualify-native-quota.mjs
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
construction is measured separately. The XFS run also samples quota-owned
prepared claims, with a regression check that a claim performs zero full usage
scans. Output includes encoded ZIP bytes, allocated base/private bytes, wall
time and process plus reaped-child CPU (including rsync), identical advisory
cold/warm 4 MiB file reads and a synchronized new 4 KiB file.

This is local filesystem preparation and IO, not production network latency.
Pool-wide `syncfs` can include other writeback; the cases run serially on their
owned filesystem. `POSIX_FADV_DONTNEED` is advisory, not proof of cold hardware.
Actual execution, Docker, controller crash recovery and expiration are separately
qualified by the full disposable native-runtime fixture. The local legacy-journal
versus direct-file block comparison is documented in [journal qualification](JOURNAL-RECOVERY.md#qualification).

The same native-runtime workflow drives the production connector and controller
from a loopback fixture manager on both candidate XFS and the pinned ext4
reference. It runs three cold and warm skill bundles per runtime, plus a warm
launch from a prepared base. The XFS fixture verifies the skill cache hit and
the claimed spare's original inode during that same launch. Each row in
`connector-startup.json` reports the encoded seed bytes, cache negotiation,
connector completion and the workload readiness marker. The response frames
are prebuilt. The timed request executes the real manager-side skill digest
and allocation estimate, with that time also reported separately. Its scan count is
process-wide inside the controller's admission-to-submission interval. It excludes
the connector's own scans. The fixture holds the seed payload fixed and creates
fresh conversations. The loopback timings exclude the production manager's
authorization and network costs, so production observations still determine
the message-to-launch target.

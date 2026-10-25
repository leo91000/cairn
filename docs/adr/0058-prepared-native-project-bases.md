# Prepared native project bases use sealed journal segments

The [project archive cache](../PROJECT-ARCHIVES.md) removes repeated network
transfer and per-file synchronization. Expanding and taking ownership of the
same repository still creates thousands of inodes for every new conversation.

Build a disposable local native base in the background, keyed by sorted project
IDs and archive checksums, virtual disk size, workload UID/GID and format version.
Only complete authorized project archives enter this cache. Homes, credentials,
skills and conversation edits never enter a base. Generate flat ext4 offline with
`mkfs.ext4 -d`, set ownership once, initialize inode/journal tables eagerly and
import the sparse image through the existing local journal bootstrap. Seal the
prefix and close its SQLite connections before publishing readiness.

A new conversation copies private SQLite metadata and hardlinks only sealed
payload segments. Existing journal generations already provide copy on write:
later writes append to a new segment; publication and reclamation unlink that
conversation's names. They never mutate the shared sealed prefix. Reflink can
copy the small metadata files when supported, with an ordinary copy on ext4.
Complete disk/accounting/publication formats stay unchanged.

## Ownership and interrupted preparation

The cached base is anonymous and has no remote authority. Prepare each clone in
a disposable unclaimed environment and bind only its private metadata to the
current plan's validated storage context. Under the existing logical ownership
and physical disk locks, synchronize its runtime identity, atomically rename the
finished journal into the conventional `disks/<run>/lazy` directory, then sync its
parent. That rename is the commit point. The controller opens the authorized
journal and initializes from its `.cairn-seed-complete` marker. A missing
ownership hint only causes the existing one-time reconciliation of file owners.
Native startup reclaims abandoned temporary environments after fencing old
workloads, before admitting new preparation.

Existing conversations always retain their own journals. A cache refresh, changed
remote, different UID, another disk size or cache eviction never replaces their
files.

## Admission, integrity and warming

Native connectors prepare project bases. Under the shared storage lock the connector
pins a complete matching base before omitting the ZIP download and full expanded
seed reservation. The controller independently derives the same key. A miss
uses the ZIP path. Combinations outside optional
base-cache limits also remain ordinary ZIP misses; foreground misses enqueue
their exact project combination and disk size for later conversations. Catalog
warming prepares individual projects at the normal 32 GiB size.

The cache uses at most one eighth of the configured pool, capped at 64 GiB, and
reserves simultaneous temporary extraction/image/journal space before building.
A metadata-only ZIP preflight includes one host allocation unit and inode/entry
overhead per entry, including explicit directories, instead of assuming logical
file lengths bound tiny-file allocation. Extraction still verifies the entire ZIP
checksum before writing; changed or corrupt preflight metadata cannot admit an
under-reserved extraction. Pool pressure
reclaims unused bases as well as ZIPs. Pins and source-reader locks fence
eviction; accounting counts shared inodes once. Pins belong to an attempt, so an
old cancellation cannot unpin its replacement. Failed preparation drops its pin;
startup clears interrupted pins after fencing old attempts and before admission. Cache recency is disposable
metadata. Removing a base cannot remove a child's remaining payload links.

The builder verifies ZIP bytes and writes checksummed journal frames. A bounded
process-local receipt records the resulting file identities, sizes, modes and
modification times. Changed files or a controller restart require full sealed
prefix verification before a base is offered; invalid readiness is discarded.
Receipts hold no file descriptors or payload buffers. Mounted volumes retain
their normal full journal-open and per-read integrity checks. Verification first
proves all payloads are sealed in a consistent SQLite snapshot, so recovery can
never truncate a shared payload as an interrupted append tail.

Warming has a bounded deduplicated queue and follows the failure backoff in
[ADR-0031](0031-runtime-cache-and-warmup-failures.md): 10, 20, 40, 80, 160 seconds,
then an hour after six failures. Cancellation does not count as failure; a
foreground request can retry after replacing its archive. Foreground intent
signals cancellation at the existing 10 ms polling interval. Extraction,
ownership, sparse indexing and journal copying cooperate; formatting is killed
and waited for. An owning blocking task retains the mutation lock and temporary
directory until nested I/O has ended. Waiting warmers never hold foreground
intent or block lease renewal.

Native stop/recovery also thaws an owned frozen unit before asking systemd to
stop it. systemd rejects stop jobs on frozen units; lease/storage protection must
not prevent the controller from fencing a workload during maintenance.

Measurements and their host/runtime limitations belong in
[native base measurements](../NATIVE-BASES.md).

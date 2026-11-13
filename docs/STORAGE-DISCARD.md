# Durable guest discard

Cairn propagates guest ext4 FITRIM through native virtio-blk Async and managed ublk.
The guest filesystem decides which extents are free. The controller never mounts
the guest filesystem to infer free extents from a partial cache or overwrite
allocated files.

## Storage and durability

Discard reads as zeros after its durable acknowledgement. Each range uses normal
checksummed `CAIRNJ03` write frames, including the ordinary zero payload checksum.
Zero payloads extend the append file sparsely instead of writing their data pages.
Headers, frame lengths, sequence numbers, group commit, segment rotation, crash
recovery retain their existing contracts. Readers see precisely the same bytes
as an ordinary zero write. No payload is punched out of an earlier frame. Local
consolidation reclaims obsolete journal segments only after their replacement
base is durable, as specified by [ADR-0083](adr/0083-local-conversation-disks-without-s3.md).

The complete range is checked before any mutation. A large operation can commit
a prefix before returning an error, like the existing write batch interface;
it is not an atomic transaction. Invalid requests cannot cross the system and
workspace views. Persistence failures are errors, never successful discards.
A no-op discard also drains staged writes before acknowledging their zero hint.

The journal indexes authenticated zero frames in memory. Already zero ranges and
the sparse tail need no new frames. Discarding damaged base bytes is allowed;
reading a damaged surviving neighbour still fails. Local consolidation must
obtain and verify every surviving byte of a partially discarded block.
The zero hint is reconstructed during existing journal authentication at reopen;
it adds no serialized field or unauthenticated shortcut to payload reads.

## Transports and latency

The pinned Firecracker 1.17.0 source supports discard only with its Sync engine
upstream. Cairn adds asynchronous `IORING_OP_FALLOCATE` to the existing restricted
ring and registered file descriptors, using mode 3 (`PUNCH_HOLE | KEEP_SIZE`).
Regular files punch a hole; Linux block devices issue WRITE_ZEROES, which Cairn
ublk translates to the same durable authenticated zero ranges as DISCARD.
The reserved `NO_HIDE_STALE` flag is rejected by
[vfs_fallocate](https://github.com/torvalds/linux/blob/v6.8/fs/open.c#L252)
before it reaches the block implementation, so it cannot select block discard.
No userspace worker or additional seccomp syscall is needed.

Discard starts after prior completion receipts are consumed and fences subsequent
read/write/flush requests until its own receipt is consumed, including on error.
The VMM control thread never waits for the backing discard: local disk reads
and dirty-page writeback can block the kernel operation
without preventing a storage-pressure pause. Both the native frontend and ublk
advertise a maximum 8 MiB request to bound each maintenance ordering boundary.
Disk draining retains its I/O completion barriers.

The ublk-only transport decision is recorded in
[ADR-0055](adr/0055-ublk-only-vm-storage.md). The ublk backend handles payload-free DISCARD and WRITE_ZEROES separately from the 512 KiB
read/write buffer, with the same 8 MiB maximum and durability barrier. The guest
virtio frontend advertises only discard. Volume space admission, faults and journal
accounting remain conservative, including while trim is running. Discard
admission returns a retryable failure immediately when headroom is insufficient;
it does not wait for space or force a storage-pressure pause. Verified no-op
ranges bypass write admission and budget debits while retaining their durability
barrier, so a large sparse tail cannot exhaust synthetic quota before reaching
real deleted bytes. Backend errors remain visible in discard counters.
FITRIM can return success after a backend I/O error, so a rejected range may
wait until the next normal 30-minute pass. The helper retries after 60 seconds
only when it observes a tool failure, timeout or cancellation.

Background trim consumes CPU and durable journal barriers and can temporarily
increase foreground latency. The new `performance.discard` counters expose count,
logical bytes, elapsed time, errors and concurrency. Fully freed regions avoid
future base-block reads; partial regions still use the normal 4 MiB local read/cache
path. See [local measurement guidance](JOURNAL-RECOVERY.md#qualification).

## Existing conversations and deployment

Retained conversation images deliberately keep their existing guest bridge and
tools. Updating only that image's node backend would not schedule guest FITRIM.
Each boot therefore attaches a small, read-only SquashFS control disk containing
the current init, a standard-library Python maintenance script and static BusyBox.
The bootstrap mounts it before the overlay pivot. The retained guest bridge still
runs from its original root image; maintenance runs from the immutable control
disk. The control disk is local to the node and separate from the conversation disk. The guest kernel already enables SquashFS/XZ; the build asserts it.

The conversation filesystem stays on `/dev/vdb`;
the read-only control disk uses `/dev/vdc`. The root image remains read-only. An interrupted
node asset copy retries its private staging file and installs only synced bytes.

The root helper starts after the mounts and acknowledges readiness before the
bridge starts. Its first pass follows a 10-second startup delay. It checks for
newly mounted targets once per second and trims each mounted ext4 view every
30 minutes, with a 64 KiB minimum free extent. An observed tool failure retries
after a minute. Ext4 may hide backend discard errors from FITRIM, so its successful
exit does not certify that all free extents were reclaimed.
Each child has a 120-second userspace deadline and an owned process group;
shutdown/timeout terminates and reaps it. Kernel I/O stuck in uninterruptible sleep
still requires the existing VM teardown path. Filesystem thaw never waits on the
helper's lock. Logs contain only fixed mount names, `invocationSucceeded` and
elapsed time. This flag records successful tool invocation; backend counters and
errors establish actual discard execution. Hidden backend failures are retried
on the next normal pass, and do not invalidate previously acknowledged writes.

Ship the full node image, including patched Firecracker, control disk and backend.
The normal node restart performs the accepted cold restart of running guests.
Existing conversations gain maintenance on their next cold boot, including when
resumed from their local durable disk. Consolidation reclaims obsolete local
segments after the trimmed data becomes part of the durable base.

## Reproduction and evidence

Native Firecracker regression tests require working host io_uring, rather than
silently skipping it in a container build sandbox:

```sh
rustup toolchain install 1.97.0 --profile minimal
scripts/test-firecracker-discard.sh
```

Disposable retained-image boot test on Debian with KVM, Python 3.11,
busybox-static, e2fsprogs and squashfs-tools:

```sh
python3 tests/storage-discard-bootstrap.py \
  --firecracker /path/to/qualified/firecracker --kernel /path/to/pinned/vmlinux \
  --output /path/to/bootstrap-reference.json
```

This boots a deliberately old bridge with a flat disk, compares with
maintenance disabled, deletes controlled files, verifies live files after FITRIM
and checks that the retained root image never changed. It uses file-backed Async
drives, so it does not substitute for ublk or production-network qualification.
`--no-seccomp` exists only for a local GNU Firecracker build; report that limitation
when used. Production qualification must use the image's musl binary and policy.

Focused native tests cover process-kill recovery, sync failures, no-op durability,
missing bases, authenticated sparse payloads, sealed/consolidation boundaries,
request ordering and bounds, view isolation and late workspace mount scheduling.
The normal full Quality/container workflow and the native Firecracker discard
workflow are both required for this change's review evidence. Reproducible
measurement JSON and host conditions belong with the PR/release reference;
controlled gains are not a forecast for production workloads.


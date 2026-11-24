# Instant native environment start

Date: 2026-11-23. Supersedes the storage substrate, base cache placement, ownership
translation and global storage gate of
[ADR-0088](0088-native-directories-and-direct-vm-images.md) and
[ADR-0087](0087-isolated-native-disk-cleanup.md) for native executions. Their
retention, expiration, quarantine, reserve and cleanup contracts are unchanged.

## Context

A fresh native conversation spent about a minute before its workload launched on
a production node (Linux 7.2, NVMe, loop-backed ext4 pool):

| Step | Cost | Cause |
| --- | --- | --- |
| Node seed staging | 23 s | full usage scans, per-file fsync, global lock held over the transfer |
| Base clone | 7 s (118 s before #807) | file-by-file copy, checksum verification, pool `syncfs` |
| Ownership translation | 9 s | 5,222 `lchown`s journaled in 164 batches, one pool `syncfs` each |
| Pool accounting and four other phases | ~20 s | 8–9 full usage scans of ~2.5 s each |
| Storage gate waits | 4–16 s | one node-wide lock over copies, transfers and scans |

Each usage scan walked 1.64 M entries of extracted base caches stored on the
controller filesystem. None of this work is inherent to starting an isolated
environment: a process on a developer machine starts in an existing directory in
one or two seconds.

## Decision

A ready native environment starts with a claim, a mount and a process launch.
An unprepared environment clones its base on demand. Reflink avoids duplicating
the data blocks, with metadata work proportional to the number of files.

### Storage substrate

The native data pool is XFS with `reflink=1` and enforced project quotas, on a
partition, logical volume or direct-I/O loop device. ext4 pools remain supported
with a copying fallback, so existing nodes keep working until migrated.
`native-host.py migrate-pool` converts an ext4 pool offline: it copies every
retained environment with its project ID, limits and ownership, verifies the
copy, then switches the mount. The base cache is rebuilt, not migrated.

### Base cache on the pool

Extracted base contents live on the pool under `native-data/.cairn-bases/<key>`;
their control records (`pin`, `ready.json`, digest) stay on the controller
filesystem so pins keep working as hard links. An environment is created with a
reflink clone of the base: data blocks are shared and only metadata is written.
A base is hashed in full when it is published and verified once per controller
lifetime; later lookups trust the sealed, root-only tree. Its allocated size is
recorded at publication, so cache trimming and usage accounting never walk it.

### Ownership through idmapped mounts

On a reflink-capable XFS pool, protected environments store a root ID, a
template engine ID and a subordinate range in their retained ownership marker.
An idmapped source presents those three roles as the conversation's current
host IDs. systemd takes private binds from that source and the controller
detaches the temporary alias after launch admission. Fresh base clones already
carry the backing engine owner. Protected moves keep the stored mapping even
when the new node allocates different host IDs.

Existing protected environments reuse their recorded backing IDs. Their
original backing map keeps unrelated host IDs outside the mounted namespace.
The journaled translation handles changes between protected and administrator
ownership. Administrator execution keeps host ownership, including a separate
Docker store. ext4 and hosts without usable idmapped mounts use the existing
host-owner path.

### Accounting

The dedicated pool's allocated bytes come from `statvfs`. Conversation usage
and preparation writes use project quota counters. When the physical reserve
limits admission, the controller can retire unused spares and evict unpinned
bases before checking free space again. Controller-side files still need inventory,
and one admission decision reuses its sample until reclamation changes usage.
A bootstrap counter records process-wide usage scans, including concurrent
activity, so tests and production observations can detect regressions.

### Durability where it buys something

The initial native seed is received in a temporary directory and has a single
writeback barrier before publication. A prepared tree is published after its
verified clone is durable. Identity mapping of an already prepared base updates
only the ownership marker. Retained ownership conversions keep their replay
journal and batch barriers because their contents can include unique work.

### Locking

The connector reserves the initial seed's bounded allocation before releasing
the common storage lock and receiving the body. Older peers that omit the bound
use serialized admission. The controller publishes the base pin before
releasing the storage lock for an on-demand clone. A per-environment owner
fences its preparation and launch. The remaining shared locks cover capacity,
cache mutation and the cold project-archive path. The background warmer
continues to yield to foreground waiters.

### Seed cache

The manager authorizes each request against its attempt. Native nodes can offer
hashes of up to eight previously verified skill bundles. The manager omits the
skill tree only after hashing the authorized source, and the node verifies the
recreated tree before staging it. Configuration, the inbox and credentials
continue to use attempt-scoped transfers.

### Prepared environments

Like the Firecracker ready pool of [ADR-0019](0019-booted-vms-with-ready-codex.md),
the controller keeps up to two anonymous XFS environments, at most one per
recently warmed base. They are cloned, verified and quota-limited. They contain
no account, credentials or conversation data. The first attempt claims an
environment by renaming its quota-owned root and transferring the ownership
record. The intent and the original project/inode identity allow replay after
interruption. Unclaimed environments are retired under pressure, on budget
changes or at restart.

## Consequences

- The target is under 2 seconds at the median and under 5 seconds at the
  95th percentile from sending a message to agent launch. The native storage
  qualification measures clone and claim costs and checks that the claim has no
  full usage scan. Production observations include transport, provisioning and
  coding-engine startup.
- The data pool must be migrated once, with the node stopped.
- Reflink clones charge their logical size to the environment quota while sharing
  physical blocks; physical reserve checks use the pool's real free space.
- Migrated projects keep their original inode allowance at the same byte grant.
  Grant changes account for the XFS geometry while preserving that allowance
  and inodes already in use.
- Idmapped mounts require Linux 5.12 or later on the pool filesystem; the node
  already requires 6.11.

## Alternatives considered

- **ZFS or btrfs snapshots** give O(1) clones regardless of file count, but ZFS is
  out of tree and btrfs quota groups are slow and semantically different from the
  project quotas the controller already enforces.
- **Creating conversations speculatively from the clients** needs client and
  manager changes and leaves orphan conversations; a node-side anonymous pool
  reaches the same start time without either.

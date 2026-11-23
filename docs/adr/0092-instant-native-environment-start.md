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

Starting a native environment costs a claim, a mount and a process launch. Work
proportional to the project size happens once per base, off the start path.

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

Files are stored with namespace IDs (namespace root 0, engine 1000, volume users
in the subordinate range), never with host IDs. The controller creates a detached
idmapped mount of the environment's data with the conversation's user-namespace
mapping and binds it into the workload. A fresh clone therefore needs no `chown`,
and moving an environment to another identity or node changes only the mapping.
Existing environments are converted once from host IDs to namespace IDs with the
journaled translation, which remains the recovery path for interrupted
conversions. Administrator Docker data keeps host ownership and is bound
separately, without an idmap.

### Accounting

Pool usage comes from `statvfs` and project quota counters, not from walking
trees. A bootstrap performs at most one full inventory of the remaining
controller-side state. A bootstrap counter records every usage scan so tests and
production observations can detect regressions.

### Durability where it buys something

Nothing created during preparation is unique until the workload writes: an
interrupted preparation is redone. Staging receives and identity work use a single
barrier before publication instead of per-file or per-batch flushes. The clone is
published after one barrier, as before.

### Locking

The node-wide storage lock becomes three short critical sections that are never
held across copies, transfers, `syncfs` or calls to the other process: capacity
reservation, base-cache pin and eviction, and project-archive cache mutation.
Long work runs under a per-environment preparation lock. Every writer that
allocates pool or staging space, including connector staging, archive downloads
and base builds, holds a durable reservation; reservations of dead owners are
reaped.

### Prepared environments

Like the Firecracker ready pool of [ADR-0019](0019-booted-vms-with-ready-codex.md),
the controller keeps a small bounded set of anonymous environments per popular
base: cloned, quota-limited and idmap-ready, with no account, credentials or
conversation data. The first attempt of a conversation claims one by renaming it
into its run and writing its owner record; an unclaimed one is retired under
pressure, on budget changes or at restart. Live work always preempts preparation.
The manager and the clients are unchanged.

## Consequences

- A fresh conversation's environment is ready in well under a second when a
  prepared environment exists, and in about a second otherwise; the remaining
  start latency is the coding agent itself.
- The data pool must be migrated once, with the node stopped.
- Reflink clones charge their logical size to the environment quota while sharing
  physical blocks; physical reserve checks use the pool's real free space.
- Idmapped mounts require Linux 5.12 or later on the pool filesystem; the node
  already requires 6.11.

## Alternatives considered

- **ZFS or btrfs snapshots** give O(1) clones regardless of file count, but ZFS is
  out of tree and btrfs quota groups are slow and semantically different from the
  project quotas the controller already enforces.
- **Creating conversations speculatively from the clients** needs client and
  manager changes and leaves orphan conversations; a node-side anonymous pool
  reaches the same start time without either.

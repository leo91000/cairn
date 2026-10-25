> Statut : La mention des disques paired est obsolète : ce layout non déployé est supprimé. L’agrandissement natif reste applicable.

# Automatic bounded native disk growth

Date: 2026-10-24.

A native conversation has an ext4 workspace and a separate managed `/tmp`. Both
count toward its combined allowance. A large build followed by a temporary archive
or SDK clone can exceed the initial 32 GiB while the node still has shared space.
The agent must see this pressure before the controller's existing protective stop.

Native nodes that advertise `diskGrowth.protocol = 1` can request growth internally
through their authenticated heartbeat. The coding agent needs no new tool and gains
no permission to allocate storage. At 80% combined usage (or workspace capacity),
the controller asks the manager for another 8 GiB. It waits until the previous
allowance has reached ext4 before asking again. At 90% it emits another notice.
Recovery notices wait at least one minute to avoid threshold oscillation; increases
in pressure, grants and capacity decisions are delivered immediately. Notices also
describe denied capacity, successful grants, and installations that
cannot negotiate growth. They enter each coding agent's steering channel as runtime
information, never as user authorization or a delivered user message. Generic status
events and `run.diskCapacity` carry the same information to existing clients.

The manager grants space in the same serialized database transaction used for
admission. It counts measured local usage plus every unreleased attempt's unused
allowance. Only exclusively owned local payloads, on the node's accounting basis,
reduce that attempt's unused promise. Workspace credit includes only decoded cached
objects referenced by the current published base, bounded by live workspace usage.
Journal frames and metadata receive no credit because retained append history is
not necessarily current backing. Temporary-file credit excludes unallocated sparse holes. These conservative credits also protect
future host allocations. Shared prepared-base segments get no duplicate credit.
A fenced native mount records its logical usage beside its journal. Fresh health
observations measure that stopped journal's current exclusive cache backing, bounded by the
recorded usage and directory identity. Same-node admission reuses this credit;
restored/rebuilt journals invalidate it. Missing observations remain conservative.
Older clean caches can be reclaimed while their conversation queues for resumption.
The node pool retains 5%, capped at 10 GiB, and the host filesystem retains
its configured storage-policy reserve. Missing or older-than-30-second measurements
block further admission and growth. Negotiation is sticky so a missing report cannot
silently discard existing promises. A lost grant reply returns the already recorded
allowance rather than adding another increment. The grant is persisted on the
attempt and run before the reply; an expired lease alone never frees the promise.

New native conversations retain a 32 GiB default. On smaller pools their initial
allowance leaves the pool reserve and 128 MiB journal preparation margin. This
partially supersedes [shared node budgets](0012-shared-node-budgets.md): CPU and RAM
remain pooled; negotiated native disk admission now protects unused allowances.
It reduces overcommit in exchange for fewer admitted simultaneous conversations.
There is no new per-conversation sizing control or billing reservation.

A native flat journal appends zero extents without downloading or rewriting its
base. Growth takes the existing write/publication locks and waits for sealed older
generations to be acknowledged. It commits the new manifest geometry durably, then
updates its owned live ublk device and runs online `resize2fs`. The committed journal
size is the recovery intent: an incomplete device/filesystem resize retries, including
on the next owned mount. No shrink is supported. Existing journal and publication
formats remain unchanged. The maximum remains 1 TiB and increments stop earlier
when shared capacity is unavailable. Resize runs outside the supervision loop;
lease, cancellation, node-budget and combined-allowance enforcement continue.

This supersedes the temporary-image growth restriction in
[mandatory S3 disks](0009-s3-backed-disks-required-on-every-node.md) **only for native flat disks** on
kernels advertising `UBLK_F_UPDATE_SIZE`. Firecracker paired disks retain their
existing behavior. Old managers/controllers and unsupported kernels keep their
existing allowance and hard stop. Already negotiated nodes with missing measurements
wait for current observations rather than reverting to legacy admission.

When admission or growth lacks disk headroom, the manager queues asynchronous
reclamation of the oldest eligible inactive local cache. It reuses all existing
publication, run-preparation, writer-release, grant and generation checks. Under
pressure the one-hour idle age is waived, and a queued continuation's old cache
can be reclaimed after its previous writer is positively released. Normal queued
recovery does not authorize deleting dirty or unpublished work. The controller still requires a
current acknowledged S3 generation and no dirty journal before eviction. One cache
is reclaimed before remeasurement. Conversation history and unpublished work are
never removed. If reclamation cannot establish safety or recover sufficient space,
the new conversation remains queued; active workloads retain their recorded allowance.

Qualification extends the disposable native runtime fixture with live growth,
unchanged workload PID, writes beyond the former device boundary, and rejected
shrink replies. Existing reference/native read/write/CPU and encoded S3 measurements
are retained. These are synthetic loopback-S3 measurements, not production network
latency. See [storage measurements](../STORAGE-COMPRESSION.md).

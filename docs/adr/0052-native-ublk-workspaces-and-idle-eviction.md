> Statut : Remplacée par [ADR-0054](0054-explicit-conversation-backend-migration.md) pour l’interdiction de conversion native/VM ; le stockage ublk natif reste applicable.

# Native workspaces use the VM journal through host ublk

Native Linux execution keeps its host drivers and systemd process boundary. Its
workspace storage uses the same `Volume`, `LazyDisk`, authenticated S3 block source,
immutable publications, generation acknowledgements and clean cache as Firecracker.
The controller mounts one flat ext4 filesystem on a ublk device; no VM is involved.
Native checkpoints use `native-ext4-v1` and a native runtime identity. Backend and
runtime checks continue to prohibit implicit native/VM or cross-OS environment conversion.
[Explicit conversation migration](0054-explicit-conversation-backend-migration.md)
now converts Linux user files into a destination-compatible disk before replacing
the saved backend; ordinary node moves keep these checks.

## Ownership and shutdown

The native controller shares the system manager's mount namespace so systemd can
resolve stable bind sources under Cairn's root-private state directory. A source
through `/proc/<controller>/root` is insufficient: systemd canonicalizes it and
loses the controller's private mount view. Writable allow-list paths are explicitly
relative to the unit root. The ublk backend enters a separate private mount
namespace before adding the device. That namespace must never contain the
filesystem it serves:
[upstream describes a self-deadlock on namespace teardown](https://github.com/ublk-org/ublksrv#mount-namespace-caveat-containerpod).
It must also drop inherited copies of earlier native filesystems: otherwise
retiring an earlier device waits for a later backend's namespace and retains
the physical execution slots. Under the controller's global disk lock, the
native layer verifies each inherited mount against its private block attachment.
The backend then makes its new namespace private and non-lazily unmounts only
those copies before adding its own device. The controller's mounts and unrelated
filesystems remain intact; an uncertain mount prevents admission.
Journal and control threads retain the existing IO_FLUSHER requirement. The workload
gets directory bindings from Cairn's root-private state, never a block device
or a host mount capability. GPU sharing and the trust model are unchanged. Each native attempt activates the
same bounded synchronization window and atomic storage CPU decision as a VM. A
separate monitor confirms freeze/thaw of the entire systemd workload cgroup for
integrity faults, space pressure, unavailable storage and bounded backup lag.
Accounting/capture cannot block that monitor. Failed protection fences the workload;
the backend and publisher remain runnable while its CPUs are paused.

Physical ownership lasts through cgroup fencing, filesystem flush, non-lazy unmount,
completed generation sealing and device retirement. Failure retains ownership and
the local journal for retry. Kernel device numbers alone never authorize retirement;
ublk's existing boot, process and immutable token records remain authoritative.
Controller restart requests workload termination, verifies each retained ext4 mount
against its owned block attachment and journal, and stops only its nonce-verified
dead backend to release pending I/O before awaiting complete cgroup fencing. It unmounts without detach before deleting
the device and recovering interrupted journal state. Both disk leases remain
held through these kernel operations, including cancellation. Unknown,
stacked, busy or mismatched mounts prevent admission; recovery never unmounts an
unrelated filesystem or deletes a device by its numeric identity alone. A crash can lose unflushed kernel pages, as it can in
a VM; committed journal writes are never replaced with zeros or an initial checkout.

Periodic publication seals a crash-consistent journal prefix without pausing the
agent. Explicit filesystem capture freezes only through the durable seal and thaws
before reconstruction/upload. A delayed freeze retains a descriptor to that exact
filesystem; it cannot reopen an unmounted pathname and freeze the host filesystem.
Normal stopped completion flushes ext4 before sealing the completed prefix.
An explicitly aborted I/O path records only the durable crash prefix after fencing
and non-lazy unmount; it does not claim a successful filesystem flush. Publication
can then run asynchronously across a following turn under the stable disk grant.

Generated runtime configuration, credentials and frozen skills stay in per-attempt
staging and are bound into the workload. Their underlying disk mount targets contain
no generated secret bytes. Claude's broker atomically refreshes its credential
file at runtime: its secure-store directory is bound separately in transient
storage, using the CLI's existing `CLAUDE_SECURESTORAGE_CONFIG_DIR` override. The
full Claude configuration directory remains on the journal, including arbitrary
user files, settings, instructions, sessions and installed tools. A readonly
credential file bind would break atomic refresh. Provider sessions, user tools,
workspaces and output use
the journal. This does not detect secrets an authorized agent copies into arbitrary
files. `/tmp` remains an ephemeral directory outside the journal. Same-node resume
avoids recursively reading or changing every workspace inode; a different workload
UID on a destination may require a one-time ownership reconciliation of metadata.

## Idle eviction

After one idle hour, a manager sweep may reclaim immutable local cache files from a
terminal native conversation. It requires the latest execution's confirmed release,
no writer/admission, pending move/restore or publication, and the exact acknowledged
S3 generation. The controller independently checks its physical owner, current
attempt, grant, published generation and absence of dirty writes. Historical consumed
movement reservations do not become permanent writers merely because their old
records lack a release timestamp.

Eviction preserves the journal, current manifest, grant context, runtime and all
unpublished writes. Resumption opens the same disk and fetches missing blocks on
demand. It neither restores the current disk from a full native file archive nor
seeds the original project into an initialized disk again. The unchanged remote
connector still transfers the original checkout in attempt staging, as for VMs;
that transfer is separate from journal demand loading and remains a startup cost. Chat history, published artifacts and shared pinned engine bundles
are retained. The one-hour policy and live storage behavior apply equally to web
and Android; no new client setting is introduced.

## Compatibility and recovery

The reviewed installation data epoch advances from 6 to 7; the legacy tree remains
unchanged. Existing VM data is unchanged. Existing stopped native file workspaces
and `native-files-v1` S3 points remain readable. Migration copies durable file/session
contents into the new filesystem while retaining the original source until its new
S3 publication is acknowledged. Failed preparation/restore never discards that source
or silently selects a different checkpoint. Migration and filesystem growth can need
extra temporary local capacity; ordinary remounts use block demand loading.

Epoch-6 native binaries cannot interpret the new native disk state. Do not run them
against an installation that has advanced. Automatic rollout must stop at the epoch
boundary; a separately reviewed rollout provisions ublk/ext4 support and updates the
manager and native controllers. Rollback needs matching prepared data and binaries;
a forward fix is preferable after new native disks have been created. No release or
deployment is part of this change. Keep failed restore journals and ownership records
for a retry of the same recovery point; never remove locks or force/lazy unmount a
filesystem to bypass uncertain ownership.

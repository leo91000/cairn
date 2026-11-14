# Conversation storage operations

Conversation working disks live only on their execution node. The manager needs
no S3 bucket or credentials to launch a conversation. History, uploaded attachments
and published artifacts remain on the manager and have their own retention.
Losing or revoking a node loses the working files of its environments. Push commits
and export files that must survive that loss; machine backups are the operator's
responsibility.

## Retained native directories and VM images

[ADR-0085](adr/0085-native-directories-and-direct-vm-images.md) defines the current
formats. Native working files, home, private Docker data and managed temporary
files live in ordinary directories under `state/native-data/<run>/`. This is one
dedicated ext4 filesystem with project quotas, separate from controller metadata
in `state/native/<run>/`. Each environment receives a non-reused project identity,
a block limit and an inode limit based on the pool's actual geometry. No native
block device, per-environment filesystem mount or application journal is created.
The OS page cache is governed by the execution's memory cgroup. `memoryCacheMiB`
remains a compatibility setting for legacy journals; it does not add a cache layer
to the new directory/image paths.

VMs receive `data.ext4` directly through Firecracker's block interface. The image
size bounds its filesystem capacity. The VMM uses asynchronous file IO and discard;
there is no ublk or application journal between the VMM and the file. While a VMM
can write, its full image capacity is reserved against the node budget and
physical free-space admission. Sparse holes and guest discards reduce actual
allocation but cannot make that writable capacity available to a second VMM.
Status reports actual allocated bytes separately from those admission promises.
This conservative reservation can refuse an oversized sparse image even when
its current data would fit. Closing the confirmed VMM releases the unused promise,
while retained image bytes continue to count. Retained live VMMs keep their promise.

CPU, memory, process count and IO throughput use cgroup controls; cgroups do not
limit occupied storage bytes. Native execution has per-attempt read/write caps of
64 MiB/s and a shared 256 MiB/s cap against the data pool's backing device. Project
quotas enforce native allocated bytes and inodes for ordinary protected workloads.
An unrestricted administrator can change or bypass kernel limits; in particular
`CAP_SYS_RESOURCE` bypasses ext4 project hard limits. Controller pressure accounting
and cancellation remain cooperative in that mode. Controller receipts stay
writable if the separate data filesystem exhausts its inodes.

## Stop, failure and cleanup

Run completion fences the actual consumers before flushing. This includes private
rootful Docker containers in external systemd scopes when their private metadata
binds prove environment ownership. Shared device numbers or historical PIDs are
insufficient evidence. Unknown consumers, replaced cgroups and incomplete waits
remain explicit failures with ownership retained for recovery. The main systemd
unit becoming empty alone does not establish closure.

Normal stop removes execution temporary files, containers and ephemeral Docker
runtime state. It preserves the workspace, provider session files and private
Docker images, build cache and volumes needed by a retained environment. Same-node
resume reuses that work; it does not reseed an initialized environment from the
original checkout. Missing retained trees or images are refused explicitly.

After confirmed fencing, an ext4 EIO/read-only flush failure reports finalization
failure and preserves the acknowledged crash prefix. Unflushed pages are not
promised. VM shutdown waits for the exact child and its image flush; uncertain
teardown retains its leases and retries observably. A controller restart may
reconcile reservations only after the old execution namespace has been fenced.

Expiration/deletion removes the environment's directories, images, private Docker
data, temporary paths, old owned attachments and technical execution records.
Quota retirement persists its intent outside the data pool, waits for open-unlinked
bytes/inodes to disappear, then clears its kernel limits. A stale ownership record
cannot retire a replacement directory. Immutable retirement receipts and small
lock/ownership namespaces intentionally survive to reject delayed deletion requests
after a new environment starts. Shared runtime/base caches and the native-data
filesystem remain node resources under their existing cache/capacity rules.

## Local format conversion

Existing journal-backed environments use a compatibility reader during conversion.
Their source remains intact until verified replacement data and a format marker
are durable. Native conversion copies files with ownership, links and xattrs,
verifies them with a checksum pass and respects the destination quota. VM conversion
exports the stopped logical disk, compares every byte and checks ext4 before
publishing its direct image. Interrupted phases resume; a committed replacement
wins over partly removed legacy files, whose cleanup is retried.

Unknown formats, missing source data and mismatched ownership fail without making
a new blank environment. Allow room for both the retained source and its temporary
replacement. If space is insufficient, increase available capacity or expire the
environment explicitly under its existing policy. No remote fetch is attempted.
A rollback to the old format needs an operator backup. This implementation's
qualification does not authorize a production migration or deployment.

## Expiration, deletion and upgrades

See [conversation retention](CONVERSATION-RETENTION.md) for inactivity expiration.
Expiration detaches the old environment immediately and removes its disk in the
background. History, attachments and artifacts remain. Cleanup on an unavailable
node retries; a revoked node's disk is forgotten. The next environment on the same
node waits for the old writer to stop and its disk to be removed.

Deleting a conversation remains irreversible and revokes its artifact links. Cairn
removes the manager's conversation data and reachable node disks, but never deletes
remote disk objects, including previously queued deletions. The publication,
restore and remote collection workers no longer run.

Existing environments that depend on S3, and environments on already revoked
nodes, expire at the local-disk upgrade. Never-published environments on nodes that
have not been revoked remain usable. Pending moves, recoveries and backend
migrations are cancelled. No blocks are downloaded for migration.

The upgrade leaves the bucket, credentials, `storage-s3.json`, `STORAGE_S3_*` and
Garage data untouched. Operators may remove them once no rollback is contemplated.
A rollback requires an operator database backup taken **before** the upgrade and
the matching bucket; the supervisor does not take that backup. Restored files only
reach their last published generation. Continue to protect the application database
and `DATA_DIR/mcp-encryption-key` separately. Agent and engine subprocesses continue
to exclude `AWS_*`, `STORAGE_S3_*` and `ARCHIVE_*` variables.

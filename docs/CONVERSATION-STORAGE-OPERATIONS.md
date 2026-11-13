# Conversation storage operations

Conversation working disks live only on their execution node. The manager needs
no S3 bucket or credentials to launch a conversation. History, uploaded attachments
and published artifacts remain on the manager and have their own retention.
Losing or revoking a node loses the working files of its environments. Push commits
and export files that must survive that loss; machine backups are the operator's
responsibility.

## Local journal and consolidation

Both VM and native environments use a durable segmented journal and immutable
local base blocks under `lazy/cache/<hash>`. Despite the directory name, these
blocks are the disk itself: Cairn never evicts them to make room. A missing block
prevents mounting the disk; a corrupt block fails authentication when read. No
remote download replaces missing or damaged data.

Completed turns seal a durable generation after filesystem synchronization. The
node consolidates it in the background, outside guest I/O and turn completion.
While the environment is mounted, a five-second check also seals a crash-consistent
prefix once the journal reaches 1 GiB, or a quarter of the task allowance when
smaller. Journal space or task-allowance pressure also triggers this work.
Two background consolidations can run on a
node at once. Space failures retry after a minute; they never discard the journal.
Prepared environments remain journal-backed until assigned to a conversation.

Consolidation reconstructs changed 4 MiB blocks while newer writes continue in a
new generation. Each new block file is synchronized, followed by its directory,
before a SQLite transaction replaces the base and removes the old segment
references. Segment files and obsolete block files are removed afterwards. A
process crash before the transaction retains the previous base and journal; after
it, recovery removes orphan segments, and a later consolidation retires obsolete
block files. Newer writes remain owned throughout. This establishes process-crash
recovery, not a physical power-loss qualification.

The journal's `dirtyBytes` measures unconsolidated writes, not remote backup lag.
It returns to zero when all writes have been consolidated. Completed-turn demand
is drained after volume release, including overlapping seals and retries; status
observation never interrupts that work. Stopping a retained VM
releases RAM while preserving its disk, irrespective of this counter. Disk deletion
after expiration or explicit conversation deletion likewise needs no publication.

## Capacity and integrity

The node budget and physical free-space reserve cover the complete local
environment, including journal, base blocks, internal consolidation copies and
managed temporary files. VM and native task allowances count retained base blocks,
journal and managed `/tmp`. Native automatic growth stays bounded by that
allowance. Old sealed generations are consolidated before growth changes the disk geometry.
Consolidation needs temporary room for replacement blocks before retiring old
segments. Guest writes and consolidation both preserve the full configured node
reserve and respect the node budget. No other environment is evicted for admission
or growth.

Only the internal replacement copy is outside the task allowance. Its logical
size is bounded by the existing disk, and failed retries remove the previous
candidate before creating another. This grants no additional working-file or
`/tmp` capacity. At the base swap, the new base counts as retained data; the
temporary credit covers only the still-present files it replaces until they are
reclaimed. The copy remains fully charged to the shared node budget, including
its actual filesystem allocation. Each new block admission uses the current full
node reserve. If node headroom is insufficient, consolidation waits for space.

A full node waits for space. Consolidating overwritten journal history, expiring
environments and deleting conversations are the ways Cairn releases disk backing.
Disabling automatic expiration means an operator must arrange explicit cleanup.
Integrity and space protection continue to stop unsafe I/O; synchronization errors
and backup-lag states no longer describe local disks.

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

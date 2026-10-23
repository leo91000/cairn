# Existing conversations can explicitly migrate between Linux backends

Date: 2026-10-21.

Changing an agent's backend no longer requires discarding an existing conversation.
After configuring the agent and authorizing its destination node, the owner can
explicitly migrate that conversation from its execution-node details on web or
Android. The public MCP and built-in `cairn_installation` expose the same preview
and confirmed mutation through `get_chat_backend_migration` and
`migrate_chat_backend`. Management access and current destination grants are checked
before starting and again before committing. An explicit owner action adopts the
historical work into their native authority; delegated agents remain bounded by
parent provenance and live revocation. Membership or a profile edit alone does not
migrate any conversation. Other reduced project/tool permissions still prevent
reuse of an environment which had broader access.

Migration pauses the conversation, cancels only its in-flight sending inputs,
fences its execution, captures a coherent source
point and reads its verified encrypted S3 blocks. After fencing, source-only read
grants can hydrate cold baseline blocks even though the profile now grants only
the destination backend. They recheck the destination, retained access and native
principal/parent authority; they never authorize a workload on the source. Native
source authority is refreshed and checked again before the final commit. A userspace ext4 reader extracts
the private home, project workspace and output into a new filesystem. Both VM flat
and paired layouts are accepted; redirected home caches are included. It never
mounts the untrusted filesystem on the manager or follows its symlinks while writing.
The manager needs fakeroot and e2fsprogs (`mkfs.ext4 -d` directory population).
The userspace reader traverses the bounded circular journal across its physical
end and honors committed revocations, including earlier transaction mappings.
Later committed rewrites remain visible.
Cairn pins this correction in [the reader patch](../../vendor/ext4-view/CAIRN-PATCH.md).
The source image remains read-only, including when journal recovery is needed.
A fake numeric ownership view makes VM files belong to its fixed UID 1000;
native admission reconciles its own workload UID. Missing tools or unsupported
filesystems fail without replacing the source.

The destination restores and validates the resulting point before one transaction
changes the saved backend, node and checkpoint. The chat, transcript and files
remain; process state and operating-system package changes do not migrate. The next
message starts a fresh engine session using the existing transcript handoff.
Dependencies may need reinstalling. Pins and preferences are cleared. An active
message is stopped rather than replayed. Previously queued messages follow the
conversation's prior pause setting after success.

The source disk and recovery point remain until ordinary explicit deletion or
collection can safely reclaim them. Failed migrations stay paused and retain their
coordination record once a converted point exists. Before conversion, failures
clear admission guards so prerequisites can be repaired on the source. The queue
stays paused after a failed attempt until the user resumes it. Retry rechecks
authorization and reuses a converted point on the same destination, including after a manager restart or a lost restore reply.
The reservation ID is durable; retry releases an interrupted destination admission
without discarding its materialized disk allocation, then rechecks capacity.
Do not downgrade the manager while a migration is pending. Legacy native file
archives must first resume once on the original node to obtain block storage.
Conversion reads and rewrites the durable environment, so it needs temporary
manager capacity and is not an on-demand or constant-time operation. Sparse user
files retain holes while copying. Extraction rechecks free space, and admission
budgets the full logical destination image in addition to source/extraction space,
so filesystem creation cannot spend the manager reserve (from its local node
policy, or the default when it has no local node), regardless of a remote
destination’s policy.
The selected home, workspace and output roots must be actual directories; links
below these roots are preserved. This prevents a saved root link from becoming a
host bind source. Staging lives in a dedicated manager directory. Before another
conversion starts, both storage locks exclude live converters while abandoned
staging from a terminated manager is reclaimed, including on a retry.
Cleanup preserves the staging root so it can be a separate mounted filesystem;
it removes only that directory's abandoned contents without following symlinks.
Extracted files are closed before packing, without a separate fsync for every
temporary file. This tree is discarded after interruption; the durability
boundary remains the verified converted recovery point, published before the
original environment is replaced.

This supersedes the blanket native/VM conversion prohibition in
[native journal storage](0052-native-ublk-workspaces-and-idle-eviction.md).
Ordinary node moves retain their backend checks; only this explicit conversion
creates a destination-compatible image. Cross-OS conversion remains unsupported.

The installation data compatibility epoch remains **9**. No new persistent disk,
manifest or checkpoint encoding is introduced: the result uses existing VM flat
ext4 or `native-ext4-v1`, existing S3 objects and existing fresh-session handoff.
The additive migration coordination fields are ignored by old readers and retain
an unchanged source checkpoint until success. Downgrading during the operation is
unsupported because old schedulers do not understand its admission guard; finish
or retry it with the current manager first. Controllers need no new conversion API.
This change does not release or deploy either runtime.

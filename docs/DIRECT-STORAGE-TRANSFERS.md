# Direct node storage transfers

Ordinary conversation disk publications and cold reads transfer encrypted blocks
between execution nodes and the configured S3-compatible store. The manager owns
authorization, deduplication references and publication commits; it does not relay
these v3 payloads. Both Firecracker and native controllers use the same transfer
operation. [ADR-0076](adr/0076-direct-node-storage-transfers.md) records the owner’s
accepted trust, migration and failure policies.

## Publication and read protocol

The manager reserves missing objects durably before delegating a bounded batch of
four PUTs. It sends short-lived, exact-object presigned requests and a key derived
only for that object incarnation. The node reads its immutable captured generation,
checks its plaintext identity/size, compresses and encrypts it, then sends a
checksum-validated PUT directly to storage, with a signed create-only condition.
All started jobs are drained
before the node returns its receipt, even if the caller disconnects.

The manager checks the receipt against its exact object plan, settles concurrent
identical uploads onto their canonical incarnation, commits the authenticated
manifest and revalidates ownership. Only the final durable acknowledgement allows
the node to reclaim the published local journal prefix. Later writes remain owned
by their newer generation. A successful PUT alone does not release local data.

For a cold read the existing disk grant authorizes an exact manifest object. The
manager returns a GET capability and its scoped key; the node downloads,
authenticates, decompresses and checks the plaintext hash and length. Provider
failures are retried without falling back to manager payload transit. Missing or
corrupt objects are reported through that same disk grant and invalidated before
reuse; a failing read never becomes zero-filled data.

Detected corruption first enters a durable, identity-only outbox beside the local
journal. Failed or lost report responses remain queued across cancellation and
restart; reopening a disk replays them even without another remote read. Journal
acknowledgement serializes with queue insertion and refuses reclamation while a
report is pending. No capability URL, object key or credential is copied into
this outbox; authentication uses the disk's existing stable context.

The manager retains the digest of each issued read grant with the exact object,
plaintext hash and originating disk. This provenance authorizes only idempotent
quarantine of that incarnation, including after a grant is revoked or its point
is removed. It never renews GET, key, execution or publication rights. Quarantine
and baseline invalidation commit before the report is acknowledged; replacement
incarnations remain untouched. Historical identity provenance is retained after
payload collection so a lost acknowledgement can be reconciled. During a network
partition another node cannot know a newly detected fault until that report
arrives; this mechanism supplies reliable reconciliation, not instantaneous
installation-wide fencing.

Each node runs at most four uploads and four reads concurrently, with separate
budgets so waiting reads cannot starve uploads or deadlock replay verification.
Up to sixteen independent
publication operations can progress without sharing the old two-slot payload
bottleneck on the manager. The compatibility path retains its old transfer limit.
These are bounds, not promised throughput or latency.

Native backend threads have a private mount namespace. Remote read work is
scheduled on the host Tokio executor before it can create a blocking codec
worker. Polling it directly with `Handle::block_on` on the backend thread could
make the host's reusable blocking pool inherit that private namespace.

## Provider contract

The driver uses generic S3 SDK SigV4 presigning, not Cloudflare-specific tokens.
It qualifies signed-request destinations and PUT availability on a tiny disposable
control object before issuing the first data transfer for that bucket/endpoint. Missing capability or node
connectivity is a visible synchronization failure, with the local journal retained.
It never silently enables manager transit. Content checksums remain mandatory on
node PUTs. When supported, the provider additionally enforces atomic create-only
PUT. The bundled Garage image currently accepts repeated conditional PUTs with
`200`, so the protocol also supports S3 stores that ignore this condition. Under
the accepted trusted-supervisor model, every writer checks the exact immutable
plaintext hash, size and scoped incarnation before encoding. A replay may replace
the ciphertext representation with another authenticated representation of the
same content. Read-only nodes receive no PUT capability. This requires trusting
the node supervisor; it is not an untrusted-writer protocol.

Provider qualification and real restore testing remain necessary;
successful mock S3 tests are not a Cloudflare production benchmark.

URLs expire after 180 seconds and each node HTTP operation is bounded to 120
seconds. The manager records protection until the capability and its transfer
window end before issuing it. GET pins likewise extend the object’s collection
deadline. Complete signed URLs, object keys and temporary authorization material
must not enter diagnostics, subprocess arguments or guest environments. HTTPS is
required for remote endpoints; loopback HTTP is available to isolated fixtures.
The bundled `http://garage:3900` endpoint is also accepted by the colocated runner
on the installation's private Compose network, verified through its local
manager-origin marker. A separate node must use a reachable HTTPS endpoint.

## Keys, format and existing disks

The v3 object key is `shared-blocks/v3/{plaintext-hash}/{incarnation-uuid}`. A
domain-separated BLAKE3 KDF derives its AES-256-GCM key from the installation root
and that complete locator. The node receives this derived key, never the Vault
root key. The encrypted payload authenticates codec, plaintext size and locator
under the v3 domain. The outer header is `CAIRNB`, byte 3, byte 0, followed by a
12-byte random nonce and ordinary AES-GCM ciphertext/tag. Maximum representation
size remains plaintext block size plus 41 bytes.

Legacy v2 objects remain readable via their explicit compatibility path. Ordinary
new captures reserve v3 incarnations, so they do not accidentally reuse a v2 object
that needs the manager’s root key. One otherwise-idle legacy conversation at a
time can be converted in the background through the same capture/receipt protocol.
Active conversations convert on publication. No whole-store conversion blocks
startup. Existing manager-side VM/native conversion images use their explicit v2
conversion route, then convert when the destination node publishes its journal.
This is compatibility traffic, not an automatic response to a failed v3 transfer.

The manager database moves from version 12 to 13 with a small transactional grant
table migration. Disk payloads are not rewritten in that startup transaction.
Older binaries reject version 13; rollback requires restoring the corresponding
installation database and storage, not editing `user_version`.

## Ambiguous requests and collection

The provider validates each PUT’s transfer checksum and, where supported, its
creation condition. A
`412` result is accepted only after a direct download authenticates the existing
object and verifies its plaintext identity, size and codec. It is not evidence
of correct content by itself.

Ordinary settled grants stop protecting the object after expiry and its bounded
transfer window. An interrupted or failed batch can leave an earlier PUT with an
unknown outcome. Its unused objects retain durable tombstones: collection deletes
their payloads, retains their inventory and rechecks hourly. This covers a late
PUT recreating a retired key after a prior DELETE. At most one batch of four
objects per failed attempt is uncertain; prior successful batches are settled.
These tombstones deliberately remain while the provider outcome cannot be proved;
a negative HEAD or expired URL alone does not settle an already accepted PUT.

## Synchronization lag

`maxDirtySeconds` is now the threshold for reporting overdue publication. It does
not suspend CPU execution for upload lag. Web, Android and run API/MCP payloads
report the overdue state and the age/volume of data that exists only on its node.
Integrity faults, the free-space reserve and a required unavailable remote read
still protect the environment. The publication retry backoff remains unchanged.

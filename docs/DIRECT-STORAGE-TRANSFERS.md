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

One authenticated `direct-reader` control response owns the immutable journal
reader across those batches. Its body carries only small heartbeats; the manager
drains them while block payloads travel directly to S3. The node registry holds
weak references and admits at most 16 publication readers. A lost manager
connection releases an idle reader without a cache expiry timer; an already
started batch retains its reader until all PUTs drain. New batches require the
reader session and fail if it disconnected. An explicit close acknowledges only
after physical reader ownership is released, before the publication can commit.
This avoids replaying a stopped disk's entire journal for every four blocks.
Outbound relays acknowledge each reader heartbeat on their bounded control
requests; they do not buffer it in a long bulk upload awaiting TCP failure.

The opt-in `measure_stopped_direct_reader_recovery` test compares the former
four-block reader lifetime with the response-owned lifetime in one binary. The
local workload has 16 distinct 4 MiB blocks and 4,096 additional 4 KiB overwrite
frames. Three alternating measured pairs follow one warmup pair. On Linux with
Rust 1.97.1's unoptimized development profile, an i9-14900K, 94 GiB RAM and `/tmp`
on tmpfs, p50 results were:

| Reader lifetime | Journal recoveries | Plaintext read | Wall | Process CPU |
| --- | ---: | ---: | ---: | ---: |
| Per four-block batch | 4 | 64 MiB | 210.11 ms | 208.21 ms |
| Per publication response | 1 | 64 MiB | 121.29 ms | 108.80 ms |

Run `pnpm test:backend -p cairn-installation --lib measure_stopped_direct_reader_recovery -- --ignored --nocapture --test-threads=1`
alone. This measures reader recovery and reads with warm OS caches on a shared
host, excluding HTTP, S3, encoding and writes; it does not predict production
synchronization latency. The write/encoding format is unchanged by this lifetime
correction; the v2/v3 component comparison is recorded in the PR measurements.

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

Node-detected v3 corruption first enters a durable, identity-only outbox beside the
local journal. Failed or lost report responses remain queued across cancellation and
restart; reopening a disk replays them even without another remote read. Journal
acknowledgement serializes with queue insertion and refuses reclamation while a
report is pending. No capability URL, object key or credential is copied into
this outbox; authentication uses the disk's existing stable context.

Journal acknowledgement acquires the journal publication lock before either
integrity guard. It retries busy readers for at most five seconds and refuses
promptly when a corruption report is pending. It never holds the report mutex
while waiting for a journal reader, including a reader awaiting reconciliation.

Before sending an acknowledgement, the manager retains its publication identity,
node and disk in SQLite. Existing shared references pin its dependencies; no
second block inventory is copied. A successful node reply retires this intent.
A lost reply or timeout retains it, because the node's blocking commit may still
be running. Before another publication for that disk, the manager obtains a
durable node fence for the old publication identity. Fences serialize with the
actual journal commit, survive controller restart and disk replacement, and can
be installed even when the original disk has already been removed. At most one
unresolved acknowledgement inventory is admitted per disk.

Environment purge and expiry resolve this inventory before releasing shared
references. The node's durable publication fence survives disk deletion and
excludes a detached late ACK; an unavailable fence leaves retirement retryable
and its dependencies pinned. A permanently revoked node identity cannot become a
future placement, so its pending inventory can be retired without contacting it.
This permanent-revocation evidence also resolves ACKs during recovered
publication and quarantine, without depending on the revoked source controller.
Completed expiry therefore never requires the old node to publish a new
environment on another node.

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

Quarantine preparation serializes briefly with publication commit/ACK admission,
then releases that global gate before network I/O. A durable preparation marker
blocks reuse and commit of only the affected incarnation. Every ambiguous ACK
whose retained inventory references it is durably fenced on its node before the
incarnation becomes invalid and the report is acknowledged. This also excludes
late ACKs on other nodes and after manager restart. Unreachable fencing targets
leave reconciliation pending; they never authorize reclamation. Background
reconciliation resumes accepted preparations independently of the original
reporting disk. A durable round-robin cursor advances before each bounded retry,
so an unreachable fencing target cannot starve other pending quarantines.
Unrelated publication traffic can continue during fencing.
Manager-side v3 conversion reads use the same durable quarantine and ACK fencing
before invalidating a dependency or any affected baseline.

Each node runs at most four uploads and four reads concurrently, with separate
budgets so waiting reads cannot starve uploads or deadlock replay verification.
The encode/PUT budget is acquired after local production: cold base recovery
does not occupy it. Normal publications admit at most sixteen readers, each
with a four-block batch, bounding queued plaintext at 256 MiB per node in
addition to the four encoding/PUT buffers. Publication reads have a 120-second
production deadline and inherit control-response cancellation, including while
waiting behind a foreground read of the same cached hash. Cancellation drains
the blocking read before releasing ownership and does not stop the VM's source.
Already-started PUTs retain their existing bounded completion behavior.
Up to sixteen independent
publication operations can progress without sharing the old two-slot payload
bottleneck on the manager. The compatibility path retains its old transfer limit.
These are bounds, not promised throughput or latency.

[Transfer diagnostics](PUBLICATION-TRANSFER-METRICS.md) use the same fresh attempt
UUID on manager and node. On the direct path, node summaries measure each reader
open, local block production, hashing, compression, encryption, upload permit,
checksum and PUT; replay verification also measures GET and decode. A batch keeps
its summary alive until every started PUT drains. The manager's `nodeHeaders`
interval includes the node's complete batch upload before its receipt headers.
Cold base reads preserve the diagnostic context when switching to the host
executor. Per-stage wall times overlap and must not be added as a decomposition
of elapsed time.

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

URLs expire after 180 seconds and each node S3 HTTP operation is bounded to 120
seconds. The manager records protection until the capability and its transfer
window end before issuing it. GET pins likewise extend the object’s collection
deadline. Complete signed URLs, object keys and temporary authorization material
must not enter diagnostics, subprocess arguments or guest environments. HTTPS is
required for remote endpoints; loopback HTTP is available to isolated fixtures.
The metadata reader response has the existing stopped-journal startup allowance
of 30 minutes and a 60-second idle deadline. Tickets are issued after its reader
is ready, so journal recovery does not consume their authorization lifetime.
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

The manager database moves from version 12 to 13 with transactional coordination
metadata. Version 11 first completes its separate preserving inventory upgrade.
Disk payloads are not rewritten in either startup transaction. See the
[version 13 migration](migrations/0013-direct-node-storage.md) and rollout order.
Older binaries reject version 13; rollback requires restoring the corresponding
installation database and storage, not editing `user_version`.

Before upgrading an existing installation, pause scheduling and retain a
consistent database backup and the existing node state. Draining old nodes with
the old manager is supported. A manager-first upgrade also has an explicit
maintenance compatibility operation: authenticated controller health selects the
publication protocol before capture or reservation. A controller advertising
`directStorageProtocol: 1` uses v3. An older protocol-2 controller without that
advertisement receives a v2 capture and its existing journal ACK protocol,
including retries across manager restart. These v2 objects cannot be dependencies
of a direct-read quarantine and do not require the new publication-fence RPC.
Failed PUTs or ACKs retain local data and prevent maintenance readiness. Ordinary
captures keep using v3 and do not switch protocols after a transfer failure.
Keep scheduling paused until every active controller supports the direct
protocol; old controllers cannot consume v3 read tickets or direct upload plans.

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

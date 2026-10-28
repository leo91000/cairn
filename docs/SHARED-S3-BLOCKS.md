# Shared S3 disk blocks

Identical nonzero 4 MiB blocks are stored once across published disks in the same manager and S3 destination. This applies across projects as well as conversations and task runs within a project. Empty regions remain implicit; the immutable guest OS image remains separate.

## Publication and reads

`nodes/shared_blocks.rs` owns the inventory and durable reference edges. One SQLite transaction reserves every distinct manifest hash, reuses verified objects, and creates missing objects. Cached statements and indexed lookups keep work proportional to the candidate manifest, not the number of other conversations. There are no S3 HEAD requests to discover reuse. Reused blocks skip node transfer, encryption, PUT, and verification GET.

Object keys are `shared-blocks/v2/{plaintext-blake3}/{object-uuid}`, with the plaintext hash encoded as `b3-` followed by lowercase hexadecimal. Encryption retains random AES-GCM nonces and uses this complete key as authenticated associated data. The authenticated manifest carries each object's UUID. Read authorization still checks the disk grant and manifest; reading a shared object needs no inventory lookup or reference-count update. Size, authenticated ciphertext, and plaintext hash are checked on reads.

After all uploads pass verification, a transaction makes their objects reusable and reconciles concurrent first uploads of identical content onto the first ready incarnation. Both authenticated manifests then name that same object; the redundant upload becomes garbage. This preserves independent-disk synchronization without waiting for another publisher's network requests. A rare simultaneous first upload can transfer duplicate bytes temporarily; ordinary reuse transfers none. Pending publication references protect verified objects even before their manifests commit.

The ownership check, final verified-object check, backup record, and current-publication pointer commit atomically. An invalidated reused object causes publication to retry instead of committing known-bad data. Per-disk operation locks serialize that disk's captures, restore setup, and metadata retirement while independent disks retain the existing bounded concurrency. Missing node blocks use the batched transfer path in manifest order.

Cancellation of a capture caller leaves the owned publication task running until its uploads and bookkeeping finish. An ordinary transfer error drains already-started PUTs before releasing the publication lock. Process death leaves durable reservations and upload intents for recovery.

## Lifetime and cleanup

Current manifests, movement references, and mounted-disk grants retain publication references. Reader draining occurs only through metadata retirement. After the last reference disappears, an object becomes eligible for collection after five minutes. A new publication can reuse it during that grace period.

The independent cleanup worker claims at most 32 shared objects and 32 manifest/prefix jobs per pass, with four remote deletions in flight. It holds neither the publication lock nor the disk-reader lock during S3 calls. Claims persist across restart and advance their retry time by one minute, so unavailable old destinations do not permanently starve newer cleanup work. Successful deletion removes all versions of the exact object key. Prefix cleanup remains limited to an explicitly purged run namespace.

Once claimed, an object cannot be adopted by a publisher. A publisher instead allocates a new UUID for the same hash. Delayed requests, failed retries, and process restarts can therefore only affect the obsolete incarnation. Immutable locators avoid waiting for S3 deletion before synchronizing new work.

Deleting a conversation removes its references and queues its metadata cleanup. Shared bytes remain until every other owner releases them. The cleanup queue remains after the conversation record has been deleted. Logical conversation deletion can finish before physical S3 reclamation; failed cleanup retries in the background.

## Installation data

Captures and restores use the current authenticated v2 block envelope and shared
objects. Unchanged blocks retain their verified shared object identities. Mounted
disk grants pin their publications until the acknowledgement protocol releases them.
The manager keeps no second local payload/receipt cache for shared objects.

Version-11 manager databases support the
[data-preserving inventory upgrade to version 12](migrations/0012-typed-node-inventory.md).
It preserves shared-object inventory and references; other manager or storage
layouts require their own explicit compatibility checks. Preserve database,
identity, journals and object storage during upgrades. One active manager must
own a data volume. Separate
installations or encryption keys do not share an inventory.

## Validation

- Inventory tests cover cross-run reuse, last-reference deletion, immutable replacement during deletion, grace-period reuse, destination/size isolation, pending references across restart, corrupt-object invalidation, simultaneous first-upload convergence, and cleanup retry fairness.
- `scripts/tests/node_s3_test.py` uses a loopback Moto server, real SDK/CLI operations, synthetic credentials, and the existing publication/restore interfaces. Coverage includes current shared ciphertext, incremental reuse, retained grants, missing-object repair, exact version deletion, interrupted publication recovery, same-content publication in another run, reuse without a source-block request, deletion of each shared owner, and publication/read progress while a prefix deletion is deliberately blocked.
- The metadata benchmark reserves/commits twenty 4,096-block manifests; the optimized publication benchmark compares initial unique data, unchanged captures, and one-block deltas against the base revision, with exact restored-byte verification.

### Reproduction

Reproduce with `moto[server]==5.2.3` and the AWS CLI installed:

```sh
pnpm check
node --import tsx scripts/backend-schemas.mjs
git diff --exit-code -- crates/installation/schemas/inputs.json
cargo clippy --locked --workspace --all-targets -- -D warnings
node scripts/test-backend.mjs --no-fail-fast -- --test-threads=1
python scripts/tests/node_s3_test.py
CARGO_TARGET_DIR=/tmp/shared-candidate-target python scripts/tests/node_s3_test.py --benchmark
CARGO_TARGET_DIR=/tmp/shared-baseline-target python scripts/tests/node_s3_test.py --benchmark --repo /path/to/baseline
```

Copy the updated `crates/installation/tests/node_performance.rs` fixture into the baseline checkout too. It supports batched source requests and uses the fixture's `AWS_ENDPOINT_URL_S3` environment override instead of an explicit loopback HTTP endpoint; production endpoint validation remains unchanged.

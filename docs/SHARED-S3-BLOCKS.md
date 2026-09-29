# Shared S3 disk blocks

Identical nonzero 4 MiB blocks are stored once across published disks in the same manager and S3 destination. This applies across projects as well as conversations and task runs within a project. Empty regions remain implicit; the immutable guest OS image remains separate.

## Publication and reads

`nodes/shared_blocks.rs` owns the inventory and durable reference edges. One SQLite transaction reserves every distinct manifest hash, reuses verified objects, and creates missing objects. Cached statements and indexed lookups keep work proportional to the candidate manifest, not the number of other conversations. There are no S3 HEAD requests to discover reuse. Reused blocks skip node transfer, encryption, PUT, and verification GET.

New object keys are `shared-blocks/v1/{plaintext-sha256}/{object-uuid}`. Encryption retains random AES-GCM nonces and uses this complete key as authenticated associated data. The authenticated manifest carries each object's UUID. Read authorization still checks the disk grant and manifest; reading a shared object needs no inventory lookup or reference-count update. Size, authenticated ciphertext, and plaintext hash are checked on reads.

The ownership check, verified-object state, backup record, and current-publication pointer commit atomically. An invalidated reused object causes publication to retry instead of committing known-bad data. The existing publication operation lock still serializes captures, restore setup, and metadata retirement. Removing that serialization is outside this change; it currently also ensures there is no concurrent upload of the same missing hash.

Cancellation of a capture caller leaves the owned publication task running until its uploads and bookkeeping finish. An ordinary transfer error drains already-started PUTs before releasing the publication lock. Process death leaves durable reservations and upload intents for recovery.

## Lifetime and cleanup

Current manifests, movement references, and mounted-disk grants retain publication references. Reader draining occurs only through metadata retirement. After the last reference disappears, an object becomes eligible for collection after five minutes. A new publication can reuse it during that grace period.

The independent cleanup worker claims at most 32 shared objects and 32 legacy/manifest jobs per pass, with four remote deletions in flight. It holds neither the publication lock nor the disk-reader lock during S3 calls. Claims persist across restart and advance their retry time by one minute, so unavailable old destinations do not permanently starve newer cleanup work. Successful deletion removes all versions of the exact object key. Prefix cleanup remains limited to an explicitly purged legacy run namespace.

Once claimed, an object cannot be adopted by a publisher. A publisher instead allocates a new UUID for the same hash. Delayed requests, failed retries, and process restarts can therefore only affect the obsolete incarnation. Immutable locators avoid waiting for S3 deletion before synchronizing new work.

Deleting a conversation removes its references and queues its metadata cleanup. Shared bytes remain until every other owner releases them. The cleanup queue remains after the conversation record has been deleted. Logical conversation deletion can finish before physical S3 reclamation; failed cleanup retries in the background.

## Existing disks and rollback

Old run-scoped binary and JSON ciphertext remain readable. New captures always publish shared objects. For incremental captures, unchanged blocks absent from the node's captured delta are fetched from their previous published object, verified, and re-encrypted into shared storage. Old grants continue pinning old publications until the existing acknowledgement protocol releases them.

An idle legacy disk is not rewritten just to migrate it: it switches on its next actual capture. This avoids an unbounded background read/rewrite of existing storage. Legacy local receipts and run-scoped objects are retired by the same durable deletion queue. New shared objects have no second local payload/receipt cache on the manager.

SQLite schema version 5 prevents the previous executable from opening a database containing shared references. Back up SQLite, the encryption key, and the corresponding S3 data before upgrading; rolling back the database alone cannot restore objects already reclaimed. One active manager must own a data volume, as before. Separate installations or encryption keys do not share an inventory.

## Validation

- Inventory tests cover cross-run reuse, last-reference deletion, immutable replacement during deletion, grace-period reuse, destination/size isolation, pending references across restart, corrupt-object invalidation, and cleanup retry fairness.
- `tests/node_s3_test.py` uses a loopback Moto server, real SDK/CLI operations, synthetic credentials, and the existing publication/restore interfaces. Coverage includes legacy and shared ciphertext, incremental reuse, old grant retention, missing-object repair, exact version deletion, interrupted publication recovery, same-content publication in another run, legacy migration without a source-block request, deletion of each shared owner, and publication/read progress while a prefix deletion is deliberately blocked.
- The metadata benchmark reserves/commits twenty 4,096-block manifests; the optimized publication benchmark compares initial unique data, unchanged captures, and one-block deltas against the base revision, with exact restored-byte verification.

### Performance comparison

Release builds against baseline `1903506`, using a 64 MiB synthetic disk and a loopback Moto 5.2.3 server. Three independent repetitions start with an empty bucket; each performs one initial capture, three unchanged captures, and three one-block changes. Each checkout uses a separate Cargo target directory. Restored bytes are checked after capture.

| Capture | Baseline median | Shared median | Baseline CPU | Shared CPU |
| --- | ---: | ---: | ---: | ---: |
| Initial unique 64 MiB | 231.49 ms | 229.79 ms | 230.90 ms | 230.93 ms |
| Unchanged disk | 51.71 ms | 6.00 ms | 2.84 ms | 1.74 ms |
| One changed 4 MiB block | 127.89 ms | 33.36 ms | 17.33 ms | 15.30 ms |

These measure foreground capture latency: remote reclamation now happens independently. The unchanged and delta paths avoid synchronous deletion work; reused shared blocks also avoid transfer and encryption. Initial unique upload time is effectively unchanged in this sample. This is a local comparison, not a production WAN latency or peak-memory measurement. Raw samples are in [the benchmark artifact](benchmarks/shared-s3-blocks-2026-09-29.json).

The integration suite separately verifies that publishing identical content from another run uploads zero bytes and issues no additional source-block request. It also blocks a remote deletion deliberately and checks that publication and reads still complete.

Reproduce with `moto[server]==5.2.3` and the AWS CLI installed:

```sh
pnpm check
node --import tsx scripts/backend-schemas.mjs
git diff --exit-code -- backend/schemas/inputs.json
cargo clippy --locked --workspace --all-targets -- -D warnings
node scripts/test-backend.mjs --no-fail-fast -- --test-threads=1
python tests/node_s3_test.py
CARGO_TARGET_DIR=/tmp/shared-candidate-target python tests/node_s3_test.py --benchmark
CARGO_TARGET_DIR=/tmp/shared-baseline-target python tests/node_s3_test.py --benchmark --repo /path/to/baseline
```

For this historical baseline, apply the benchmark fixture's removal of the explicit loopback HTTP endpoint to `backend/tests/node_performance.rs` in that checkout too. Both versions use the fixture's `AWS_ENDPOINT_URL_S3` environment override instead; production endpoint validation remains unchanged.

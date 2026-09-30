# Verified block cache and active synchronization budget

Date: 2026-09-30.

## Problem

Production CPU sampling placed 78.71% of runner CPU samples in SHA-256 under
FUSE reads. A 4 KiB read could repeatedly reload and verify an entire 4 MiB block
because nine alternating blocks exceeded the shared 32 MiB base/journal cache.
The two observed startup protection pauses lasted about 91 seconds and ended
at publication acknowledgement. Their exact triggering reason was not logged.

## Decision

Each node's private disk state shares a bounded LRU of verified immutable base
blocks, initially 256 MiB. The setting is `memoryCacheMiB`, separate from the
clean disk-cache limit. Lookup and recency updates are constant time. Retained
payload bytes and entry count are bounded; in-flight readers can retain additional
buffers temporarily. The node controller retains the cache while idle. The
registry holds weak references and releases it after the last controller and
reader close. Standalone disks do not share outside their directory.

Only extents in an owned, validated disk manifest can request cached content.
The node's existing disk ownership and execution leases still fence access.
Size is checked on every cache hit; content is verified before admission.
Concurrent cold readers using the same source coalesce by hash, without holding
the cache lock over I/O. Distinct sources remain independent: one conversation's
unavailable source cannot strand another conversation or its cancellation.
Verified bytes remain shared across the node. Independent blocks remain parallel. Journal payloads use a
separate per-disk cache. Snapshot reads may reuse existing hot bytes but do not
admit cold blocks or promote scan hits, preserving the foreground working set.

New block identities are `b3-<64 lowercase hex digits>`. BLAKE3 identifies newly
indexed and changed blocks. Plain 64-digit SHA-256 identities retain their exact
meaning for older blocks. All upload, decode, restore and controller checks use
the declared identity algorithm; a prefix cannot silently reinterpret a digest.
Journal frames use `LEOJNL03` with BLAKE3 header and payload checksums. Existing
`LEOJNL02` SHA-256 frames and historical SQLite writes remain readable, including
mixed segments. Metadata and durability fences are unchanged. An old binary
must not be used after new frames or identities have been published.

## Controller and publication

CPU safety decisions read atomics and a configured in-memory policy. They never
wait for a policy-file read, filesystem scan, journal lock or blocking health
task. Initial open and status inspection observe free space; admission also checks
the reserve under the node lock before every write. Fault, space pressure and
unavailable-source signals still protect the VM. Configuration updates mounted
policies immediately. Lost VM-transition responses are retried, never confirmed
speculatively; explicit refusals and sustained uncertainty retain fail-closed
teardown. Logs now distinguish requested and confirmed transitions and their reason.

`dirtySince` remains the original age of unpublished work. It marks overdue
publication urgent, ahead of normal candidates, with a five-second retry floor.
`maxDirtySeconds` bounds the current VM's active synchronization window: the
deadline starts at the later of its oldest unpublished write and activation of
the current attempt. An old backlog therefore starts urgent synchronization
without immediately freezing guest startup. The grace is not renewed by polls,
seals or subsequent writes. It is renewed for a genuinely new execution attempt.

This deliberately changes the previous wall-clock-age pause policy. Work may
already have been unsynchronized while the node was idle; pausing immediately on
resume does not recover it. The local journal remains durable, but loss of the
node before remote publication still loses unpublished work. This is asynchronous
S3 synchronization, not synchronous remote durability.

## Validation

The regression uses 576 alternating 4 KiB reads over nine 4 MiB blocks. Current
main reverified every read; the cache verifies each block once and a second owned
conversation reuses those verified bytes. Unit tests cover scan pollution,
oversized entries, resize, mixed checksum formats, a blocked policy file and the
bounded activation deadline. Existing crash, publication, refusal and lost-ack
tests remain required, alongside encrypted S3 and real Firecracker qualification.

Run `cargo run --locked --release --example storage-benchmark` to compare the same
working set with 32/256 MiB cache budgets, SHA-256/BLAKE3, and direct positional
disk reads. This isolates block-serving cost and does not measure FUSE, network,
VM boot or model latency. End-to-end startup and prolonged-load results belong
in the release's performance evidence.

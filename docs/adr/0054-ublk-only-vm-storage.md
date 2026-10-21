# ublk as the sole VM block transport

Date: 2026-10-21

Status: accepted; rollout qualification recorded below.

Supersedes the optional/default-transport decision in
[ADR-0022](0022-optional-ublk-transport-and-vmgenid.md). The owner requested finishing
the rollout and removing vhost. ADR-0022 already records the paired production
comparison: fsync 10.31 to 7.47 ms and durable small files 19.66 to 17.35 ms, with
S3 publications enabled. Those are historical measurements, not new results from
this change. It establishes no general improvement for every workload.

## Decision

Use managed ublk for every VM data disk, through native virtio-blk Async with
ordered discard. Remove the Cairn vhost backend, its vendored shutdown fix,
rust-vmm queue/memory dependencies and the Firecracker shared-memory patch.
libublk becomes a required dependency; the old Cargo feature names remain empty
aliases for existing build commands. Native host execution retains its existing
ublk/ext4 path.

Prepare the host before stopping workloads: check/load the driver, require a
character controller and enabled io_uring, discover character/block classes and
grant SYS_RESOURCE for I/O flusher threads. The installation updater and node
supervisor share these checks. Existing operator resource limits, environment
expressions and capabilities survive. Unknown transport selections fail;
persisted supervisor selections for the former backend migrate to ublk.

## Data compatibility and rollout

No disk layout, block encoding, journal, checkpoint, installation schema or
identity changes. Keep epoch 9/database version 8. Existing flat journals mount
through ublk without conversion. The production v0.58.0 release predates other
main-branch migrations; those retain their separate reviewed forward migration
contract and must never be rolled back after the database fence advances.

Refresh the existing installation's host supervisor before approving its first
ublk-only image. The old installed updater cannot add new permissions by merely
pulling an image. Checkpoint the manager before stopping the runner; retain all
data and Garage. Verify the immutable image, healthy manager and both execution
nodes, native path-based Firecracker data drives, conversation recovery and no
owned fixture devices after tests. Keep snapshots and disk-layout choices as
configured; this rollout does not enable them.

## Qualification

The deployment regressions cover default ublk, migration of the previous
selection, exact host device permissions, preserved operator settings and refusal
to stop services when prerequisites fail. VM smoke tests cover actual native
block drives, checkpoint/reopen, retention, memory reclamation and teardown.

Fresh common-storage measurements compare this change against main
`1e7355877adf763326b69fbaddc0edbb22c04734`. Reproduce on each checkout with
`cargo build --locked --example storage-discard-benchmark`, then run
`target/debug/examples/storage-discard-benchmark` separately. Both use Rust
1.97.1, the development profile, Linux 7.2.9 on the same 32-thread workstation,
local tmpfs journals and in-memory encoded objects. Background compilation was
active; these timings do not establish a transport performance improvement.
The workload writes 16 unique 4 MiB regions (half seeded random, half repeated),
then optionally discards 8 full regions and random halves of 4 more. Values are
the median of three trial medians. Cold reads start with empty decoded caches;
the OS page cache is not flushed. Every decoded byte is verified.

| Case / metric | Reference | Candidate |
| --- | ---: | ---: |
| No discard, encoded bytes | 33,557,040 | 33,557,040 |
| No discard, cold 4 KiB read p50, ms | 206.745 | 202.049 |
| No discard, warm 4 KiB read p50, ms | 0.0018 | 0.0017 |
| No discard, write p50, ms | 2.061 | 2.275 |
| No discard, total write CPU, ms | 36.426 | 36.473 |
| No discard, total cold read CPU, ms | 3317.238 | 3245.023 |
| With discard, encoded bytes | 8,390,012 | 8,390,012 |
| With discard, cold 4 KiB read p50, ms | 3.305 | 3.132 |
| With discard, warm 4 KiB read p50, ms | 0.0013 | 0.0014 |
| With discard, write p50, ms | 2.039 | 2.058 |
| With discard, total write CPU, ms | 33.660 | 33.454 |
| With discard, total cold read CPU, ms | 836.113 | 809.564 |

This checks the unchanged codec/journal path, without a VM, S3 or production
network latency. Actual transport qualification uses the real-image VM smoke
suite; the release PR records those results and served-image read-back.

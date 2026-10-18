# Durable discard for current and retained guest images

Date: 2026-10-17.

## Decision

Propagate guest ext4 FITRIM through bounded native ublk and vhost requests into
ordinary durable zero write frames. Sparse append payloads preserve the existing
journal byte format and recovery contract. Current publication and shared owner
references determine when superseded objects can be collected. No controller
infers live free extents or mounts a guest-controlled filesystem.

Retained root images keep their existing tools and bridge. Attach a small,
immutable node control disk to start the current maintenance process at cold
boot. It batches trim outside unlink, observes late paired mounts and owns its
children through timeout and shutdown. The system and workspace device names
remain unchanged. Anonymous template identity changes with the added control
drive and its contents; old templates are not reused with the new topology.

Retain Async foreground I/O in the pinned Firecracker build. Submit discard through the existing
restricted io_uring using fallocate mode 3. On block devices Linux issues
WRITE_ZEROES, which ublk persists through the same authenticated zero operation
as DISCARD. The reserved NO_HIDE_STALE flag is rejected by the VFS and is not used.
Fence prior and subsequent requests until discard completion without blocking
VMM control, even when another paired view or kernel writeback is waiting. Reject maintenance immediately
when disk headroom is insufficient, preserve completions and bound a request to
8 MiB. Keep the
existing seccomp policy. Native CI exercises io_uring outside the Docker build
sandbox, and disposable guest tests verify retained-image bootstrap behavior.

## Consequences

This storage change preserves the existing installation compatibility epoch:
journal frames, database schemas, manifests and envelopes retain their formats.
Rollback follows the installation's overall compatibility policy.
The new cached template uses the same identity fields and a different version;
a compatible release can cold-boot ordinary disks and skip maintenance. No guest data reset,
delta dependency graph or eager bucket scan is introduced.

FITRIM may hide backend I/O errors. Guest logs report tool invocation success
only; backend discard counters establish execution. Observed tool failures retry
after a minute; hidden backend failures may wait for the normal 30-minute pass.

Trim adds bounded journal work and can increase competing read/write latency;
measure encoded bytes, cold/warm reads, writes, CPU and host/workload conditions
against a reference. Measure the guest bootstrap and native ublk path separately
from local codec work and production network time. See
[durable guest discard](../STORAGE-DISCARD.md) for contracts, rollout and commands.

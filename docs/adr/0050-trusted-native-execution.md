# Trusted native execution alongside Firecracker

Date: 2026-10-18.

Cairn supports an explicit Linux native backend for selected agents directed by
trusted people, so contributed desktops can share their installed GPU with agents.
This extends [GPU deferral](0003-defer-gpu-until-direct-firecracker-support.md)
for native work only; it does not change Firecracker's device model or its default.
A separate unprivileged identity, systemd cgroups and a boot-scoped lease guardian
provide managed ownership and cleanup, with a weaker isolation boundary than a VM.
CPU/RAM are bounded; desktop responsiveness, disk quotas and per-agent VRAM are
not hard guarantees. No GPU takeover or personal desktop access is required.

Native execution extends [mandatory S3-backed disks](0009-s3-backed-disks-required-on-every-node.md)
with stopped `native-files-v1` checkpoints. The existing encrypted block publication
and scoped grants remain mandatory. Live files are not advertised as a coherent
backup. Recovery validates before replacement and preserves prior local work.

People and explicit node ceilings propagate from authenticated requests through
bounded, revocable agent delegation. Shared installation membership alone never
grants native execution. Unknown historical/plugin provenance fails closed. The
manager requires fresh Beacon membership evidence before extending host leases.

The reviewed compatibility epoch advances from 5 to 6 for persisted native backend,
provenance, node grants and checkpoint formats. Forward migration is additive;
legacy VM data is unchanged and the legacy tree stays fixed. Epoch transitions
require reviewed rollout as in [production releases](../PRODUCTION-CAIRN.md#installation-data-compatibility).
Epoch 5 already preserves F2's durable session-restart intention; its behavior is
retained. Epoch-5 binaries cannot interpret the native records introduced here.
Rollback to epoch 5 requires its matching prepared data backup and binary; automatic
updates do not cross epochs or restore data. Windows/macOS and desktop-session
access remain separate future work.

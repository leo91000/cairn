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

Update, 2026-10-22: the owner may grant an agent all nodes, including future ones.
This replaces the original requirement for an explicit agent-side selection;
node-side opt-in, trusted people, provenance, delegation ceilings and revocations
remain mandatory. Existing environments keep their backend and require migration
before resuming after an agent changes backend, including with an all-node grant.
Native slots are configurable and share one
systemd workload slice's CPU/RAM ceiling, alongside the existing shared disk
accounting. The supervisor and lease guardians remain outside that slice so an
exhausted workload budget cannot prevent fencing. Epoch 11 and SQLite version 10
fence older binaries, which refuse a persisted multi-slot native budget. The
reviewed forward transition and native supervisor procedure are recorded in
[automatic migrations](../AUTOMATIC-MIGRATIONS.md#current-transition-epochs-310-to-11).

The reviewed compatibility epoch advances from 5 to 6 for persisted native backend,
provenance, node grants and checkpoint formats. Forward migration is additive;
legacy VM data is unchanged and the legacy tree stays fixed. Epoch transitions
require reviewed rollout as in [production releases](../PRODUCTION-CAIRN.md#installation-data-compatibility).
Epoch 5 already preserves F2's durable session-restart intention; its behavior is
retained. Epoch-5 binaries cannot interpret the native records introduced here.
Rollback to epoch 5 requires its matching prepared data backup and binary; automatic
updates do not cross epochs or restore data. Windows/macOS and desktop-session
access remain separate future work.

Update, 2026-10-23: native conversations use distinct host accounts and disjoint
subordinate UID/GID ranges. A RootlessKit enclave separates their network, PID and
user namespaces; its immutable supervisor starts rootless Docker on demand.
Only the outer identity-mapping bootstrap permits the mapping helpers' privilege
transition. The enclave sets NoNewPrivileges before starting the engine/daemon;
the engine has an empty capability bounding set. Existing managed mounts, device
filters, shared budgets, leases, guardians and cgroup fencing remain. Docker's
mandatory authorization plugin preserves restricted filesystem rights. The host
Docker daemon and desktop sockets remain unavailable. See [native Docker](../NATIVE-DOCKER.md)
for storage, conversion, networking and compatibility limits. Epoch 13, SQLite
12 and the complete native runtime identity fence previous writers.

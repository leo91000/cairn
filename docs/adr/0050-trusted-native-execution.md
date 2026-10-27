# Trusted native execution alongside Firecracker

Update: [ADR-0073](0073-automatic-agent-placement.md) permits automatic per-agent
placement across both backends while retaining the native authority rules below.

Date: 2026-10-18.

Cairn supports an explicit Linux native backend for selected agents directed by
trusted people, so contributed desktops can share their installed GPU with agents.
This extends [GPU deferral](0003-defer-gpu-until-direct-firecracker-support.md)
for native work only; it does not change Firecracker's device model or its default.
A separate unprivileged identity, systemd cgroups and a boot-scoped lease guardian
provide managed ownership and cleanup, with a weaker isolation boundary than a VM.
CPU/RAM are bounded; desktop responsiveness, disk quotas and per-agent VRAM are
not hard guarantees. No GPU takeover or personal desktop access is required.

People and explicit node ceilings propagate from authenticated requests through
bounded, revocable agent delegation. Shared installation membership alone never
grants native execution. Unknown historical/plugin provenance fails closed. The
manager requires fresh Beacon membership evidence before extending host leases.

Update, 2026-10-23: protected native conversations use distinct host accounts and disjoint
subordinate UID/GID ranges. A RootlessKit enclave separates their network, PID and
user namespaces; its immutable supervisor starts rootless Docker on demand.
Only the outer identity-mapping bootstrap permits the mapping helpers' privilege
transition. The enclave sets NoNewPrivileges before starting the engine/daemon;
the engine has an empty capability bounding set. Existing managed mounts, device
filters, shared budgets, leases, guardians and cgroup fencing remain. Docker's
mandatory authorization plugin preserves restricted filesystem rights. The host
Docker daemon and desktop sockets remain unavailable. See [native Docker](../NATIVE-DOCKER.md)
for storage, conversion, networking and compatibility limits.

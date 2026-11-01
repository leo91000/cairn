# Trusted native execution alongside Firecracker

Update: [ADR-0075](0075-plugin-hooks-rules-and-scoped-grants.md) replaces plugin
delegations with hook rules. A rule version records the native authority of the
authenticated request that saved it; each action that reaches native work checks that
record and the creator's current native authority.

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

Directing an existing conversation also checks the prospective native parent
chain against the destination run before committing the directing effect. The
check runs in the same writer transaction as the grant and message/control, so
opposing requests cannot both create a cycle. This applies to VM conversations
that retain native provenance too. Backend migration validates its destination
grant at both the initial and final writer transaction; a stored pending
migration is rechecked before source reads and destination admission. Admission
of a prospective run includes that run's identity instead of following its old
stored authority. A cyclic directing effect is refused without
discarding a saved revocation edge or rewriting existing authority. Delegated
reports remain readable through `task_status`; they do not require the requester
to acquire its child's authority as a parent. Existing malformed lineages remain
denied and require a separate authorized recovery, rather than an automatic
provenance rewrite.

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

# Managed administrator Docker lifecycle

Native administrator execution remains cooperative host administration, as in
[ADR-0050](0050-trusted-native-execution.md). Root can bypass its supervisor. The
ordinary Docker command must nevertheless keep services within the attempt's
CPU, memory, freeze and termination boundary. A rootful daemon using the systemd
cgroup driver otherwise creates sibling `docker-*.scope` units in `system.slice`;
fencing the runner does not fence these consumers of its disk.

Provide a lazy Docker wrapper and root-owned supervisor in the administrator
workload's mount namespace. Give the workload service `Delegate=yes` and
`DelegateSubgroup=runner`. Dockerd, its starter, the engine and helpers execute
in that subgroup; Docker uses the cgroupfs driver with a mandatory parent equal
to the enclosing attempt service. Container leaves are siblings of `runner`
inside that service, so its existing resource limits, freeze and
`KillMode=control-group` cover the complete tree. A separate systemd slice would
require moving the existing protection, accounting and lease fences to a second
unit. Retaining the service as the single boundary avoids that migration.

The mandatory administrator authorization plugin permits host administration
while refusing foreign container cgroup parents and automatic restart policies,
including case-folded/duplicate JSON controls. Before dockerd opens retained
metadata, reset saved restart policies and replace saved cgroup parents with the
current attempt's service. Refuse Swarm and managed daemon plugins, whose persisted
desired state can activate work independently of container restart policies.
Refuse the embedded build/session/gRPC API, whose internal container creation
and arbitrary BuildKit frontends can override cgroup ownership. The wrapper
configures a named docker-container Buildx builder with no restart policy,
private cgroup namespace, the current attempt parent and default image loading.
Its requests pass the ordinary container API boundary. Pin the BuildKit image by
digest so builder image verification does not require the refused embedded API.
Run dockerd in separate mount/network namespaces established before runtime
threads. Mount propagation is recursively slave: daemon mounts cannot propagate
out, while host unmounts retire inherited copies of other runs' filesystems.
Making the whole tree private would pin those disks until this daemon exits.
A private `/run` preserves only this attempt's Docker runtime/plugin
bindings; it hides host Docker/containerd/DBus sockets, including firewalld.
Record the host resolver's canonical target before hiding `/run`. For a target
under `/run`, create only a private placeholder before binding the slirp resolver;
this supports systemd-resolved symlinks without changing the host resolver.
Slirp supplies egress and explicit IPv4 TCP/UDP host forwarding, whose lifecycle
follows container start/stop/deletion. Administrator slirp permits host loopback
access: disabling it also drops UDP replies to localhost publication clients.
Administrator workloads already have host authority; the separate network owns
firewall/interface lifecycle rather than restricting access to host services.
Protected RootlessKit retains its host-loopback restriction.
Before exec, its child clones a mount
namespace and makes inherited mounts recursively slave. The pinned sandbox
makes only its root private before detaching the old tree; shared submounts
could otherwise propagate their removal into the supervisor's namespace.
Mutation responses reconcile the current
complete container and slirp forwarding inventories before reusing host ports.
The slirp inventory recovers additions or removals whose reply was lost after
the mutation committed; received rule IDs alone cannot prove current state.
A one-second observer
also retires publications after natural exits, with missed ticks skipped during
CPU freezes. Default publications bind loopback;
unsupported IPv6/SCTP and non-loopback-specific addresses are explicitly refused.
A private sysfs reflects this network's interfaces, retaining the workload's
cgroup mount separately. Only this private network's route_localnet and a raw
PREROUTING accept for the slirp TAP permit access to Docker loopback publications;
Docker keeps its ordinary protections on container interfaces. No host bridge, firewall chain, forwarding policy or
sysctl is changed. A unique host bridge would still share Docker firewall chains
and can erase another daemon's rules, so it is insufficient isolation.
Unusable saved container records are durably quarantined outside `containers`,
without deleting their files or preventing healthy records from loading. Bundled Compose and
Buildx are registered in the client's plugin directory. Explicitly starting a saved container remains possible;
starting dockerd alone never starts a saved service. An uncertain startup is not
retried in the same attempt. The daemon and plugin supervisor inherit the workload
cgroup and cannot retain its client stdout/stderr pipes. In a managed background
environment, the wrapper requests this same lazy start through an attempt-local
socket. The environment starts the supervisor outside individual provider and
job process trees, while the attempt's service still owns the entire cgroup.

Rootful metadata lives in `docker-administrator` next to the protected `docker`
store on the conversation's filesystem. They have different UID mapping and
privilege contracts; mode transitions preserve both without opening one as the
other. The rootful store is excluded from protected identity translation,
including its volume user IDs, ACLs and capabilities. Existing agent-installed daemons and stores are not silently adopted.
Their escaped consumers require the subsequent verified holder recovery work.
Protected RootlessKit execution retains its current daemon, policies and cgroup.
The native runtime marker advances to `ublk-rootless-v2` before identity work.
The new reader accepts `ublk` and `ublk-rootless-v1`; older controllers refuse the
new marker instead of translating raw rootful ownership on rollback. Journal,
block-device receipts and the pending identity journal retain their formats.

Before systemd termination, thaw the workload and request container shutdown
through its existing private socket, without starting a daemon. Bound that best-effort phase to three seconds (one-second container stop grace, concurrent
requests), including paused containers. A root ExecStop command runs that same
phase before systemd terminates descendants on normal engine exit. A missing daemon is harmless; failure is reported and the positive
cgroup fence remains mandatory. The independent lease guardian uses its explicit
state directory for the same operation. Node startup recovery and forced disk
shutdown are addressed separately; this decision alone does not repair an already
escaped legacy scope or a blocked kernel deletion.

Qualification extends the disposable native runtime fixture with administrator
containers that write to a bind of the run disk: verify service cgroup ancestry,
resource charging, storage-pressure freeze/thaw, termination before unmount,
retiring an earlier run's disk while a later administrator daemon stays live,
reopening without implicit restart and return to the protected store. Unit tests
cover saved metadata rewriting, lifecycle authorization and private-socket stop
requests. Production services and disks are never used to reproduce the kernel
hang.

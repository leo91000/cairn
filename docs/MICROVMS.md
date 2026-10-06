# MicroVM execution

The manager schedules work and owns account refresh credentials. The runner owns
execution attempts. Every production agent, including Main, executes in a private
Firecracker VM. YOLO remains the default inside the guest.

## Interface and implementation

The authenticated runner interface is start, output stream, wait and stop, keyed
by an attempt UUID. A separate run UUID identifies persistent storage. Chats reuse
that run across turns. The manager saves the attempt before starting it and fences
the previous attempt before reusing the disk or account lease.

`runner.rs` implements that interface and durable attempt outcomes. `microvm/host.rs`
hides disks, jailer, network rules, vsock, imports and teardown. `microvm/guest.rs`
bridges the existing Rust chat/task process inside the VM. `microvm/wire.rs` bounds
protocol messages and rejects truncated frames. Output reads retain partial frames
while live inbox updates are delivered.

`microvm/pool.rs` owns execution slots (initially `CONCURRENCY`, default four).
It can prepare anonymous VMs with native Codex initialized, within those same
slots and shared budgets. The manager can update both through authenticated
`/node-budget`. `LEO_READY_VM_POOL=false` disables speculative preparation;
the default is `true`. Guests without the readiness capability keep booting cold.
`LEO_READY_VM_POOL_SIZE=1..4` controls the target size (default one). The node
supervisor preserves the equivalent `readyVmPoolSize` in its `config.json`.
The same file accepts `readyVmPool: false` to disable preparation entirely.
Setting `cacheApprovedRuntimes` to `false` in that file disables background
downloads of historical runtime packages. The current image and locally pinned
environments remain available; moving an older environment to this node can
require re-enabling preloading. The default remains `true`.
Additional spares need measured resident RAM plus 2 GiB of startup headroom
within one quarter of the node's RAM budget, bounded between 2 and 8 GiB.
Actual RAM is checked again after preparation; an oversized pool shrinks oldest
first. The original single-spare policy and shared hard limits still apply on
small nodes. This is a target, not a reservation of four guaranteed spares:
active work, retained conversations, slot limits and RAM pressure take priority.

A compatible new managed Codex chat claims that VM once, after durable disk
ownership and storage authorization. Existing disks, threads, custom Codex homes,
other providers and command plans use the ordinary cold path. Preparation yields
to live admission; its physical slot is released only after teardown. Spare RAM
is included in node usage, preparation requires 2 GiB of headroom, and a spare
is evicted under resource pressure or a budget change. Anonymous readiness has
no periodic expiration: retiring a healthy spare would create a cold-start gap
while its replacement boots. The controller checks native readiness at claim
and retires all spares on shutdown or runtime replacement. `/health` reports `ready` and
`preparing` within occupied slots. See [the measured comparison](adr/0019-booted-vms-with-ready-codex.md).

Reservations release their slot after teardown, including abandoned HTTP requests.
If all slots are occupied or shared memory/disk headroom is low, new work waits.
`/health` exposes capacity, occupied slots, applied budgets, usage and pressure.

Successful Codex conversations may retain their own CPU-paused VM for
180 seconds (`LEO_VM_RETENTION_SECONDS=0..300`; zero disables retention). A
retained VM is never assigned to another conversation. Its balloon reclaims
available pages before pausing, with free-page reporting returning shared memfd
pages to the host. Admission counts allocated memfd pages once, plus the VMM's
other resident memory; virtual guest RAM is not a reservation. The shared cgroup
remains the hard limit. At most two conversations are retained, with a combined
cost of at most 2 GiB or one quarter of the node budget, whichever is smaller;
retention also requires node usage at or below 75% of its RAM budget.

Active admission evicts the oldest retained conversation first, then the
anonymous spare, before rejecting work. Idle monitoring uses the same order.
Expiry, pressure, changed budgets or immutable mounts/privileges, controller
shutdown and disk deletion also retire retained VMs. Teardown reaps the VMM
before releasing its physical disk or slot. Failed or cancelled turns and
command plans use the ordinary cold lifecycle. `/health` reports `retained` and
`retained_memory_mi_b` alongside the anonymous pool and occupied slots.

Each resumed turn gets a new attempt lease, renewed managed-account login and
thread/MCP configuration. CPU pause and retention share the attempt's safety-control lock;
no guest process runs while retained. Background guest processes pause with the
VM and can resume on its next turn. Repeated disk publication holds a per-VM
capture barrier and leaves CPUs paused; it never closes the mounted source's
read cancellation token. The persisted disk, journal and acknowledged publication
remain the recovery authority after eviction or a controller crash. RAM retention
is only an optimization. See [the retention decision](adr/0020-paused-conversation-retention.md).

Archive imports negotiate `binaryImports` through guest status. Supporting guests
receive length-prefixed binary chunks of at most 64 KiB and an explicit zero-length
end marker, avoiding Base64 expansion and JSON encoding of repository contents.
Control messages remain bounded JSON. Both sides retain the legacy JSON/Base64
path for compatibility with older images. Imports still stream directly to guest
extraction and preserve the existing authorization, atomic publication and
read-only policy. See [startup measurements](STARTUP-PERFORMANCE.md).

Each VM boots a pinned kernel and a read-only root image. A sparse 32 GiB ext4
private disk supplies the writable overlay, repositories, home, sessions, installed
tools and Docker cache. Docker and containerd data are mounted directly from that disk. The daemon
starts only when the `docker` command first needs it. VM recreation retains the disk;
RAM snapshots are not part of correctness or recovery.
Docker Engine, Buildx and Compose come from Docker's signed Debian repository.
The CLI-update guest build refreshes those packages as well.

The controller never mounts a guest-modified filesystem. Initial archives travel
into the guest over vsock; only a bounded result file comes back to a predetermined
manager path. Guest edits never overwrite the host checkout. The UI retains the
conversation/run needed to resume and inspect saved work. Disks are retained,
including on failure or cancellation; back them up and account for their storage.
Never remove a retained disk if its uncommitted work is still needed.

## Projects on demand

Project authorization and workspace preparation are separate. New conversations
without a selected project start in an empty workspace. Chats and scheduled tasks
with a selected project seed only that repository. The prompt lists the authorized
project IDs and loaded paths; it never advertises host paths for unopened projects.

Every new VM run receives a built-in `leo_workspace.open_project` MCP tool, even
when the agent has no external MCP connections. Its short-lived bearer grant is
limited to that run and revoked when the run ends. `project_workspaces.rs` checks
the current agent policy and snapshotted project catalog, then prepares a private
host seed. Selecting a project supplies initial context; restrictions belong to
the agent's project access policy.

The manager asks the authenticated controller to import the seed into that active
attempt. The controller accepts only canonical directories beneath that run's
private storage. The guest extracts into a temporary directory and publishes it
only when complete. Existing destinations are reused, never replaced. Read-only
project policy is persisted and reapplied after reboot. Workspace-write sessions
include the private project root in their writable roots.

Loaded projects are also recorded on the run, independently of the worker's
checkpoint writes. Resuming merges that catalog into the saved execution plan;
the persistent guest disk retains edits, Git history and caches. Legacy VM
conversations keep their existing eagerly prepared workspaces and can open
additional authorized projects after resuming. A failed transfer
can be retried with `open_project`. The manager never mounts or reads the guest's
writable filesystem. Independent VM disks do not take shared-host project locks;
local development executions retain those locks.

## Authentication and live chat

The guest has a local Unix authentication socket backed by a per-VM vsock relay.
That relay connects only to the manager socket for the current run/account lease.
The manager serializes refresh rotation across concurrent uses of an account.
Valid token reads use an atomic vault snapshot under the live lease lock, without
waiting for network-bound quota monitoring. Refresh requests still use the shared
rotation lock and previous-token check. New account selection continues to honor
due quota polling. Committed chat/task work wakes the scheduler immediately, and
fresh account-specific model capabilities avoid redundant guest discovery where
available. Connected apps and MCP startup remain enabled for actual runs.
Account IDs, refresh tokens, the runner credential and the host Docker socket are
not supplied through this relay. Managed authentication files are excluded from
home imports. GitHub credentials remain scoped according to the agent policy.

The manager remains authoritative for queued messages, answers and attachments.
Changed inbox contents are transferred while output continues streaming. The guest
inbox is readable by the agent and written by the guest supervisor. Codex session
IDs are persisted in manager events; `thread/resume` verifies the retained session
inside the replacement guest.

## Host and resource constraints

Production currently supports Linux x86-64 with working KVM. The Compose controller
has explicit infrastructure capabilities, access to KVM/TUN and no Docker socket.
Firecracker runs through jailer as an unprivileged UID, with its default seccomp
filters. The controller's AppArmor/seccomp allowances are needed for jailer mount
namespaces and privilege setup; they are not granted to agents on the host.

Execution slots start from `CONCURRENCY` (default 4), then follow the node's
configuration. CPU and RAM are shared under the controller's private cgroup-v2
limits. Each VM sees the node's RAM ceiling and up to 32 vCPUs, without reserving
that physical capacity. Free-page reporting and cooperative ballooning return
unused guest memory to the host. Above 85% of the RAM budget, running guests
give memory back while keeping 1 GiB available; below 75%, they get it back. Disk budgets bound local journals and caches;
retained disks keep their logical size. See [the shared-budget decision](adr/0012-shared-node-budgets.md).
Guest RAM is a shared memfd, so the host can only reclaim pages the guest gives
back. Without host swap, a peak above `memory.high` stalls every VM and can still
end in an OOM kill at `memory.max`. A compressed host swap lets such peaks slow
down instead; see [compressed host swap](DEPLOYMENT.md#compressed-host-swap).
The Compose controller initially allows 8 CPUs, 20 GiB RAM and 256 host processes.
Guest process counts are not host process counts. Additional slots do not raise
resource budgets; simultaneous peaks can still exhaust shared RAM. VM networks
use distinct /30 subnets in private 10.0.0.0/8; address exhaustion is reported rather
than reusing another slot’s network. Disk ownership is protected by an exclusive file lock;
one writable disk cannot be opened by two attempts. Console and execution output
are bounded independently of the guest disk.

Per-guest firewall rules permit outgoing TCP on all ports to public destinations
and DNS (UDP 53). SSH works on standard and custom ports; other UDP is blocked.
They reject private/reserved destinations and access to the controller itself;
new inbound connections are not forwarded into guests. This allows the public
manager MCP gateway, public SSH servers and Git over HTTPS or SSH. Additional
outbound protocols require an explicit network-policy change. Firecracker does not
remove the need to patch host kernel, firmware, guest kernel and the VMM; follow
upstream production guidance.

## Builds, upgrades and recovery

The Dockerfile builds Linux 6.12.109 from its checksum-pinned kernel.org source with
`deploy/microvm/kernel.config`. Firecracker and jailer 1.17.0 are checksum pinned.
The guest root uses the same Rust binary and development tools as the manager.
CLI-update images rebuild the embedded guest root as well. Tag deployment migrates
Coolify's stored Compose configuration, preserving generated network names and
labels. `/health` only succeeds once the Firecracker runner is ready.

Before the first upgrade, back up application volumes and saved workspaces and
wait for active work to finish. Retain the previous Compose document with its image
for rollback: an old container-runner image needs its old Docker-socket setup.
After agents have made changes in VM disks, a rollback must preserve those disks;
old releases cannot interpret their guest storage as host worktrees.

Controller restarts fence its container PID namespace. Interrupted attempts are
recorded as exited; the manager recreates VMs against their retained disks. Graceful
stop requests guest shutdown and sync; forced stops rely on ext4 journal recovery.
If only the controller restarts, saved conversations are automatically recovered,
with at most three attempts per run. Cancellation and ordinary command failures
do not trigger this recovery. A run without a recorded session requires review
instead of silently starting a new conversation after an ambiguous interruption.
No task is allowed to silently switch to host execution when VM setup fails.

## Validation

Run `pnpm test:backend`, `cargo clippy --workspace --all-targets -- -D warnings`
and `pnpm check`. The real KVM test runs with `node tests/runner-smoke.mjs IMAGE` on
the Docker host. It uses synthetic credentials and temporary storage to exercise
kernel isolation, the authentication relay, live steering, Docker and disk reuse.
Use `VM_TEST_ROOT=/var/tmp` when `/tmp` is a small memory filesystem.

The September 11 hardware check on the production VPS completed the first probe,
including downloading and running BusyBox, in 9.8 seconds. Recreating the VM with
its saved disk and Docker image took 4.0 seconds. Cancellation, forced controller
restart and recovery also passed. These are complete probe durations, not bare
Firecracker boot times. Concurrent read-only and workspace-write guests are also
covered by the smoke test; neither restricted mode receives sudo access.

Sources: [Firecracker production setup](https://github.com/firecracker-microvm/firecracker/blob/main/docs/prod-host-setup.md),
[jailer](https://github.com/firecracker-microvm/firecracker/blob/main/docs/jailer.md),
[vsock transport](https://github.com/firecracker-microvm/firecracker/blob/main/docs/vsock.md),
[Linux source](https://cdn.kernel.org/pub/linux/kernel/v6.x/linux-6.12.109.tar.xz).

## Nested KVM for Android

The guest kernel builds in Intel and AMD KVM. If the outer host enables nested
virtualization, agent UID/GID 1000 can use the guest-created `/dev/kvm`. This does
not pass the outer host device into the guest. The default Firecracker CPU
configuration is preserved so KVM controls which features are available.

Check `/sys/module/kvm_intel/parameters/nested` (`Y` or `1`) or the corresponding
`kvm_amd` file on the outer host. Configure it through the host's normal boot
configuration if disabled; do not unload KVM while production VMs run. A cloud
VM host must itself expose virtualization extensions. Guest package installation
cannot compensate for unavailable host capabilities.

The image CI requires a nested `KVM_RUN` probe and an actual Android boot/tap and
restart scenario. The supported lifecycle remains fresh guest boot from disk;
we do not serialize Firecracker snapshots containing running nested VMs. See
[research and limitations](NESTED-KVM-RESEARCH.md).

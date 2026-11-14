# Operating a native node

Native execution is for selected agents used by trusted people. It runs host GPU
APIs under a managed account while the owner continues using the computer. It
provides less isolation than Firecracker. GPU memory exhaustion, desktop slowdown
and driver bugs remain possible; no software boundary can promise a GPU job will
never crash a driver. Review code before testing on a personal workstation.

## Requirements and enrollment

Use Linux x86-64 with systemd 255+, cgroup v2, Python 3, curl, useradd/usermod,
e2fsprogs, io_uring and a working `ublk_drv` kernel driver on Linux 6.11 or newer
(asynchronous device retirement is required). Host libraries must be
compatible with the published coding engines. The installed GPU
driver, render/video device permissions and application libraries must already
work. Cairn does not install GPU drivers, detach a GPU or close a graphical session.
Enrollment loads the existing ublk storage module and checks its control device;
unsupported kernels must be provisioned before native execution can start.
The installation manager must be configured; conversation disks require no S3 storage.

As the installation owner, open Nodes, add a machine and choose Native process.
Use the generated installer on that node and enter the single-use code when asked.
Without a direct manager address, it first installs the node gateway, which Beacon
introduces to the manager ([additional execution nodes](DEPLOYMENT.md#additional-execution-nodes)).
The installer creates `cairn-workload`, installs a root-owned systemd controller,
and leaves native execution disabled. In Configure, explicitly enable native work,
select trusted people and conservative CPU/RAM/disk limits. Configure a native
agent and select this node. Other agents retain their Firecracker backend.

Approved updates drain the node before selecting the verified candidate. The
selected candidate is retained across failed health checks and service restarts.
A durable update record retries the manager's maintenance-completion
acknowledgement once the candidate is healthy, including after a lost reply.

Start a new conversation as a trusted person. An authorized coordinator can also
start a conversation with that native agent through its installation tools. Each
follow-up, resume, scheduled dispatch and delegated run retains and rechecks its
originating permission. Previously unattributed conversations and external plugin
origins cannot silently gain host access. Explicit node moves may change an
existing conversation’s backend under those same checks; see ADR-0082.

## Switching an existing machine

A node keeps the execution backend it enrolled with; a heartbeat reporting another
backend is refused. To move a Firecracker machine to native execution, or back, use
Switch to native (or Switch to VM) on its node. Cairn pre-fills the machine name
and the other backend. Run the generated installer on the same machine: the new
runtime enrolls as a separate node with its own identity, grants and storage,
alongside the existing one. After its configuration and agent step, Cairn offers to
retire the previous node. The invitation assigns the replacement's identity before
it connects, so another enrollment cannot advance this switch. Every invitation
and enrolling component must carry the current identity and version.
Revoking it from there also returns conversations fixed
to or preferring that node to automatic placement; a plain revocation keeps those
explicit choices waiting. Switch each agent's backend on its page and allow the
new node. Existing conversations show **Migrate this conversation…** in their
execution-node details on web and Android. Confirming discards the working files:
the environment expires and its first empty environment starts on the selected
node. History, attachments and artifacts remain. The current turn stops, questions
close, queued messages and waits keep their continuations, and active missions
resume automatically. A chat otherwise waits for its next message. There is no
disk transfer or physical conversion. Back up needed files before confirmation.
Finally run the uninstall command shown for the previous runtime.

The public Cairn MCP and the `cairn_installation` agent tool expose the same setup
and retirement controls: `list_nodes`, `list_native_callers`,
`create_node_enrollment`, `update_node`, `update_node_storage` and `revoke_node`. Installation management
access is required for mutations; read-only public MCP access cannot enroll,
configure or revoke a node. Agent installation grants are checked again in each
mutation transaction. Enroll the other backend, wait for the returned `nodeId`,
configure its native trust when applicable, and grant agents through `update_agent`.
Only after the replacement is ready, revoke the old identity with
`discardWorkingFiles: true` and `releasePlacements: true`. Plain revocation also
requires the discard confirmation but preserves existing placement choices.
Revocation expires every environment on the node; the next turn starts empty.
`get_chat_backend_migration` previews the saved/profile backend difference,
authorized destinations and the working-file loss warning. `migrate_chat_backend`
accepts either `chatId` or `runId`, plus `nodeId`, the destination `backend`,
`confirm: true` and, while the environment exists, `discardWorkingFiles: true`.
For an already expired environment the discard confirmation and warning are
unnecessary. These tools neither install nor uninstall a runtime on the host.

## Daily use

An agent can use all nodes, including future ones, or an explicit selection.
New conversations use its selected execution backend. Existing environments keep
their saved backend; after an agent changes backend, migrate its existing
conversations before resuming them. An all-node grant does not replace that
migration. Native execution still requires the node owner's opt-in, trust
in every originating person, and fresh membership and delegation evidence.

Native runtimes supporting exclusive reservations report `exclusiveReservations: true`.
The owner grants “Reserve exclusively” on the agent's node access page, on web or
Android. An authorized run uses `acquire_exclusive`, `exclusive_status`,
`renew_exclusive` and `release_exclusive` through its workspace tools. The same
agent profiles and node capabilities are exposed through public Cairn MCP and
`cairn_installation`; installation agents cannot grant themselves exclusivity.
Reservations block new Cairn execution and freeze other managed workload cgroups,
including descendants, with confirmed automatic resume after release or expiration.
Storage protection retains its own suspension reason. Confirmed exclusive pauses
extend execution time budgets; security leases keep expiring normally. Native
controller recovery fences the old workloads before clearing reservation ownership.

This reserves Cairn work on that node, not external applications, physical GPUs or
the whole host. Frozen processes keep their RAM and GPU allocations. Optional RAM
headroom is checked against real shared cgroup usage and host availability; native
reservations fail when it is unavailable rather than reclaiming peer memory.
Administrator enforcement remains cooperative because root can escape cgroups.
Older native runtimes require an update before the reservation can be used.

The controller defaults to one slot and supports configurable concurrent executions.
All slots share the configured CPU/RAM ceiling through a systemd workload slice,
and the existing disk budget. Per-conversation limits are ceilings, not reservations.
It uses low CPU/I/O priority and conservative default budgets.
Keep interactive applications running during qualification. Start with a short,
small GPU workload and inspect memory use and desktop responsiveness before larger
jobs. VRAM has no universal hard per-agent quota. Disabling native execution or
removing a trusted person stops affected work after its remaining lease expires.
Turning off admission prevents new placement without automatically canceling work.
An offline Beacon authority check eventually stops native work too.

The managed filesystem contains system binaries/libraries read-only, a private
HOME, project files, output, temporary files and GPU devices. The personal home,
desktop sockets, host Docker daemon and administrator privileges are unavailable.
Conversations use their own lazy [rootless Docker enclave](NATIVE-DOCKER.md) for
builds, containers and Compose. Published ports are private to that conversation's
network; GPU containers and privileged Docker features are unavailable. Images,
build cache and named volumes follow the durable environment's native storage.
Changing nodes or execution backend expires the environment, including its Docker
images, build cache and named volumes. Push images or export data that must survive.
Graphics tests must use headless/offscreen APIs. Cairn bundles the coding engines,
including Codex's `codex-code-mode-host` companion beside its executable, Node.js
and `gh`; other toolchains use this node's installed binaries/libraries or
user-managed tools under the private home. Respect repository version pins.

Skills are the run's frozen bundle, copied afresh for each attempt. Accounts and
MCP access use the usual scoped brokers. Credentials provided to an engine are
available to that engine. Do not assume the checkpoint exclusions detect secrets
written into arbitrary project files.

## Storage and recovery

Expiration retirement uses a durable `retirementId`, shared by all retries for
that detached environment. The controller stores completion receipts in
`state/disk-retirements/<run>/<retirementId>` outside the conversation disk and
checks them under its storage lock before deletion. Receipts must survive disk
recreation and controller restarts: delayed requests become no-ops. A manager
requires the matching receipt acknowledgement before admitting a new environment
of the same run on this node. Older controllers that ignore this identity must be
upgraded before that readmission. Support is advertised as
`diskRetirementProtocol: 2`; the manager checks it before issuing deletion as well
as validating the identity in the acknowledgement. An unqualified deletion must
explicitly declare permanent conversation deletion. A manager-owned run namespace
is never erased as native node staging, and its marker is excluded from remote
seeds.

The native controller mounts ext4 directly on ublk using the same durable local
journal and immutable local base blocks as a VM. There is no guest kernel and no
remote disk source. Same-node resume reopens that disk; it does not replace it
with the original project checkout. Transfer staging still carries the original
checkout, but an initialized disk does not consume that seed.

Completed turns fence their processes and flush/unmount the filesystem before
sealing the journal. Local background consolidation reconstructs durable base
blocks and retires sealed segments. While mounted, journal pressure triggers
crash-consistent seals without freezing the filesystem. A controller or machine
crash can lose unflushed kernel pages; acknowledged journal writes remain local.
Missing base blocks prevent mount and corrupt blocks fail authentication on read.

Cairn never reclaims inactive environment blocks for cache pressure or idle age.
The complete disk stays on its node until expiration or deletion. Loss or
revocation of that node loses the working files. History, attachments and published
artifacts remain on the manager. See [local disk operations](CONVERSATION-STORAGE-OPERATIONS.md).

Root-private data is under `/var/lib/cairn-native`. Transfer staging is in `data/`;
journals and local base blocks are in `state/disks/<run>/lazy/`. The mounted conversation
filesystem is `state/native/<run>/data/` under the root-private state directory,
in the system manager's mount namespace. The unprivileged workload gets only its
selected directory binds; the ublk backend has a separate private namespace that
never contains its consumer filesystem. Stopped execution leaves an empty
mountpoint. Restart fences workloads and verifies/unmounts retained owned mounts
before device cleanup and journal recovery.
Temporary files in `state/native/<run>/tmp/` are outside the durable filesystem.
The root-private controller socket is `state/controller.sock`. Do not mount the
disk elsewhere or delete ownership/active markers to force a second writer.

The conversation filesystem size enforces its workspace capacity. The shared node
budget charges retained journals, local base blocks, staging, logs and
artifacts; the mounted view is not charged a second time. Temporary files and
other host-side copies remain monitored limits rather than hard host filesystem
quotas. Physical free-space reserves and journal admission still apply. Exceptional
filesystem growth can need extra temporary capacity. Ordinary remounts reopen the
local journal and base blocks.

Generated runtime configuration, account credentials and frozen skills are bound
from per-attempt staging instead of being copied into persisted blocks. Provider
session files, user tools and workspace/output files stay on the journal-backed
filesystem. This does not detect secrets deliberately written into arbitrary user
files. Native conversations keep a distinct identity and subordinate ID range on
their node. Changing nodes or backends starts an empty environment; existing
container stores are not converted or transferred.

## Updates and removal

Native releases now use `nativeHostProtocol: 1` and `executionProtocol: 2`,
replacing the legacy distribution field `nodeProtocol`. Existing supervisors
refuse this metadata before preparation or draining, so they keep their current
runtime. The execution wire protocol remains 2. Before the first update to this
release, provision Linux 6.11+ and refresh the root-owned supervisor from the
installation's `/internal/nodes/native-host.py` download, then restart
`cairn-native-node.service` during a maintenance window. Verify its healthy
current runtime before allowing the normal candidate update. Do not delete
node state or re-enroll the machine. This explicit supervisor migration is
required even on hosts whose kernel is already compatible.

Revoke the node in Cairn first, then stop/disable `cairn-native-node.service` on its
host. Removing `/opt/cairn-native` and `/var/lib/cairn-native` deletes binaries and
all retained local work; push commits and export files to keep before confirming revocation.
First confirm with `findmnt` that no conversation disk is still mounted below
`/var/lib/cairn-native` after a failed stop. If one remains, start
`sudo systemctl start cairn-native-node` so its recovery releases the disks it owns,
then retry. Removal must never walk into a live filesystem. Delete the directories
and `cairn-workload` account before removing the systemd unit and running
`systemctl daemon-reload`. The Revoke dialog shows one command that lists such mounts
and stops before deleting anything, including when mount enumeration fails. It keeps
the unit file until the directories and account are gone, and skips stopping a unit
that no longer exists, so it can be rerun after any failed step. When no Firecracker
node remains on the machine, it also removes the node gateway. The
installer leaves GPU drivers and the personal desktop session untouched.

Claude's broker and CLI use a separate writable transient secure store for
credentials. The full Claude user directory remains journal-backed, including
root-level instructions/settings, arbitrary tool state, projects, sessions and
installed plugins. Cairn-generated account metadata and configuration are bound
from staging; generated Codex configuration and frozen Cairn skills retain read-only
bindings. Other user tools and model caches in the private home remain durable.

## Node privilege mode

Native protected remains the default. Node details on web and Android can switch
to Native administrator, with an informed owner preview and confirmation, then
back to protected without enrollment or loss of conversation files/history.
Administrator is real host access through unrestricted passwordless sudo, for an
already isolated machine/VM. Transitions stop managed executions before restart;
pending or unconfirmed failure blocks new placements. Returning to protected does
not clean/restore the machine, undo changes or establish a new trust boundary.
Root can read local credentials and defeat its own supervisor and cooperative
limits. See [privilege admission, APIs, recovery and limitations](NATIVE-ADMINISTRATOR.md).

VM nodes use the same mandatory ublk backend and a single flat ext4 filesystem.
Neither backend advertises or requires a host FUSE capability. VM suspension and
live memory reclamation preserve the running VMM; memory snapshots and the paired
filesystem layout are no longer supported.

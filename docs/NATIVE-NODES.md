# Operating a native node

Native execution is for selected agents used by trusted people. It runs host GPU
APIs under a managed account while the owner continues using the computer. It
provides less isolation than Firecracker. GPU memory exhaustion, desktop slowdown
and driver bugs remain possible; no software boundary can promise a GPU job will
never crash a driver. Review code before testing on a personal workstation.

## Requirements and enrollment

Use Linux x86-64 with systemd 252+, cgroup v2, Python 3, curl, useradd/usermod,
e2fsprogs, io_uring and a working `ublk_drv` kernel driver. Host libraries must be
compatible with the published coding engines. The installed GPU
driver, render/video device permissions and application libraries must already
work. Cairn does not install GPU drivers, detach a GPU or close a graphical session.
Enrollment loads the existing ublk storage module and checks its control device;
unsupported kernels must be provisioned before native execution can start.
The installation manager and its S3 storage must be configured normally.

As the installation owner, open Nodes, add a machine and choose Native process.
Use the generated installer on that node and enter the single-use code when asked.
The installer creates `cairn-workload`, installs a root-owned systemd controller,
and leaves native execution disabled. In Configure, explicitly enable native work,
select trusted people and conservative CPU/RAM/disk limits. Configure a native
agent and select this node. Other agents retain their Firecracker backend.

Start a new conversation as a trusted person. An authorized coordinator can also
start a conversation with that native agent through its installation tools. Each
follow-up, resume, scheduled dispatch and delegated run retains and rechecks its
originating permission. Previously unattributed conversations and external plugin
origins cannot silently gain host access. When changing an existing agent's backend,
start a new conversation; environments cannot switch backend in place.

## Switching an existing machine

A node keeps the execution backend it enrolled with; a heartbeat reporting another
backend is refused. To move a Firecracker machine to native execution, or back, use
Switch to native (or Switch to VM) on its node. Cairn pre-fills the machine name
and the other backend. Run the generated installer on the same machine: the new
runtime enrolls as a separate node with its own identity, grants and storage,
alongside the existing one. After its configuration and agent step, Cairn offers to
retire the previous node. The invitation assigns the replacement's identity before
it connects, so another enrollment cannot advance this switch. Older invitations
remain consumable; installations without identity tracking keep the previous node
and ask for an update before completing a guided switch.
Revoking it from there also returns conversations fixed
to or preferring that node to automatic placement; a plain revocation keeps those
explicit choices waiting. Switch each agent's backend on its page and allow the
new node. Existing conversations show **Migrate this conversation…** in their
execution-node details on web and Android. Confirm the destination to preserve the
same conversation, history, private home, workspace and output. Execution stops;
the next message opens a fresh engine session with the conversation context.
System packages and running processes do not transfer; dependencies may need
reinstalling. Migration needs a coherent source capture, S3 access, temporary
manager disk space, fakeroot and e2fsprogs. An interrupted or
failed migration retains the source and stays paused; retry from the same action.
Legacy native file archives must resume once on their original node first.
Finally run the uninstall command shown for the previous runtime after the
conversations you want to keep have migrated.

The public Cairn MCP and the `cairn_installation` agent tool expose the same setup
and retirement controls: `list_nodes`, `list_native_callers`,
`create_node_enrollment`, `update_node`, `update_node_storage` and `revoke_node`. Installation management
access is required for mutations; read-only public MCP access cannot enroll,
configure or revoke a node. Agent installation grants are checked again in each
mutation transaction. Enroll the other backend, wait for the returned `nodeId`,
configure its native trust when applicable, and grant agents through `update_agent`.
Only after the replacement is ready, revoke the old identity with
`releasePlacements: true`. Plain revocation preserves existing placement choices.
`get_chat_backend_migration` previews the saved/profile backend difference and
authorized destinations. `migrate_chat_backend` requires `chatId`, `nodeId`, the
profile's current `backend` and `confirm: true`; it returns `status: "migrating"`.
Poll the preview until `pending` is false and `required` is false. An `error` with
`required: true` means the source remains and migration can be retried. These tools
neither install nor uninstall a runtime on the host.

## Daily use

The controller uses one slot, low CPU/I/O priority and conservative default budgets.
Keep interactive applications running during qualification. Start with a short,
small GPU workload and inspect memory use and desktop responsiveness before larger
jobs. VRAM has no universal hard per-agent quota. Disabling native execution or
removing a trusted person stops affected work after its remaining lease expires.
Turning off admission prevents new placement without automatically canceling work.
An offline Beacon authority check eventually stops native work too.

The managed filesystem contains system binaries/libraries read-only, a private
HOME, project files, output, temporary files and GPU devices. The personal home,
desktop sockets, Docker daemon and administrator privileges are unavailable.
Graphics tests must use headless/offscreen APIs. Cairn bundles the coding engines,
Node.js and `gh`; other toolchains use this node's installed binaries/libraries or
user-managed tools under the private home. Respect repository version pins.

Skills are the run's frozen bundle, copied afresh for each attempt. Accounts and
MCP access use the usual scoped brokers. Credentials provided to an engine are
available to that engine. Do not assume the checkpoint exclusions detect secrets
written into arbitrary project files.

## Storage and recovery

The native controller mounts ext4 directly on ublk, using the same local journal,
scoped S3 block reads, encrypted immutable publications and cache as a VM. There
is no guest kernel. Same-node resume reopens that disk and loads missing blocks on
demand; it does not restore the current workspace from a complete file archive
or replace it with the original project checkout. The remote connector still
transfers the original checkout in attempt staging, as it does for VM nodes; an
initialized disk does not consume that seed. Lazy disk resume therefore does not
mean total startup traffic consists only of demanded blocks. See the
[storage measurements](NATIVE-NODES-MEASUREMENTS.md) for the measured boundary.

Periodic live checkpoints are crash consistent. Completed turns fence their
processes and flush/unmount the filesystem before sealing their completed journal
prefix. Final S3 publication can finish asynchronously while a later turn starts.
A controller or machine crash can lose unflushed kernel pages; committed local
journal writes remain recoverable. Missing or unauthorized S3 bytes never become
zero-filled files. Unavailable storage can delay I/O and shutdown until recovery;
keep the node available for synchronization. Sync errors retain the journal and
surface in the execution-node details.

After one idle hour, Cairn automatically reclaims a terminal conversation's clean
local block cache and obsolete generated attempt files. The sweep checks bounded
batches every minute, so reclamation can occur later than the one-hour threshold.
It requires the exact latest acknowledged S3 generation and no active attempt,
pending publication or movement. Unpublished journal writes are never evicted. The
journal metadata, base manifest, scoped grant and pinned runtime remain, so the
next execution can read its files from S3 on demand. Chat history, published
artifacts and shared engine/tool bundles remain available.

Root-private data is under `/var/lib/cairn-native`. Transfer staging is in `data/`;
journals and clean cache are in `state/disks/<run>/lazy/`. The mounted conversation
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
budget charges retained journals, caches, migration copies, staging, logs and
artifacts; the mounted view is not charged a second time. Temporary files and
other host-side copies remain monitored limits rather than hard host filesystem
quotas. Physical free-space reserves and journal admission still apply. First-time
migration from native file storage, and exceptional filesystem growth, can need
extra temporary capacity. Ordinary remounts keep demand loading.

Generated runtime configuration, account credentials and frozen skills are bound
from per-attempt staging instead of being copied into persisted blocks. Provider
session files, user tools and workspace/output files stay on the journal-backed
filesystem. This does not detect secrets deliberately written into arbitrary user
files. A different workload UID on a destination can require a one-time ownership
reconciliation of filesystem metadata.

Existing stopped `native-files-v1` checkpoints remain readable. Local file work is
copied into the block filesystem while retaining the old source until its new S3
point is acknowledged. Displaced recovery copies remain under `stale-*` for explicit
prune/delete. Restore staging is journaled; retry that same recovery point after an
interruption. A failed restore cannot silently fall back to an original seed.

Cross-node recovery requires native Linux x86-64, ublk and the pinned coding-engine
runtime on the destination. Different installed application dependencies can still
prevent a project from running. Native-to-VM and cross-OS environment conversion
remain unsupported. This storage change advances installation data epoch 6 to 7;
the [reviewed automatic transition to epoch 9](AUTOMATIC-MIGRATIONS.md) qualifies its
conversion and recovery tests alongside the rules in
[ADR 0052](adr/0052-native-ublk-workspaces-and-idle-eviction.md).
An older native binary must not be started against the new state.

## Updates and removal

The root service polls approved manager releases. It verifies bounded payloads,
checks binaries before draining, synchronizes current work, then health-checks the
candidate. Failed compatible updates retain the prior version; epoch transitions
stop for reviewed intervention. Existing local conversations keep their pinned
engine bundle. Inspect `journalctl -u cairn-native-node` and the node maintenance
status for update failures. Enrollment and update credentials stay root-private.

Revoke the node in Cairn first, then stop/disable `cairn-native-node.service` on its
host. Removing `/opt/cairn-native` and `/var/lib/cairn-native` deletes binaries and
all retained local work; do this only after synchronization and recovery review.
First confirm with `findmnt` that no conversation disk is still mounted below
`/var/lib/cairn-native` after a failed stop. If one remains, start
`sudo systemctl start cairn-native-node` so its recovery releases the disks it owns,
then retry. Removal must never walk into a live filesystem. Delete the directories
and `cairn-workload` account before removing the systemd unit and running
`systemctl daemon-reload`. The Revoke dialog shows one command that lists such mounts
and stops before deleting anything, including when mount enumeration fails. It keeps
the unit file until the directories and account are gone, and skips stopping a unit
that no longer exists, so it can be rerun after any failed step. The
installer leaves GPU drivers and the personal desktop session untouched.

Claude's broker and CLI use a separate writable transient secure store for
credentials. The full Claude user directory remains journal-backed, including
root-level instructions/settings, arbitrary tool state, projects, sessions and
installed plugins. Cairn-generated account metadata and configuration are bound
from staging; generated Codex configuration and frozen Cairn skills retain read-only
bindings. Other user tools and model caches in the private home remain durable.

# Operating a native node

Native execution is for selected agents used by trusted people. It runs host GPU
APIs under a managed account while the owner continues using the computer. It
provides less isolation than Firecracker. GPU memory exhaustion, desktop slowdown
and driver bugs remain possible; no software boundary can promise a GPU job will
never crash a driver. Review code before testing on a personal workstation.

## Requirements and enrollment

Use Linux x86-64 with systemd 252+, cgroup v2, Python 3, curl, useradd/usermod, and
host libraries compatible with the published coding engines. The installed GPU
driver, render/video device permissions and application libraries must already
work. Cairn does not install drivers, detach a GPU or close a graphical session.
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

Local workspace changes persist between turns. Stopped checkpoints publish to S3;
active work has no live coherent backup. A sudden power loss can lose changes
since the last checkpoint, just as other unsynchronized local work can. Keep the
node available long enough for final synchronization. Sync errors retain local
files and surface in the execution-node details.

Root-private data is under `/var/lib/cairn-native`: transfer staging in `data/`,
conversation files in `state/native/<run>/data/`, stopped archives in `state/snapshots/`.
The local controller socket is root-private at `state/controller.sock`.
Disk budgets cover the shared logical size of all retained conversations, archives,
seed/restore staging, logs and artifact copies on this node. Each conversation also
has its own workspace ceiling; those ceilings are not preallocated reservations.
Controlled copies use the remaining shared budget, and running work is stopped
when either ceiling is exceeded. These are monitored limits, not hard filesystem
quotas. Checkpoint creation needs spare space for a full framed archive and its
metadata while the prior archive is retained, plus a safety reserve.
Previous work displaced by restore remains in `stale-*` until explicit prune or
delete. Restore staging is journaled. If a restore is interrupted, retry that same
backup before resuming execution. Never delete a lock or active marker to force
concurrent writers. Stop the service before manually inspecting or repairing state.

Stopping for maintenance waits for fencing, final capture and workspace ownership
release within the maintenance deadline. A failed capture keeps local work and
invalidates its previous checkpoint. After freeing space or fixing the cause,
retry synchronization: the stopped capture is retried safely, and only a successful
replacement clears the error.

Cross-node recovery requires native Linux x86-64 and the pinned coding-engine
runtime on the destination. Different installed application dependencies can still
prevent a project from running. Native-to-VM and cross-OS environment conversion
are not supported. The immutable source and artifact protocols remain shared.

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
Remove its systemd unit and run `systemctl daemon-reload` when uninstalling. The
installer leaves GPU drivers and the personal desktop session untouched.

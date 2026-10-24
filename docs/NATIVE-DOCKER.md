# Docker in native conversations

Native Linux conversations have a private rootless Docker engine. Use `docker
build`, the default Buildx builder, `docker run` and `docker compose` from the
conversation's shell. The daemon starts on the first command requiring a server;
client version/help commands do not start it. Images are pulled only when needed.
The host's Docker daemon, personal home and desktop sockets are never mounted.

## Boundary and lifecycle

Each conversation has its own nologin host account and non-overlapping 65,536-entry
subordinate UID and GID ranges. Namespace-root host UIDs/GIDs are explicitly
allocated from 500,000,000–501,048,575, outside the host's small system-service
account pool, skipping host identities and subordinate ranges. A durable sequence
prevents reuse even after interrupted account creation. Actual kernel UID/GID
maps are checked against the reserved identity before engine admission. RootlessKit starts a private user, mount, PID, IPC,
UTS and slirp network namespace under that account. The engine runs as namespace
UID/GID 1000 with an empty capability bounding set and NoNewPrivileges. Namespace
root is an unprivileged host UID, reserved for the immutable enclave supervisor
and rootless daemon. Accounts and ranges are retained with the conversation and
are never reassigned to another conversation by Cairn.

The outer RootlessKit bootstrap needs the distribution's `newuidmap` and `newgidmap` helpers, using either
setuid or only their effective global CAP_SETUID/CAP_SETGID file capabilities. Its unit permits that transition under the unique account;
the enclave sets NoNewPrivileges before starting the engine or daemon. Bootstrap
and daemon helpers use an immutable system/managed PATH; user-installed helpers
are available only to the engine after isolation. Managed
roots, read-only system bindings, device filtering, cgroup protection and the
existing guardian remain. The bootstrap retains module/clock/raw-I/O/reboot/swap
syscall filters and excludes their capabilities. Proc/sys/cgroup read-only masks
are installed inside the new namespace before the engine starts: inherited locked
systemd submounts prevent RootlessKit from creating private procfs/sysfs. A failed
private procfs check refuses execution; it never accepts the host proc view.
Network sysctls remain configurable by namespace root for Docker's private bridge;
engine UID 1000 cannot write them, and they do not address the host network. No systemwide removal of user namespace restrictions
or privilege hardening is required. On restricted AppArmor hosts the installer
loads a profile allowing user namespaces only for the managed RootlessKit binary.

Docker uses the same agent-visible workspace paths. Bind sources must already
exist and contain no symlink components; create source directories from the shell
first, or use ordinary named volumes. A mandatory authorization
plugin refuses privileged containers/exec, added capabilities, host PID/IPC/UTS
namespaces, devices, alternate runtimes, volume driver options and mounts outside
the conversation's home, workspace, output and temporary directories. Recursive
bind restrictions are retained; workspace binds require `:ro` in read-only mode.
The public Docker socket is a streaming gateway; the daemon socket is root-private.
JSON controls require their canonical Docker field names and reject duplicate
object keys, including nested fields and Unicode case aliases. This keeps the
authorization policy consistent with Docker's case-folding and merging decoder;
labels and other free-form maps retain ordinary user keys.
Before forwarding a container create, the gateway resolves bind sources with
`openat2` without symlinks, clones the pinned mount descriptors, and gives Docker
immutable private mirror paths. Replacing a source name cannot redirect Docker
to private runtime or store files. Mirrors are rebuilt from durable source records
before daemon startup and rechecked against the current turn's rights. Runtime
mirrors are limited to 4,096 per turn, including deleted-container mirrors pending
reclamation; a new turn reclaims unreferenced mirrors. This bounds metadata outside
the ext4 payload as well as the payload's normal disk budget.
The read-only system/credential bindings remain read-only through Docker too.
YOLO does not grant host administrator rights. The engine cannot read or replace
raw daemon metadata, its authorization plugin or its configuration.

RootlessKit has no parent port driver. Published container ports listen only in
the conversation's network namespace, where the agent can reach them on localhost.
Two conversations may use the same published port. Publishing `0.0.0.0:8080:80`
does not publish on the node's LAN or public interfaces. There is no new public
service-sharing UI: use the existing artifact publication tools for deliverables.
Host loopback access through the slirp gateway is disabled.

The daemon, containerd, builds, containers and slirp remain descendants of the
existing workload unit. Its per-conversation ceilings and shared node slice bound
CPU/RAM/PIDs; it cannot delegate processes into another cgroup. The existing
freeze monitor freezes them together. Stop, cancellation, revocation/lease expiry,
guardian death and controller death fence the entire unit before storage unmount.
Container restart policies are refused: recreate/start development services in
each new turn, after its current policy is checked. No daemon runs between turns.

## Persistence and compatibility

Images, BuildKit cache, named volumes and stopped container metadata reside in
`/docker` on the conversation's journal-backed ext4 disk. Runtime sockets, PIDs,
network namespaces and processes are ephemeral. They never travel in checkpoints.
Live publication is crash consistent; completed turns fence all processes and
flush/unmount before sealing. A crash can lose unflushed pages, including database
transactions not yet synced by a container. S3 publication failure retains local
work and the existing unsynchronized-work status.

Same-node resume retains Docker state without a directory-wide ownership pass.
Cold restore, acknowledged-cache eviction and movement to another native node use
the same encrypted block publication and demand reads as the other disk bytes.
The UID/GID mapping travels inside the disk; a destination reconciles owners while
preserving container users, volume modes, POSIX ACL identities and namespaced
file capabilities. Durable transition and per-batch metadata journals allow an
interrupted ownership change to finish before a subsequent destination remapping.
Startup completes pending inode recovery before refreshing empty generated
configuration mount targets; their transient source files remain outside the disk.
A destination needs compatible
managed Docker tools and Linux native prerequisites. It does not restore running
containers, network connections or process memory. Disk usage counts against the
existing conversation filesystem and shared node disk budget; image pulls and
build caches can exhaust them. Use ordinary Docker image/build/volume cleanup.

Native-to-VM migration currently refuses a nonempty rootless Docker store and
retains the source environment. The VM's rootful daemon cannot consume this
metadata, and the existing file converter does not preserve its ownership,
hardlinks, whiteouts and extended attributes. Export needed images (`docker save`)
and volume contents into the workspace before clearing Docker state with
`docker cairn-reset --confirm-delete-state`. This command permanently deletes
all conversation Docker images, containers, caches and volumes after confirmed
daemon shutdown; ordinary workspace/home/output files remain. Native
conversations without Docker keep their existing conversion path. VM Docker state
is not imported into the rootless daemon during VM-to-native conversion.

This is installation data epoch 13 / SQLite version 12. The native runtime identity
also records `nativeIsolation: rootless-v1`, so previous controllers' complete
identity comparison rejects the disk before changing its file owners, including
after cross-node restore. See the [forward transition](AUTOMATIC-MIGRATIONS.md).
Do not roll back to an old controller or lower compatibility markers. This change
does not release or deploy any runtime.

## Requirements and limitations

The native installer provisions verified, versioned binaries under its managed
directory: Docker 29.9.0, RootlessKit 3.3.0, slirp4netns 1.3.6, Buildx 0.38.1 and
Compose 5.6.1. Exact archive/executable SHA-256 pins are in `native-host.py`.
The host needs Linux 5.12+, systemd 255+, cgroup v2, distribution `uidmap`,
`iproute2`, `iptables`, `/dev/net/tun`, root-managed `/etc/subuid` and `/etc/subgid`,
and the existing ublk/ext4 prerequisites. Mapping helpers must be root-owned, executable and not writable by workloads,
with setuid or the exact effective global mapping capability. Unsupported prerequisites fail explicitly.
The installer does not install/start a host Docker daemon or alter GPU drivers.

Rootless Docker is not full host Docker compatibility. NoNewPrivileges also
applies to containers, so setuid binaries cannot grant additional privilege. Privileged containers,
GPU containers, Docker plugins/Swarm, alternate volume drivers and insecure build
entitlements are unavailable. Buildx's default Docker driver is supported; its
privileged docker-container driver is refused. Volumes use container UID/GID
semantics: applications may need `chown` through a normal container. Source IPs,
MTU/throughput and privileged-port behavior follow slirp/rootless constraints.
Per-container cgroup flags are not the node's budget mechanism: with no delegated
systemd user bus Docker may report cgroup driver `none` and ignore those flags.
The enclosing workload/slice limits remain enforced for every descendant.

Primary references: [Docker rootless requirements](https://docs.docker.com/engine/security/rootless/),
[rootless operation and resource limits](https://docs.docker.com/engine/security/rootless/tips/),
[supported drivers and limitations](https://docs.docker.com/engine/security/rootless/troubleshoot/).

## Qualification and measurements

The `native-runtime` CI job uses a disposable Linux host, synthetic credentials
and loopback Moto S3. It executes the shipped native controller and entrypoint,
the real systemd/ublk/ext4 stack and the managed Docker tools. Reference binaries
use the PR base or main push's previous commit; manual runs and tags use the
checked-out candidate's first parent, fetching shallow history when needed. They
may be reused only for the same exact base, pinned Rust/profile/features and runner
image, with recorded binary checksums and original build run. The complete reference
qualification still runs on the current host before candidate measurements. No qualification
command runs against the owner's desktop node or its Docker daemon.

`native-storage.json` records the host/kernel/build profile, shared budgets,
startup samples, cold daemon startup before pulls, warm daemon commands, image
pull time/digests separately, build/cache resume and restore, storage read/write
wall and process CPU times, encoded S3 bytes and cgroup limits/accounting.
Host load averages (1/5/15 minutes) and aggregate CPU ticks bracket the fixture.
Startup samples include CPU charged to the fresh workload cgroup at readiness;
short executions may already be collected and report unavailable CPU as null.
Docker timings include matching cgroup CPU deltas and observed host load. These
are aggregate client/daemon/helper/service costs, not exclusive daemon CPU.
Compare no-Docker startup samples separately: Docker-marked readiness includes
the complete qualification workload, while `firstDaemonMs` measures only the
first server command before image pulls. Cold here means a new conversation
store and daemon, without forcing the host's executable page cache cold.
A static C probe records identical direct-host and named-volume 8 MiB read / 128
fsynced 4 KiB write workloads separately from the seeded native filesystem probe.
The destination-ID test emulates unavailable source IDs on the same disposable
host; it is not a physical two-node or network-transfer benchmark.
Cold file reads use advisory `POSIX_FADV_DONTNEED`; eviction also clears userspace
cache. This is local synthetic S3 evidence, not production WAN latency. Results
must be attached from successful CI before the PR is considered qualified; no
unavailable measurement is represented as zero or as a passed test.

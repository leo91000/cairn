# One-command installation

For an installation sharing the beacon `https://cairn.build` production server,
follow the [same-server configuration guide](PRODUCTION-CAIRN-SETUP.md). Reserve
memory/disk for the beacon process, Postgres and proxy before assigning
installation VM budgets.

Sign in to the Beacon and choose **Add an installation**. Copy the
complete `curl … | sudo bash` command onto a trusted Linux x86-64 machine. Its
single-use code lasts ten minutes and is passed by the launcher through a private
environment to `host.py`, never in a download URL. The copied shell command can
still appear in shell history and sudo audit logs; its code is short-lived and
single-use. The downloaded script executes only when complete, requires HTTPS,
and verifies the embedded SHA-256 of `host.py` before installing it. No local password, domain, certificate, incoming port
is needed. The installation connects out using the existing claim and relay.

Use a host with systemd. Install Docker Engine with its Compose plugin, Python 3 and curl first. Enable
hardware virtualization, `/dev/kvm`, `/dev/net/tun` and a working `ublk_drv` kernel module
(`/dev/ublk-control`, with io_uring enabled). The script checks each requirement before
downloading or starting services, including a working Docker daemon and 16 GiB
of free disk for runtime images and recovery staging. Conversation disks need
additional space. It does not install Docker or change virtualization settings.

The root-owned directory `/var/lib/cairn-installation` holds the installation.
Compose starts the manager and its local runner with restart policies and
rotated logs. No service publishes a host port; no container receives the Docker
socket. Private identity and agent credentials must stay on this host. Do not
publish the directory or `docker compose config` output.

The manager's origin, `http://manager:4310`, exists only on that private Compose
network, which the manager and local runner use to communicate. Additional
nodes enroll and connect to the manager over HTTPS; HTTP is accepted only on
loopback. A node added without
an address uses that loopback exception: its local gateway carries each connection
to the manager, directly or through Beacon, encrypted to the manager's pinned node
certificate, so this installation still publishes no port. See
[additional execution nodes](DEPLOYMENT.md#additional-execution-nodes).

Agents never use this origin: their VMs cannot resolve or reach it. Inside a VM,
`cairn_workspace` and MCP connections use `http://127.0.0.1:5202`. The guest
relays it over vsock to the runner, which forwards it to a private socket in the
run's directory, served by the manager during the attempt. A remote node's
connector serves that socket instead and forwards each request over its
authenticated manager connection. The VM firewall, the published ports and the
manager's network are unchanged. See [ADR-0035](adr/0035-vm-local-mcp-channel.md).

Reruns retain the chosen immutable Cairn image, identity and persistent data. Concurrent installers are refused. An incomplete or unsafe identity stops the
rerun with recovery instructions and is retained rather than overwritten. The one-use
claim environment is cleared after success or failure; after startup the manager
is recreated without it. A refused or expired code leaves Cairn running unclaimed.
Obtain a new command and rerun it, or run `sudo cairn claim` and confirm its code in
the beacon app. After the fallback command confirms success, restart the manager
to load its new relay identity:

```sh
sudo docker compose --project-directory /var/lib/cairn-installation \
  -f /var/lib/cairn-installation/compose.json restart manager
```

No installation is accessible before claiming. Refresh
installations in the app after the command finishes.

## Local conversation disks

A conversation's working environment stays on its execution node. It has no
remote copy: restarting on the same node reopens its local disk; changing node,
including between VM and native execution, requires explicit confirmation and
starts an empty environment. Losing a node loses its working files. Conversation
history, uploaded attachments, published artifacts and public artifact links stay
on the manager and are retained when an environment expires.

Back up the manager data and each node's disks with their services stopped. Push
commits and save working files before changing nodes. See
[conversation storage operations](CONVERSATION-STORAGE-OPERATIONS.md) for disk
capacity, retention and recovery.

New installations have no Garage service and need no bucket or S3 credentials.
Updating an older installation expires environments that depend on S3, without
copying their files back. The [migration and release note](migrations/0032-local-conversation-disks.md)
describe this loss: push commits and
save files to keep before updating. The update does not erase old
`data/storage-s3.json`, `garage.env`, `garage.toml`, Garage data, external buckets
or environment values. Rerunning the installer leaves those files untouched.
The update supervisor retains any existing Garage service. Once rollback is no
longer needed, the operator can stop and remove that service and its data,
credentials and bucket. Do not remove unrelated Docker services or volumes.

Rollback requires a consistent manager database backup taken by the operator
before the update and the matching bucket. Automatic updates create no backup;
starting an older binary alone does not restore lost working files. Only their
last published generation can be recovered from that backup and bucket.

## Automatic approved updates

The installer enables `cairn-installation-update.timer` on the host. It checks the
saved beacon origin's `/install/release` every five minutes, with up to thirty
seconds of jitter, and two minutes after boot. The operator selects the tested
immutable image using `CAIRN_INSTALLATION_IMAGE`; no mutable tag is accepted.
Downloads are verified against Docker's repository digest before any restart.
If the Beacon or registry is unavailable, the current image keeps running.
The update channel is independent of the relay, so an incompatible installation
can still update.

The supervisor pulls and verifies the approved image, then takes the persisted
deployment lease to pause new work. It stops the manager gracefully to checkpoint
active conversations, then stops the local runner. It replaces both images
together, preserving Compose settings, identity, agent credentials,
workspaces and runner state. Compose checks both containers; the manager's health
must report the downloaded image's runtime identity before the image is committed
and the lease released.

A durable pending candidate lets the next timer retry an interrupted replacement,
including an exited or unresponsive candidate container. Failed health leaves the
candidate pending for repair and retry. The supervisor does not start a previous
binary against the data. The replacement's stop, readiness and verification
bounds remain within the twenty-minute lease.

Version-11 manager databases support the automatic
[data-preserving upgrade to version 12](migrations/0012-typed-node-inventory.md).
Direct-storage releases continue with the preserving
[version 13 metadata upgrade](migrations/0013-direct-node-storage.md).
Retain a consistent database/WAL backup before upgrading; automatic updates create
no backups. Preserve installation identity and node state; retain legacy object storage
while a rollback remains possible. Other
schema transitions require their own documented compatibility path; an unsupported
version is not an instruction to reinstall or erase data.

```sh
sudo systemctl status cairn-installation-update.timer
sudo systemctl start cairn-installation-update.service
sudo journalctl -u cairn-installation-update.service
```

The timer and installer share the same nonblocking host lock. Containers receive
no Docker socket. Rerun the current beacon installation command to refresh the
checksum-verified host supervisors on an existing one-command installation. Recovery
renews the recorded deployment lease before stopping a running manager, because
a durable acknowledgement may outlive the lease. If the interrupted replacement
already removed/stopped it, or the exact recorded candidate has never started, is
crash-looping or is running without a healthy HTTP endpoint, the host lock and
recorded acknowledgement permit retrying the same pending candidate. The
supervisor never restores a previous image. An unrelated or functioning manager
still requires lease acquisition.

## Operation and validation

```sh
sudo docker compose --project-directory /var/lib/cairn-installation \
  -f /var/lib/cairn-installation/compose.json ps
sudo docker compose --project-directory /var/lib/cairn-installation \
  -f /var/lib/cairn-installation/compose.json logs --tail 50
```

For a disposable test, build the current binaries and web output, then run
`bash tests/installation-installer-container.sh`. It uses the real beacon
service, manager and Postgres. All Docker lifecycle commands (pull,
startup, recreation and container health execution) are simulated by an adapter
that starts the manager binary as a fixture process. The generated configuration
is independently parsed by real `docker compose config`, including healthchecks,
claim substitution, dependencies and uid 1000. Loopback HTTP download transport
is adapted only in the fixture; the production script requires HTTPS. Inert
devices and command adapters exercise prerequisite and checksum failures.
Claim, relay, deployment leases and runtime health checks are real. The fixture
also verifies an approved update and repair/retry of a wrong-runtime candidate,
preserving relay identity. No VM boots; this is not runtime/KVM coverage.
Containers, network and data are cleaned up on exit.
`CAIRN_BEACON_TEST_DATABASE_URL=… python3 tests/installation_update_containers.py`
also replaces real manager and readiness-runner containers using locally built
fixture images, including interrupted recovery with `restart: unless-stopped`.
Only the registry is adapted; Compose, persistent mounts, runtime
metadata, deployment leases, recovery from a running candidate with a dead HTTP
endpoint, relay reconnection and conversation reads are real.
It checks successful replacement and retry of exited and HTTP-dead candidates, including
identity, synthetic credentials, workspaces and runner state. The readiness runner
boots no VM; active VM checkpointing remains covered by the existing node tests.
`python3 scripts/tests/installation_installer_test.py` checks failure cleanup and
idempotent configuration separately.

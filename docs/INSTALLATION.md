# One-command installation

For an installation sharing the beacon `https://cairn.build` production server,
follow the [same-server runbook](PRODUCTION-CAIRN.md). Reserve memory/disk for the
beacon process, Postgres and proxy before assigning installation VM budgets.

Sign in to the Beacon and choose **Add an installation**. Copy the
complete `curl … | sudo bash` command onto a trusted Linux x86-64 machine. Its
single-use code lasts ten minutes and is passed by the launcher through a private
environment to `host.py`, never in a download URL. The copied shell command can
still appear in shell history and sudo audit logs; its code is short-lived and
single-use. The downloaded script executes only when complete, requires HTTPS,
and verifies the embedded SHA-256 of `host.py` before installing it. No local password, domain, certificate, incoming port or S3 account
is needed. The installation connects out using the existing claim and relay.

Use a host with systemd. Install Docker Engine with its Compose plugin, Python 3 and curl first. Enable
hardware virtualization, `/dev/kvm`, `/dev/net/tun` and `/dev/fuse` (install
`fuse3` and load the kernel modules). The script checks each requirement before
downloading or starting services, including a working Docker daemon and 16 GiB
of free disk for runtime images and recovery staging. Conversation disks need
additional space. It does not install Docker or change virtualization settings.

The root-owned directory `/var/lib/cairn-installation` holds the installation.
Compose starts the manager, its local runner and Garage with restart policies and
rotated logs. No service publishes a host port; no container receives the Docker
socket. Private identity and storage credentials must stay on this host. Do not
publish the directory or `docker compose config` output.

The manager's origin, `http://manager:4310`, exists only on that private Compose
network. At startup the manager records it in the data directory it shares with
its local runner, which then reads conversation disks from exactly this origin.
Every other controller keeps the HTTPS rule: remote nodes enroll, connect and
read disks over HTTPS, HTTP being accepted only on loopback for tests.

Agents never use this origin: their VMs cannot resolve or reach it. Inside a VM,
`cairn_workspace` and MCP connections use `http://127.0.0.1:5202`. The guest
relays it over vsock to the runner, which forwards it to a private socket in the
run's directory, served by the manager during the attempt. A remote node's
connector serves that socket instead and forwards each request over its
authenticated manager connection. The VM firewall, the published ports and the
manager's network are unchanged. See [ADR-0035](adr/0035-vm-local-mcp-channel.md).

Reruns retain the chosen immutable Cairn image, identity, Garage keys, storage
settings and persistent data. Concurrent installers are refused. An incomplete or unsafe identity stops the
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

## Integrated storage and external S3

Garage 2.3.0 is pinned by digest. Its
[single-node mode](https://garagehq.deuxfleurs.fr/documentation/quick-start/)
creates the bucket and credentials automatically, with one small storage process
and no provisioning client. Data and metadata persist in the installation
directory. Only this integrated configuration accepts `http://garage:3900` on the
private Compose network; external storage uses HTTPS. S3 remains mandatory for
every disk under ADR-0009. Garage has no object versioning; collection deletes
exact object keys directly. External versioned providers retain the existing
version and delete-marker cleanup.

In **Settings → Conversation storage**, the owner supplies an external HTTPS
endpoint, bucket, region and credentials. Use a dedicated private bucket without
active lifecycle rules or Object Lock, allowing object read/write/delete.
Validation and a write/read/delete probe run before saving. Credentials stay in
the private manager configuration and are never returned to the browser.

Cloudflare R2 uses `auto` as its region. Its
[S3 interface](https://developers.cloudflare.com/r2/api/s3/api/) cannot report
public domains, bucket locks or ACLs. For R2 endpoints the owner must confirm in
the form that `r2.dev` and public custom domains are disabled and bucket locks
are absent in the Cloudflare dashboard. Lifecycle rules and object access are
still checked. Other providers retain the existing privacy and Object Lock
checks. R2 uses its own encryption at rest; Cairn does not send unsupported SSE-S3
headers or version-list operations.

The external target becomes the default for new disks. Existing disks keep using
their original storage and its retained credentials. This does not migrate disk
data; keep Garage and its directory while disks still use it. Local S3 does not
protect against losing the machine. Back up the entire installation, including
Garage, with the services stopped. Never discard volumes containing needed data.

Nonempty `STORAGE_S3_*` or legacy `ARCHIVE_S3_*` overrides remain authoritative;
remove them before editing the default in the app. For an environment-managed R2
endpoint, set `STORAGE_S3_PRIVATE_BUCKET_CONFIRMED=true` only after checking in the
Cloudflare dashboard that public domains and bucket locks are disabled. This
explicit server confirmation replaces the form confirmation; `false` refuses R2.

## Automatic approved updates

The installer enables `cairn-installation-update.timer` on the host. It checks the
saved beacon origin's `/install/release` every five minutes, with up to thirty
seconds of jitter, and two minutes after boot. The operator selects the tested
immutable image using `CAIRN_INSTALLATION_IMAGE`; no mutable tag is accepted.
Downloads are verified against Docker's repository digest before any restart.
If the Beacon or registry is unavailable, the current image keeps running.
The update channel is independent of the relay, so an incompatible installation
can still update.

The supervisor reuses the node supervisor's pull/prepare/launch/rollback cycle.
It takes the existing persisted deployment lease to pause new work, stops the
manager gracefully to checkpoint active conversations, then stops the local runner.
It replaces both images together, preserving Compose settings, Garage, identity,
agent credentials, workspaces and runner state. Compose checks both containers;
the manager's health must report the downloaded image's runtime identity.
Failed health restores and verifies the previous approved image before releasing
the lease. Each replacement has a 550-second maximum, including Docker inspection,
graceful stops, runner readiness and HTTP runtime verification; candidate and
rollback, including checks for a failed candidate container, together remain within
the twenty-minute lease. A journal written before replacement lets the next timer
restore the last committed image after interruption. A refused lease or interrupted
supervisor leaves the same approved digest eligible for the next check. Only a
candidate confirmed unhealthy by its runtime probe or by Docker container health
is retained in
`installation.json` and is not retried automatically until the approved digest
changes. After diagnosing and fixing a host problem, the operator may remove
`failedImage` from that private file to retry the same approved digest.

Only approve releases whose installation database remains readable by the previous
release; this ticket adds no installation database migration. An image update
cannot undo a destructive database migration. Keep the stopped-installation backups
required above before approving a release with a new data format.

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
crash-looping or is running without a healthy HTTP endpoint, the host lock and recorded acknowledgement permit restoring only the previously
committed approved image. An unrelated or functioning manager still requires
lease acquisition.

## Operation and validation

```sh
sudo docker compose --project-directory /var/lib/cairn-installation \
  -f /var/lib/cairn-installation/compose.json ps
sudo docker compose --project-directory /var/lib/cairn-installation \
  -f /var/lib/cairn-installation/compose.json logs --tail 50
```

For a disposable test, build the current binaries and web output, then run
`bash tests/installation-installer-container.sh`. It uses the real beacon
service, manager, Postgres and Garage. All Docker lifecycle commands (pull,
startup, recreation and container health execution) are simulated by an adapter
that starts the manager binary as a fixture process. The generated configuration
is independently parsed by real `docker compose config`, including healthchecks,
claim substitution, dependencies and uid 1000. Loopback HTTP download transport
is adapted only in the fixture; the production script requires HTTPS. Inert
devices and command adapters exercise prerequisite and checksum failures.
Claim, relay, deployment leases, runtime health and S3 write/read/delete checks are real. The fixture also verifies an approved update and rollback from a wrong-runtime candidate, preserving relay identity and S3 configuration. A disk published to Garage
continues publishing, reading remotely and purging there after relayed settings
select external HTTPS S3. No VM boots; this is not runtime/KVM coverage. Containers, network and data are cleaned up on exit.
`CAIRN_BEACON_TEST_DATABASE_URL=… python3 tests/installation_update_containers.py`
also replaces real manager and readiness-runner containers using locally built
fixture images, including interrupted recovery with `restart: unless-stopped`.
Only the registry is adapted; Compose, persistent mounts, runtime
metadata, deployment leases, recovery from a running candidate with a dead HTTP
endpoint, relay reconnection and conversation reads are real.
It checks successful replacement and an exited-candidate rollback, including
identity, synthetic credentials, workspaces and runner state. The readiness runner
boots no VM; active VM checkpointing remains covered by the existing node tests.
`python3 scripts/tests/installation_installer_test.py` checks failure cleanup and
idempotent configuration separately.

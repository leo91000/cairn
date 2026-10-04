# One-command installation

Sign in to the official service and choose **Add an installation**. Copy the
complete `curl … | sudo bash` command onto a trusted Linux x86-64 machine. Its
single-use code lasts ten minutes and is passed as a shell argument, never in a
download URL. No local password, domain, certificate, incoming port or S3 account
is needed. The installation connects out using the existing claim and relay.

Install Docker Engine with its Compose plugin, Python 3 and curl first. Enable
hardware virtualization, `/dev/kvm`, `/dev/net/tun` and `/dev/fuse` (install
`fuse3` and load the kernel modules). The script checks each requirement before
downloading or starting services, including a working Docker daemon and 16 GiB
of free disk for runtime images and recovery staging. Conversation disks need
additional space. It does not install Docker or change virtualization settings.

The root-owned directory `/var/lib/leo-installation` holds the installation.
Compose starts the manager, its local runner and Garage with restart policies and
rotated logs. No service publishes a host port; no container receives the Docker
socket. Private identity and storage credentials must stay on this host. Do not
publish the directory or `docker compose config` output.

Reruns retain the chosen immutable Leo image, identity, Garage keys, storage
settings and persistent data. Concurrent installers are refused. The one-use
claim environment is cleared after success or failure; after startup the manager
is recreated without it. A refused or expired code leaves Leo running unclaimed.
Obtain a new command and rerun it, or run `sudo leo claim` and confirm its code in
the official app. No installation is accessible before claiming. Refresh
installations in the app after the command finishes.

## Integrated storage and external S3

Garage 2.3.0 is pinned by digest. Its
[single-node mode](https://garagehq.deuxfleurs.fr/documentation/quick-start/)
creates the bucket and credentials automatically, with one small storage process
and no provisioning client. Data and metadata persist in the installation
directory. Only this integrated configuration accepts `http://garage:3900` on the
private Compose network; external storage uses HTTPS. S3 remains mandatory for
every disk under ADR-0009.

In **Settings → Conversation storage**, the owner supplies an external HTTPS
endpoint, bucket, region and credentials. Use a dedicated private bucket without
active lifecycle rules or Object Lock, allowing object read/write/delete.
Validation and a write/read/delete probe run before saving. Credentials stay in
the private manager configuration and are never returned to the browser.

The external target becomes the default for new disks. Existing disks keep using
their original storage and its retained credentials. This does not migrate disk
data; keep Garage and its directory while disks still use it. Local S3 does not
protect against losing the machine. Back up the entire installation, including
Garage, with the services stopped. Never discard volumes containing needed data.

Nonempty `STORAGE_S3_*` or legacy `ARCHIVE_S3_*` overrides remain authoritative;
remove them before editing the default in the app. Automatic image updates
belong to ticket #54.

## Operation and validation

```sh
sudo docker compose --project-directory /var/lib/leo-installation \
  -f /var/lib/leo-installation/compose.json ps
sudo docker compose --project-directory /var/lib/leo-installation \
  -f /var/lib/leo-installation/compose.json logs --tail 50
```

For a disposable test, build the current binaries and web output, then run
`bash tests/installation-installer-container.sh`. It uses the real official
service, manager, Postgres and Garage. Only the external Docker process that
would boot the runner is replaced, so claim, relay and S3 checks need no KVM.
Inert devices and command adapters exercise prerequisite messages without
changing the controller. Containers, network and data are cleaned up on exit.
`python3 tests/installation_installer_test.py` checks failure cleanup and
idempotent configuration separately.

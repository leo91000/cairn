# Production releases on cairn.build

Use this runbook for releases and recovery of the existing Beacon service and
its approved installation image. It does not require repeating the v0.53 initial
migration, DNS setup, provider registration or installation claim.

- [Production configuration](PRODUCTION-CAIRN-SETUP.md): provisioning and changes
  to the host, Coolify, proxy, DNS/TLS, providers and installation networking.
- [Historical v0.53 migration](archive/CAIRN-V0.53-MIGRATION.md): first-launch
  qualification, old public manager retirement and naming cutover.

A documentation or implementation task alone does not authorize a release or
production change. Follow the scope Léo authorized for the current release and
the configured GitHub production approval. Infrastructure changes have their
own scope; a routine application release uses the existing configuration.

<a id="8-manual-release-checklist-required-before-approving-a-tag-deployment"></a>

## Release checklist

1. **Choose the version and exact source.** Use a stable `vMAJOR.MINOR.PATCH` tag
   and a reviewed commit. A successful `Quality and container` run must cover its
   exact Git tree. One schema-3 `validated-image-<tree>/image.json` records both
   immutable image digests, the tested commit, run ID and CLI versions. Take the
   Beacon and installation images from that same evidence. A reused PR image
   can report its tested merge commit while matching the tag's tree.
2. **Prepare recovery.** Privately record the current Beacon digest, approved
   installation digest and health identity. Follow [backups and rollback](#backups-and-rollback):
   before each Beacon upgrade, take the Postgres backup, test its restore on
   isolated Postgres 17, encrypt it and retain a copy off-host. Check installation
   schema rollback compatibility; an incompatible change requires a matching
   stopped backup of the whole installation, including Garage and its identity.
   Check the proxy still uses its persisted exact trusted address; configuration
   details are in the [reference](PRODUCTION-CAIRN-SETUP.md#persist-a-fixed-traefik-peer-across-recreation).
3. **Create the tag and release notes.** A pushed version tag starts the container
   and Android workflows. They reuse compatible validation evidence or run fresh
   checks; a tag can wait for validation already running on main. For Android,
   compatibility includes the version and build configuration. Prepare notes in
   a draft release if needed. The Android workflow signs the validated APK,
   attaches it and its update manifest, then publishes the release atomically.
   Leave the draft unpublished until that workflow completes.
4. **Approve deployment after preparation succeeds.** The tag's production job
   requires the existing environment approval. The workflow promotes the tested
   image pair and deploys Beacon through Coolify. A failed backup, restore check
   or required validation keeps deployment unapproved. The initial #105/#113
   migration checklist is historical; qualifying a changed feature or network
   path is part of that change's own validation.
5. **Verify the served release.** Check `/health` for the expected tested commit
   and runtime identity, and `/install/release` for the paired immutable
   installation digest. The deployment workflow verifies both, with up to ten
   minutes for readiness. Check authenticated access, installation reconnection
   and conversation access. Stop further releases and use the recovery procedure
   if the rollout fails.
6. **Verify Android publication when applicable.** Confirm the release contains
   `cairn-android.apk` and `android-update.json`, the APK version/hash match the
   manifest, and its signature preserves the stable update channel. See
   [Android distribution](../apps/android/README.md).

Main, PR and manual validation workflows do not deploy production. A version tag
is the release/deployment path; production jobs are serialized. The hosted web
application and account service belong to Beacon. Installations keep the agents'
work and update separately through their existing host supervisor.

## Installation updates and temporary offline status

The installation timer polls `/install/release` every five minutes. It downloads
the approved image, acquires a deployment lease, then replaces manager and runner
while preserving their data and Garage. An installation can appear **Offline**
during replacement. The manager has a 300-second stop grace period so it can
save checkpoints; the runner has a separate 60-second stop grace period, followed
by startup/readiness checks. These delays are separate from the polling interval.

For an outage during a release, first inspect the update's progress and service
health on the installation host:

```sh
systemctl show cairn-installation-update.service \
  -p ActiveState -p SubState -p ExecMainStatus -p Result
docker ps -a --filter label=com.docker.compose.project=cairn-installation \
  --format '{{.Names}} {{.Status}}'
```

A healthy Beacon alone does not establish that an installation is online. Verify
manager and runner health, cleared maintenance, the account's installation status
and a conversation read. Let an in-flight update or consistent backup finish;
do not race it with another supervisor, container replacement or reinstall.
If progress stalls or the update fails, inspect its stage and pending image
before choosing recovery. Never expose the saved identity, maintenance token or
container environment in diagnostics. See [restart recovery](RESTART-RECOVERY.md)
and [installation recovery](INSTALLATION.md).

<a id="7-backups-and-rollback"></a>

## Backups and rollback

Before **each** beacon upgrade, record the current beacon **and approved
installation** digests and health commit. Back up Postgres while beacon is
stopped (short downtime). Use the actual Coolify service UUID to list containers,
then select the current beacon and Postgres container IDs by role and image:

```sh
service_uuid='the-beacon-coolify-service-uuid'
docker ps -a --filter "label=com.docker.compose.project=$service_uuid" \
  --format '{{.ID}}  {{.Names}}  {{.Image}}  {{.Label "com.docker.compose.service"}}'
```

Coolify can suffix service/container names; do not assume `cairn-beacon-postgres-1`
or the checkout's Compose project. If the installed Coolify version uses a
different project label, find the IDs in its service Containers screen and
confirm the project, role and image with the same listing. Select only this
service's current containers, never the installation or Coolify's own database.
Do not print `docker inspect` environment values or `docker compose config`.

Run from a trusted checkout containing the helper; no Compose interpolation or
operator `.env` file is needed. The database commands use the container's local
operator connection and never pass a password through shell arguments:

```sh
umask 077
beacon_container='selected-beacon-container-id'
postgres_container='selected-postgres-container-id'
backup_path='/private/backup/beacon-before-release.dump'
# Each step runs only if its predecessor succeeded; on error investigate.
docker stop --time 60 "$beacon_container" && \
  bash deploy/beacon/postgres-backup.sh backup "$postgres_container" "$backup_path" && \
  docker start "$beacon_container"
```

The helper writes a private custom-format dump to a temporary adjacent file,
then publishes it atomically only after `pg_dump` succeeds. An existing backup
is never overwritten; the directory must support hard links. Failure removes
the partial file.
Use a new filename for each release and retain the release/digest record privately.
If backup fails, resume the previous Beacon process and investigate; keep the
deployment unapproved. A failed preparation must not leave the existing service
stopped while waiting for approval.

Check the backup can be restored on a disposable Postgres 17 instance; encrypt it
and copy it off the server. It contains account/session material and must never
be attached to GitHub or published. The installation separately needs stopped
backups of **its whole directory including Garage and private identity**; a
Postgres backup does not include conversations, disk objects or agent accounts.

If beacon deployment fails, freeze further release approvals/tags, stop
**beacon**, and restore `CAIRN_BEACON_IMAGE` to the previous verified digest.
Restore `CAIRN_INSTALLATION_IMAGE` to its previous paired digest too. An already
updated installation will see that approved rollback via its existing timer;
only approve it if its database remains compatible, otherwise restore its
matching stopped backup as described in [installation recovery](INSTALLATION.md).

For beacon, startup migrations can be incompatible with the older binary.
Restore the **matching pre-release Postgres backup** into an empty database while
beacon is stopped; preserve the failed database privately for investigation.
Use a fresh isolated volume/database, or explicitly recreate `cairn_beacon` only
after retaining a backup of the failed database. Rediscover the current container
IDs after a failed deployment; never restart a stale container from before it.
Set the **previous paired digests** in Coolify while stopped. For the explicitly
chosen database container, with every beacon process stopped:

```sh
# Destructive recovery: only after preserving the failed database privately.
docker exec "$postgres_container" dropdb -U cairn --force cairn_beacon && \
  docker exec "$postgres_container" createdb -U cairn -O cairn cairn_beacon && \
  bash deploy/beacon/postgres-backup.sh restore "$postgres_container" "$backup_path"
```

The helper invokes `pg_restore --exit-on-error --no-owner --no-privileges` into
that empty database. The schema/data are owned by the current `cairn` operator
role; database roles and grants must be provisioned separately if that changes.
A failed restore keeps beacon stopped; recreate the empty recovery database
before retrying rather than continuing on a partial schema. This loses account,
claim and sharing changes made after the backup; reconcile installation identities
using [claim recovery](INSTALLATION-RELAY.md) if necessary. Never start the older
binary against a newer schema or reuse a backup from an unrelated release.

`node tests/beacon-postgres-backup.mjs` exercises this exact helper against
pinned, disposable Postgres, including a renamed container, file permissions,
known-data round trip, refusal to overwrite a previous backup, rejection of a
nonempty target/invalid dump, and cleanup
of a failed backup. It uses synthetic data and never connects to production.

Restart the single beacon process, verify `/health`'s previous commit,
`/install/release`'s previous digest, email sign-in, claim/relay reconnection and
conversation access. TLS/DNS do not need changing for this rollback. Keep the
failed release, backups and previous digests until recovery is confirmed.


## Configuration reference

<a id="4-resend-and-optional-sign-inpush"></a>
<a id="5-create-the-beacon-coolify-service"></a>
<a id="6-install-and-claim-on-the-same-server"></a>
<a id="stun-and-direct-installation-connectivity-101"></a>
<a id="naming-cutover-manual-actions-reserved-for-léo"></a>

The setup details have moved to [production configuration](PRODUCTION-CAIRN-SETUP.md):
[providers](PRODUCTION-CAIRN-SETUP.md#email-oauth-and-push),
[Coolify and proxy trust](PRODUCTION-CAIRN-SETUP.md#coolify-service-and-trusted-proxy),
[installation claim](PRODUCTION-CAIRN-SETUP.md#install-and-claim-on-the-same-server),
and [STUN](PRODUCTION-CAIRN-SETUP.md#stun-and-direct-installation-connectivity).
The [v0.53 archive](archive/CAIRN-V0.53-MIGRATION.md) records the naming cutover.

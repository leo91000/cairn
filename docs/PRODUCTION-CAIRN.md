# Production releases on cairn.build

Use this runbook for releases and recovery of the existing Beacon service and
its approved installation image. It does not require repeating the v0.53 initial
migration, DNS setup, provider registration or installation claim.

- [Production configuration](PRODUCTION-CAIRN-SETUP.md): provisioning and changes
  to the host, Coolify, proxy, DNS/TLS, providers and installation networking.
- [Historical v0.53 migration](archive/CAIRN-V0.53-MIGRATION.md): first-launch
  qualification, old public manager retirement and naming cutover.

A documentation or implementation task alone does not authorize a release or
production change. Creating an authorized stable release tag starts the automatic
production pipeline; routine releases do not require another approval click.
Infrastructure changes have their own scope and are not part of a routine release.

<a id="8-manual-release-checklist-required-before-approving-a-tag-deployment"></a>

## Release checklist

1. **Choose the version and exact source.** Use a stable `vMAJOR.MINOR.PATCH` tag
   and a reviewed commit. A successful `Quality and container` run must cover its
   exact Git tree. One schema-3 `validated-image-<tree>/image.json` records both
   immutable image digests, the tested commit, run ID, CLI versions and the served
   engine releases its rollback checks used
   ([tool versions](CLI-UPDATES.md)). Take the
   Beacon and installation images from that same evidence. A reused PR image
   can report its tested merge commit while matching the tag's tree.
2. **Check data compatibility.** The workflow compares the served and candidate
   installation data epochs. A changed epoch can deploy automatically when its
   source epoch is explicitly listed in the reviewed migration contract and its
   declared regressions passed exact-tree image validation. See
   [automatic migrations](AUTOMATIC-MIGRATIONS.md). Unknown transitions and
   downgrades stop before deployment. Releases do not create or retain backups.
3. **Create the tag and release notes.** A pushed version tag starts the container
   and Android workflows. They reuse compatible validation evidence or run fresh
   checks; a tag can wait for validation already running on main. For Android,
   compatibility includes the version and build configuration. Prepare notes in
   a draft release if needed. The Android workflow signs the validated APK,
   attaches it and its update manifest, then publishes the release atomically.
   Leave the draft unpublished until that workflow completes.
4. **Follow automatic deployment.** The production job first checks that
   production still serves the engine releases the rollback checks used. After
   that and the compatibility check, the production
   job deploys the validated image pair through Coolify. It does not stop Beacon
   for a database dump or upload recovery artifacts.
5. **Verify the served release.** Check `/health` for the expected tested commit
   and runtime identity, and `/install/release` for the paired immutable
   installation digest. The deployment workflow verifies both, with up to ten
   minutes for readiness. The workflow also waits for the installation's manager
   and runner to adopt the approved image and for execution readiness to return.
   Check authenticated access and conversation behavior when qualifying a changed
   feature. Stop further releases and use the recovery procedure if rollout fails.
6. **Verify Android publication when applicable.** Confirm the release contains
   `cairn-android.apk` and `android-update.json`, the APK version/hash match the
   manifest, and its signature preserves the stable update channel. See
   [Android distribution](../apps/android/README.md).

Main, PR and manual validation workflows do not deploy production. A version tag
is the release/deployment path; production jobs are serialized. The hosted web
application and account service belong to Beacon. Installations keep the agents'
work and update separately through their existing host supervisor.

## Installation data compatibility

`deploy/installations/data-compatibility.json` declares the installation's reviewed
data compatibility epoch. During implementation and review, account for every
changed persistent schema, record, disk manifest, encrypted envelope, checkpoint,
identity and plugin format, wherever its owner lives. Keep the epoch unchanged
only when the previous release can read data written by the new release and the
new release can read existing data. A compatibility break **must increment the
epoch in the same PR**, with a reviewed forward migration and failure recovery plan.
There is no automatic backup or data restoration.

The automatic job compares the served and candidate source contracts. Equal
epochs with unchanged database compatibility allow routine deployment. Schema 2
also permits declared forward transitions whose plan and regression evidence were
reviewed and validated with the exact candidate tree. Missing/invalid metadata,
unlisted source epochs and downgrades stop before deployment. The database version
fence prevents an old binary or the installed updater from rolling back after
migration starts. See [the transition contract](AUTOMATIC-MIGRATIONS.md).
Never lower the epoch, database version or legacy tree to force a deployment.
Initial adoption accepts epoch 1 only when the served
Git tree matches the recorded reviewed legacy tree. The contract covers all
persisted data; it relies on source review, rather than inferring compatibility
from file names. Include the compatibility decision in reviews of format changes.
Do not advance the recorded legacy tree to bypass an unknown installation state.

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
and a conversation read. Let an in-flight update finish;
do not race it with another supervisor, container replacement or reinstall.
If progress stalls or the update fails, inspect its stage and pending image
before choosing recovery. Never expose the saved identity, maintenance token or
container environment in diagnostics. See [restart recovery](RESTART-RECOVERY.md)
and [installation recovery](INSTALLATION.md).

<a id="7-backups-and-rollback"></a>
<a id="backups-and-rollback"></a>

## Failed releases

Releases do not create backups. Existing release archives and their off-host
copies have been removed. No data restore point is provided by the release system.

If a rollout fails, stop further releases and inspect the updater state before
intervening. Record the current and previous immutable image pairs. An image
rollback is valid only if both binaries can read the current persisted data.
An epoch change requires a forward fix; do not start an older binary against
migrated data. Preserve the active installation directory, Garage, private
identity and Beacon database while investigating.

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

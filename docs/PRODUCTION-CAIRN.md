# Production releases on cairn.build

Use this runbook for releases and recovery of the existing Beacon service and
its approved installation image. Preserve the existing installation and Beacon
data during routine upgrades; check the supported schema path below.

- [Production configuration](PRODUCTION-CAIRN-SETUP.md): provisioning and changes
  to the host, Coolify, proxy, DNS/TLS, providers and installation networking.

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
   Beacon and installation images from that same evidence. Nightly qualification produces reusable evidence; historical complete main/PR
   evidence remains compatible. Short PR checks provide no image provenance.
   When the candidate changes installation migrations, complete the
   [private upgrade rehearsal](#private-installation-upgrade-rehearsal) before
   pushing the tag.
2. **Create the tag and release notes.** A pushed version tag starts the container
   and Android workflows. They reuse compatible validation evidence or run fresh
   checks; a tag can wait for nightly qualification already running on main. For Android,
   compatibility includes the version and build configuration. Prepare notes in
   a draft release if needed. The Android workflow signs the validated APK,
   attaches it and its update manifest, then publishes the release atomically.
   Leave the draft unpublished until that workflow completes.
3. **Follow automatic deployment.** The production job first checks that
   production still serves the engine releases the rollback checks used. After
   that check, the production
   job deploys the validated image pair through Coolify. It does not stop Beacon
   for a database dump or upload recovery artifacts.
4. **Verify the served release.** Check `/health` for the expected tested commit
   and runtime identity, and `/install/release` for the paired immutable
   installation digest. The deployment workflow verifies both, with up to ten
   minutes for readiness. The workflow also waits for the installation's manager
   and runner to adopt the approved image and for execution readiness to return.
   Check authenticated access and conversation behavior when qualifying a changed
   feature. Stop further releases and use the recovery procedure if rollout fails.
5. **Verify Android publication when applicable.** Confirm the release contains
   `cairn-android.apk` and `android-update.json`, the APK version/hash match the
   manifest, and its signature preserves the stable update channel. See
   [Android distribution](../apps/android/README.md).

Main, PR and manual validation workflows do not deploy production. A version tag
is the release/deployment path; production jobs are serialized. The hosted web
application and account service belong to Beacon. Installations keep the agents'
work and update separately through their existing host supervisor.

## Data-preserving upgrades

Installation database version 11 upgrades automatically to version 12 at manager
startup through the [typed inventory migration](migrations/0012-typed-node-inventory.md).
Direct-storage releases then continue to version 13 through a separate
[coordination metadata migration](migrations/0013-direct-node-storage.md).
Version 13 then upgrades to version 14 through the
[typed account pool migration](migrations/0014-typed-account-pool.md), preserving
account identities, quota state, order, encrypted credentials and references.
Version 14 then upgrades to 15 through the
[typed conversation migration](migrations/0015-typed-conversations.md).
Version 15 then upgrades to 16 through the
[typed questions and waits migration](migrations/0016-typed-questions-and-waits.md).
Version 16 then upgrades to 17 through the
[attachment reservation migration](migrations/0017-attachment-reservations.md).
Version 17 then upgrades to 18 through the
[per-account read state migration](migrations/0018-personal-read-state.md).
Version 18 upgrades to 19 through the [run projections migration](migrations/0019-run-projections.md).
Version 19 upgrades to 20 by adding plugin hook tables.
Version 20 upgrades to 21 through the [writer-owned progress migration](migrations/0021-run-progress.md).
Version 21 upgrades to 22 through the [timeline projection migration](migrations/0022-timeline-projection.md), replaying retained events.

Version 22 upgrades to 23 through the [typed placement migration](migrations/0023-typed-placement.md).
Stop the old manager and retain a consistent database/WAL backup first. Keep the
installation identity, configuration, data roots, node state and object storage.
The migration preserves node identities, credentials, configuration and references;
conversion failures leave the original version available for repair and retry. Earlier
layouts need their explicit migrations; do not replace a migration with a schema
version edit or a data reset. Deploy matching manager, runner, node and guest
versions before resuming work.

Beacon has its own Postgres migration ledger. Validate its compatibility before
promotion; this installation migration neither resets Beacon nor authorizes
discarding accounts. A historical incompatible ledger requires a separately
prepared data-preserving migration before rollout. See
[Beacon database compatibility](BEACON.md#next-release-recreate-the-beacon-database).

### Private installation upgrade rehearsal

Before tagging a changed installation migration, exercise the candidate against
a recent, consistent copy of the installation database. Synthetic migration tests
remain necessary; historical production shapes also need this private check.

1. Retain an untouched, private source backup. Use SQLite's backup API or a
   coordinated stopped-manager backup that preserves the database and its WAL.
   Copying only a live `manager.db` file is not a consistent backup. Record its
   schema version and checksum; keep directories owner-only and files private.
2. Make a separate writable rehearsal copy containing `manager.db`. Never pass
   the live installation directory or the untouched source backup to the helper.
   Keep all database copies on the installation host; do not upload them to CI,
   artifacts or a PR.
3. Use the `browser_fixture` binary from the candidate's qualified
   `browser-backend` artifact, or build that example from the exact candidate
   tree. Verify the artifact's run, source tree and digest, record the executable
   checksum, and use a binary compatible with the host's architecture and libc.
   Run only the Store migration entry point, isolated from networking:

   ```sh
   printf '%s\n' '{"operation":"upgradeStoreCopy","args":["/private/rehearsal-copy"]}' \
     | sudo unshare --net -- /private/candidate/browser_fixture
   ```

   This operation writes the supplied copy. It requires an existing `manager.db`
   and opens the Store without configuration loading, Service, Vault or credential
   refresh. It does not start a manager or verify application readiness.
4. Require the expected `versionBefore` and `versionAfter`, `quickCheck: "ok"`
   and `foreignKeyCheck: "ok"`. Reopen the migrated copy to confirm its resulting
   schema version and verify the untouched source checksum is unchanged. Retain
   only aggregate verdicts and source/artifact identities in public evidence;
   keep database contents, credentials and private paths out of published logs.
   A failure blocks the tag: preserve the private copies and repair the migration.

This rehearsal does not replace the pre-upgrade backup or the post-deployment
readiness checks. It creates no automatic release archive or restore point.

## Réinstallation complète

Cette procédure destructive est réservée à une demande explicite d’effacement et
de réinstallation. Elle ne fait pas partie d’une release ordinaire ni de la migration
11 → 12 ci-dessus. Ne pas l’utiliser pour réparer un échec de migration en conservant
les données.

1. Arrêter les timers de mise à jour, le manager, le runner et toutes les nodes.
   Supprimer les conteneurs de l’installation avec `docker compose down`, avant
   d’effacer leur configuration Compose.
2. Vider entièrement le bucket S3 externe de cette installation, versions et
   marqueurs de suppression compris. Supprimer toute la racine de l’installation
   (`/var/lib/cairn-installation` par défaut), dont `installation.json`, la
   configuration Compose et de mise à jour, `data`, `home`, `workspaces`,
   `runner-state` et `garage`. Cela efface aussi le bucket Garage intégré, les
   anciennes images sélectionnées, l’identité et les secrets. Effacer également
   la configuration et tout l’état local de chaque node (disques, journaux et caches).
3. Réinstaller dans des racines vides avec les images et binaires de la même
   release, selon [l’installation](INSTALLATION.md), puis revendiquer l’installation.
4. Reconnecter les comptes et plugins, réimporter les projets et vérifier une nouvelle
   conversation. Les anciennes conversations et identités ne sont pas récupérées.

L’effacement de Beacon est une opération distincte ; cette procédure ne l’autorise pas.

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
intervening. Record the current and previous immutable image pairs. Use a forward fix or an explicitly validated compatible recovery image; automatic update retries retain
the candidate image instead of starting an older binary against current data. Preserve the active installation directory, Garage, private
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

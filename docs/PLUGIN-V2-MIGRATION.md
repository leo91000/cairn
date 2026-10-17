# Plugin API v2 installation migration

Plugin API v2 changes the installation data compatibility epoch from 1 to 2.
Merging the implementation does not run the migration. The first startup with the
new binary does, and the automatic production compatibility check must stop an
upgrade from epoch 1 until its rollout and recovery preparation are complete.
See the [production release contract](PRODUCTION-CAIRN.md#installation-data-compatibility).

## What changes on disk

`crates/installation/src/plugins/legacy.rs` migrates existing 1Password service
accounts into plugin instances. IDs, names, enabled state, agent grants and tokens
are preserved. One database transaction replaces the `onepassword` records with
`plugin-connections` records and moves the encrypted credential from its old vault
key to the plugin instance's secret key. The old records and credential entries
are removed. Later startups find no legacy records and do no further migration.

The previous binary cannot read these accounts after migration. Installed packages
must also declare `apiVersion: 2`; their old approval digests do not authorize the
new manifest-and-files digest. Previous conversation VM binaries must restart to
use the current plugin CLI protocol. This is a forward migration, without a
reverse migration implemented in Cairn.

## Prepare and roll out

1. Record the existing binary/image version and the exact tested candidate source
   and immutable images. Complete the normal release checks before deployment.
2. Stop the installation and all writers before taking a matching backup of its
   complete installation directory, Garage data, configuration and private identity.
   Protect the backup as secret material and verify that it can be restored in
   isolation. A Beacon database dump alone cannot restore installation state.
3. Rehearse the candidate against an isolated restore with external transports
   disabled. Verify account IDs, names, enabled state and agent grants against the
   pre-upgrade inventory. Use account/vault metadata to verify credentials; do not
   print tokens or secret field values. Check both an allowed and a denied agent.
4. Approve the epoch transition with the stopped backup and its recovery point
   recorded. The migration deployment needs an explicitly reviewed procedure;
   changing the epoch back or changing `legacyTree` is not a way to bypass the
   automatic compatibility check.
5. Start the candidate and verify Plugins → 1Password, account metadata access,
   agent authorization, installed-package reapproval and restarted conversation VMs.
   Confirm Discord configuration and connectivity, including a real bot before
   relying on that integration in production. Keep the pre-migration backup.

## Roll back

Stop the new installation and its writers. Restore the complete matching
pre-migration installation and Garage backup, configuration and private identity,
then start the recorded previous binary/image. Do not point the old binary at a
migrated directory or restore only selected database rows or vault keys.

Restoring the backup also discards writes made after its recovery point; account
for that loss when approving the rollout or recovery. Retain the migrated state
separately for investigation, with the same access controls as the backup.

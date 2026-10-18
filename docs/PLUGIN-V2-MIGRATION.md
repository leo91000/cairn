# Plugin API v2 installation migration

Plugin API v2 changes the installation data compatibility epoch from 1 to 2.
Merging the implementation does not run the migration. The first startup with the
new binary does, and the automatic production compatibility check must stop an
upgrade from epoch 1 until its reviewed forward rollout is carried out.
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
2. Inventory account IDs, names, enabled state and agent grants before rollout.
   Use metadata to verify credentials without printing tokens or secret fields.
   Check the migration's transactional and authorization regression tests.
3. Review the forward-only epoch transition separately from routine releases.
   The release system creates no backup and provides no data restore point.
   Do not change the epoch or legacy tree to bypass its compatibility check.
4. Deploy the tested candidate image pair through the reviewed operator procedure.
   Do not roll back to an epoch-1 binary after migration. If startup fails, inspect
   the migration and apply a forward fix while preserving the active data.
5. Verify Plugins → 1Password, account metadata, allowed and denied agents,
   installed-package reapproval and restarted conversation VMs. Confirm Discord
   connectivity with a real bot before relying on that integration in production.

# Reviewed automatic installation migrations

A stable release can cross a declared data epoch without an operator step. The
reviewed declaration is part of `deploy/installations/data-compatibility.json`,
and the normal exact-tree image validation covers it. Merging does not deploy.

## Contract

Schema 2 records the installation `epoch`, its SQLite `databaseVersion`, and an
optional `automaticMigration` with:

- `fromEpochs`: the exact earlier epochs this candidate can migrate;
- `previousDatabaseVersion`: the previous binaries' database version ceiling;
- `plan`: a committed document describing conversion and forward recovery;
- `tests`: the exact Rust regression names that must pass in the backend suite.

The backend job preserves the test exit status and checks its real output with
`production-release.mjs qualify`. Missing, ignored, renamed or failing tests do
not qualify a candidate. Image publication and reuse retain the existing exact-tree
validation requirement; a tag cannot add a declaration to an otherwise tested tree.

The release gate accepts an equal epoch with the same contract schema and database
version, or an explicitly listed forward transition. A lower epoch, unknown source
epoch, missing plan, malformed declaration or inconsistent database version stops
before deployment. Future transitions need their own reviewed declaration and tests.
The legacy tree remains unchanged; epochs 1 and 2 are not approved by this plan.

This replaces the manual-only rollout requirement in the historical session-restart,
native-execution and GitHub transition plans, including
[ADR 0050](adr/0050-trusted-native-execution.md) and
[ADR 0052](adr/0052-native-ublk-workspaces-and-idle-eviction.md). Their conversion,
trust and recovery rules remain required.

## Current transition: epochs 3–8 to 9

The served production source inspected during preparation was
`067153e797f426e9b1d224cee0b2e48fab793ec3` (epoch 3, SQLite version 7).
The same SQLite ceiling is used by the earlier epoch-4 through epoch-8 managers.
Epoch 9 makes the forward-only boundary enforceable on disk by advancing SQLite
to version 8. It preserves the startup conversions and reader behavior already
reviewed for the intervening epochs:

| Boundary | Behavior retained by the candidate | Regression |
| --- | --- | --- |
| 3 → 4 | Strict plugin setting constraints; incompatible packages remain on disk with version errors, without changing their approvals, settings or secrets | `settings::tests::length_limits_are_positive_integers_with_type_specific_caps` |
| 4 → 5 | Both legacy checkpoint restart markers and durable chat restart intent remain readable; consent and transactional consumption are preserved | `incompatible_restored_session_requires_consent_and_never_restarts_on_its_own` |
| 5 → 6 | Native execution remains explicitly trusted; existing VM placement is not converted into native work | `native_placement_works_without_kvm_or_fuse_and_never_falls_back_to_a_vm` |
| 6 → 7 | Native file checkpoints remain readable; conversion to block storage preserves work, model caches and sessions | `native::workspace::tests::local_block_migration_preserves_model_cache_and_sessions_without_managed_auth` |
| 7 → 8 | GitHub login, dedicated tokens, grants, projects, skills and waits migrate transactionally into plugin instances, idempotently | `data_saved_before_the_package_moves_into_instances` |
| 8 → 9 | SQLite version advances before startup migrations; a newer database is rejected without being downgraded | `upgrades_version_five_without_changing_records_and_fences_older_binaries` |

The existing native Linux storage/image qualification also remains required for
published images. See the detailed [plugin constraints](PLUGINS.md#setting-constraints-and-compatibility-f6),
[session restart](SESSION-RESTART.md#compatibility), [native storage](NATIVE-NODES.md),
and [GitHub migration](GITHUB-PLUGIN-MIGRATION.md) contracts. Invalid installed
packages require a corrected, reviewed package version; the release never silently
clamps values, repairs a package or grants it new approval.

## Startup, verification and recovery

The updater pulls the immutable image, obtains its deployment lease and stops the
old manager and runner before starting the candidate. `Store::open` advances the
SQLite version transactionally before `Service::new` performs its migrations or
opens HTTP, relay and worker admission. A failure after this fence retains the
candidate even if a later migration transaction has not committed yet.

The installed updater already compares `PRAGMA user_version` with the version it
recorded before replacement. An increased version prevents its ordinary fallback
to the older image and survives supervisor interruption. Earlier managers also
reject version 8 before creating schemas or modifying application records. This
works with the existing updater; it needs no privileged helper replacement.

Readiness verifies both manager and runner on the exact immutable installation
image, plus execution readiness and cleared maintenance. The new manager exposes
health only after startup migrations finish. The release therefore waits for the
converted candidate rather than treating Beacon health alone as success.

On a migration failure, preserve the installation directory, Garage, identity and
pending candidate. Retry the same image or deploy a reviewed forward fix. Do not
lower `user_version`, the data epoch or the legacy tree, or start an older manager
against this database. Compatible same-epoch failures retain the normal image
fallback. The release creates no backup or automatic data restore point.

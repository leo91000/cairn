# Reviewed automatic installation migrations

A stable release can cross a declared data epoch without an operator step. The
reviewed declaration is part of `deploy/installations/data-compatibility.json`,
and the normal exact-tree image validation covers it. Merging does not deploy.

## Contract

Schema 2 records the installation `epoch`, its SQLite `databaseVersion`, and an
optional `automaticMigration` with:

- `fromEpochs`: the exact earlier epochs this candidate can migrate;
- `previousDatabaseVersion`: the maximum SQLite version of the explicitly listed
  source epochs; an earlier listed epoch may have a lower version;
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

## Slack draft boundary

Main's guided plugin setup transition uses epoch 12 / SQLite 11, after mission
waits at epoch 10 / SQLite 9 and native concurrency at epoch 11 / SQLite 10.
PR #226 reserves epoch **13 / SQLite 12** for its incompatible plugin/requester
contract. Its earlier, unreleased 12/11 reservation was raised when the guided
setup transition was integrated; it cannot share Main's boundary.
The retained transitions below do not qualify Slack. This draft declares no
automatic migration to epoch 13; deployment from an earlier epoch remains blocked
until [Slack's compatibility inventory](SLACK-PLUGIN-COMPATIBILITY.md) and a
separate forward transition are fully qualified and independently reviewed.
Do not lower the version fence to bypass this gate.

## Retained transition: epochs 3–8 to 9

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


## Retained transition: epochs 3–9 to 10

Epoch 10 adds durable mission waits. SQLite advances from version 8 to 9 in the
same transaction that rebuilds `runs_active_task` to include `waiting`. Existing
run documents, conversation waits, checkpoints and all prior startup conversions
are preserved. The index continues to prevent a second active execution of the
same mission. A waiting run has no `finishedAt`, retains its checkpoint/session,
and releases its process, account, MCP grants and execution slot before resuming.

Mission wakeups and continuation identities are stored in the existing run JSON.
Older managers neither understand `waiting` nor fence credentials against a new
mission turn, so they must not read or write these records. The version-9 fence
rejects every previous manager, including epoch 9. The updater retains the
candidate after crossing this boundary. `upgrades_version_eight_and_keeps_waiting_missions_in_the_active_slot`
verifies the index conversion, active query, duplicate admission rejection and
version fence. The earlier migration tests remain required by the release gate.

As with the earlier transition, a failure requires retrying this candidate or a
reviewed forward fix. Preserve the installation data; never lower SQLite's
`user_version` or roll an older binary onto the migrated database. Merging this
change does not deploy an installation. Local storage measurements and the
reproduction command are in [mission waits](MISSION-WAITS.md).

<a id="current-transition-epochs-310-to-11"></a>

## Retained transition: epochs 3–10 to 11

Epoch 11 permits configurable concurrent native slots and an owner-selected
all-node agent grant. SQLite advances from version 9 to 10 before admission,
without rewriting existing records or changing workspace/checkpoint formats.
Existing one-slot budgets, explicit node grants, native trust, provenance and
delegation bounds remain intact. The previous controller refuses a persisted
native budget with more than one slot, so matching JSON shapes do not make a
rollback compatible. The version-10 fence prevents earlier managers from
reopening the installation after this transition.

`upgrades_version_nine_preserving_native_nodes_and_fences_older_binaries` verifies
record preservation and the forward fence. Native placement and controller tests
verify the applied-budget handshake, six admissions, overflow, release and trust
checks; earlier migration regressions remain mandatory in exact-tree validation.

Native supervisors retain their independent epoch gate. For an existing native
host, prepare and verify the exact released candidate and its pinned tool bundle,
drain admission and confirm no active attempts or owned workload cgroups remain,
then stop the supervisor. Preserve its identity, state and previous tool bundles;
select the reviewed candidate while stopped and restart the supervisor. Verify
its runtime, health and applied budget before clearing maintenance or raising
slots. Do not use the supervisor's compatible-version rollback activation path
across this boundary. If startup fails after migration, keep the candidate and
state, diagnose and retry it or apply a reviewed forward fix. Never lower
`user_version` or restart the previous controller over the migrated state.

Manager/runner rollout uses the existing automatic migration gate. This release
does not create or restore backups, convert conversations, or grant host trust.

<a id="current-transition-epochs-311-to-12"></a>

## Retained transition: epochs 3–11 to 12

Epoch 12 adds optional `[setup]` declarations to installed plugin manifests.
Previous binaries reject this field, so SQLite advances from version 10 to 11
transactionally in `Store::open`, before plugin loading or admission. Existing
package files, approvals, connections, vault entries and workspace formats are
retained without conversion. Packages without `[setup]` derive their assistant
steps; declaring it requires the new reader. Shared settings copy into each new
connection's existing settings and vault format.

`upgrades_version_ten_preserving_plugin_packages_and_fences_older_binaries`
checks preservation of package files and plugin records and the candidate fence
(which is now SQLite 12 on the unqualified Slack branch),
and refusal of a newer database without downgrading it. All earlier migration
regressions remain required by the exact-tree release gate. The version advance
changes only the compatibility marker; it adds no read/write storage format or
workload change requiring new throughput measurements.

The installed updater retains the candidate once this fence is crossed. If
startup or a later plugin operation fails, preserve the installation directory,
package files, vault and identity, then retry this candidate or deploy a reviewed
forward fix. Never lower SQLite `user_version`, the epoch or the legacy tree, or
start an older manager against the migrated installation. Merging does not
release or deploy an installation.

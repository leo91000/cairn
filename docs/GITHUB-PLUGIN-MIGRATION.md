# GitHub plugin installation migration

Moving GitHub into the bundled GitHub package ([ADR 0053](adr/0053-github-as-a-bundled-plugin.md))
changes the installation data compatibility epoch from 7 to 8. Merging the
implementation does not run the migration. The first startup with the new binary
does. The [reviewed automatic transition to epoch 9](AUTOMATIC-MIGRATIONS.md)
includes this epoch-7 → 8 conversion and adds its on-disk rollback fence. A stable
release from a declared source epoch performs it automatically. See the
[production release contract](PRODUCTION-CAIRN.md#installation-data-compatibility).

## What changes on disk

`crates/installation/src/plugins/legacy_github.rs` runs before agents are
normalized, in one database transaction:

- The shared connection: the token `gh` saved in the installation home
  (`<AGENT_HOME>/.config/gh/hosts.yml`, flat or per-user format) becomes the
  `GitHub` instance of the `github` package, its token encrypted in the vault, granted
  to every agent whose `access.github` was not `false` (the old default, Main
  included). Without a saved login, no shared instance is created.
- Dedicated tokens (`agent-github:<agent>`, plaintext in `kv`) become
  `GitHub (<agent name>)` instances granted to their agent and are deleted from
  `kv`. An agent that also used the shared connection never used its token: the
  instance is kept without a grant.
- `access.github` is removed from agents, whose policy is materialized first, and
  from the approved access of plugin delegations, so Discord bots stay approved.
- Skill sources `{ type: "github", repository, path, ref, commit }` become plugin
  sources `{ pluginId: "github", source: "repository", location: { repository, ref },
  folder, version, … }` attached to the shared instance.
- Projects whose remote is on github.com gain `source` pointing at the shared
  instance, so its credentials fetch them.
- Pending `github_workflow` / `github_pr_checks` waits gain `pluginId: "github"`.

Later startups find nothing to move. Once the plugins start, the package's form
fills each migrated instance's login and commit identity from GitHub; a failure only
leaves them empty (runs then use the installation's Git identity, if any).

The `gh` login file stays in the installation home: Cairn no longer reads it. Run
`gh auth logout` there once the migrated instance works, if it is not used otherwise.

The previous binary cannot read the migrated agents, instances and sources. This is
a forward migration without a reverse migration implemented in Cairn.

## Prepare and roll out

1. Record the existing image pair and the exact tested candidate. Complete the
   normal release checks.
2. Inventory, without printing tokens: agents and their `access.github`, the
   presence of `agent-github:*` keys, whether the installation home has a `gh` login,
   GitHub skill sources, and pending GitHub waits. Check the migration regression
   test (`data_saved_before_the_package_moves_into_instances`).
3. Keep the forward transition and its regression in the reviewed automatic
   migration declaration. CI must observe that test passing before the candidate
   image is published. The release system creates no backup; do not lower the epoch,
   database version or legacy tree to bypass the compatibility check.
4. The stable release deploys the qualified candidate and waits for the installation
   manager, runner and execution readiness. Do not roll back to an epoch-7 binary
   after migration; retry the same candidate or apply a forward fix if startup fails.
5. Verify Plugins → GitHub: the migrated accounts, their login and test, the agents
   granted on each agent page, a run pushing a branch, a `wait_for` on a pull request,
   a project import, and a skill source status check. Conversation VMs started before
   the upgrade receive the new credential files when they resume.

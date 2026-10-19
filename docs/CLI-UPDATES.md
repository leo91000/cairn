# Agent tool versions and retired standalone updates

Standalone CLI updates were disabled at the Beacon cutover (#113). Their
workflow, candidate-image Dockerfile, deployment commands and source systemd
units have now been removed. Do not dispatch `cli-updates.yaml` or invoke
`scripts/cli-updates.mjs check|deploy` from external automation.

## Supported update path

Every application CI run, including tag releases, uses
`node scripts/cli-updates.mjs build-versions` to resolve stable Codex and GitHub
CLI versions before deciding whether to reuse an image. This command remains
available at its existing path. It prints JSON and, when `GITHUB_OUTPUT` is set,
appends the exact `codex` and `gh` outputs consumed by `ci.yaml`.

There are no default Codex/GitHub CLI versions in the Dockerfile. CI passes the
resolved `CODEX_VERSION` and `GH_VERSION` build arguments and records them in
validation evidence. An image can be reused for the same source tree only when
both tool versions match discovery. Tools never change after image validation.
Direct Docker builds must pass these build arguments too; see
[deployment setup](DEPLOYMENT.md) and the [agent toolkit](TOOLKIT.md).

Installations follow approved, paired immutable images through
`cairn-installation-update.timer`. This host updater retains the persisted
deployment lease, runtime verification and rollback. Removing the standalone
updater does not remove or replace the lease API or its worker scheduling rules.
See [installation updates](INSTALLATION.md#automatic-approved-updates) and the
[production runbook](PRODUCTION-CAIRN.md#installation-updates-and-temporary-offline-status).

## Retiring an existing standalone dispatch timer

The state of production timers and external callers has not been verified.
Deleting units from the repository does not uninstall copies on a host. A
remaining old timer would dispatch a removed workflow and fail; it must not be
repointed at the release workflow or given new production credentials.

An operator retiring a historical installation can inspect it with:

```sh
sudo systemctl status cairn-cli-update.timer cairn-cli-update.service
sudo systemctl list-timers --all cairn-cli-update.timer
sudo journalctl -u cairn-cli-update.service
```

After checking for local unit overrides and an in-flight invocation, disable the
old timer and remove only its historical units:

```sh
sudo systemctl disable --now cairn-cli-update.timer
sudo systemctl stop cairn-cli-update.service
sudo rm -f /etc/systemd/system/cairn-cli-update.timer /etc/systemd/system/cairn-cli-update.service
sudo systemctl daemon-reload
```

These are operator instructions; this repository cleanup performs no production
operation. Keep `cairn-installation-update.timer` and its service installed. Do
not delete installation state, maintenance credentials or deployment leases as
part of retiring the standalone timer.

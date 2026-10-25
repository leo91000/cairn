# Agent tool versions

Every application CI run, including tag releases, uses
`node scripts/cli-updates.mjs build-versions` to resolve stable Codex and GitHub
CLI versions before deciding whether to reuse an image. It prints JSON and,
when `GITHUB_OUTPUT` is set,
appends the exact `codex` and `gh` outputs consumed by `ci.yaml`.

There are no default Codex/GitHub CLI versions in the Dockerfile. CI passes the
resolved `CODEX_VERSION` and `GH_VERSION` build arguments and records them in
validation evidence. An image can be reused for the same source tree only when
both tool versions match discovery. Tools never change after image validation.
Direct Docker builds must pass these build arguments too; see
[deployment setup](DEPLOYMENT.md) and the [agent toolkit](TOOLKIT.md).

Installations follow approved, paired immutable images through
`cairn-installation-update.timer`. This host updater retains the persisted
deployment lease, runtime verification and rollback.
See [installation updates](INSTALLATION.md#automatic-approved-updates) and the
[production runbook](PRODUCTION-CAIRN.md#installation-updates-and-temporary-offline-status).

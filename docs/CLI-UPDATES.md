# Agent tool versions and retired standalone updates

Standalone CLI updates were disabled at the Beacon cutover (#113). Their
workflow, candidate-image Dockerfile, deployment commands and source systemd
units have now been removed. Do not dispatch `cli-updates.yaml` or invoke
`scripts/cli-updates.mjs check|deploy` from external automation.

## Supported update path

Every application CI run, including tag releases, uses
`node scripts/cli-updates.mjs build-versions` to resolve the agent tool versions
before deciding whether to reuse an image. This command remains available at its
existing path. It prints JSON and, when `GITHUB_OUTPUT` is set, appends the exact
`codex`, `gh`, `claude` and `opencode` outputs consumed by `ci.yaml`.

| Tool | Official source | Channel | Native check in CI |
| --- | --- | --- | --- |
| Codex | npm `@openai/codex` | dist-tag `latest` | none |
| Claude Code | npm `@anthropic-ai/claude-code` | dist-tag `stable` | `initialize` model catalog |
| OpenCode | npm `opencode-ai` | dist-tag `latest` | turn and session resumption |
| GitHub CLI | GitHub `cli/cli` latest release | not draft or prerelease | none |

Every tool follows its channel into new minor and major releases. There is no
version range to widen by hand: a release that breaks its adapter fails CI
instead of shipping.

Claude Code follows Anthropic's
[stable release channel](https://code.claude.com/docs/en/setup#configure-release-channel):
a version "typically about one week old, skipping releases with major
regressions", published as the npm `stable` dist-tag. Its `latest` channel is
never used. On 24 October 2026 they differed: `stable` was 2.1.308 and `latest`
2.1.310; builds chose 2.1.308. Anthropic notes that a newly launched model can
require a version newer than the stable channel serves. Such a model becomes
available to Cairn when the stable channel reaches that version. Builds only
install a release the stable channel serves.

OpenCode documents its npm package `opencode-ai` as the install path and
publishes releases on the `latest` dist-tag. Only that dist-tag is read; the
others carry `0.0.0-*` development snapshots or older release lines. The adapter
speaks the HTTP session protocol of OpenCode 1.19.0 and later, and rejects older
releases at startup (`opencode::MINIMUM`). A `latest` release older than that
fails resolution.

Each resolved version must be a plain `MAJOR.MINOR.PATCH` release, so prereleases
and snapshots are rejected, and must not be deprecated on npm. A missing channel,
an unreachable registry, malformed metadata, a prerelease, or a deprecated channel
release fails the `resolve` job before any build, and its error names the source
that failed. No fallback version is guessed. When a vendor moves a channel back to
an older release, builds follow it, because the vendor withdrew the newer one.
For OpenCode, CI also tests that sessions of the release production serves resume
on the older resolved one, and back.

There are no default agent tool versions in the Dockerfile. CI passes the resolved
`CODEX_VERSION`, `GH_VERSION`, `CLAUDE_VERSION` and `OPENCODE_VERSION` build
arguments. The image build installs each tool at that exact version and fails
unless its `--version` output matches. The image records the versions as
`APP_*_VERSION`. `/health` reports them under `tools`. The container smoke test
checks that they equal the versions CI resolved. Native node bundles
(`deploy/nodes/bundle-native-tools.mjs`) refuse an executable whose version
differs from the image's resolved version. MicroVM guests are built from the same
runtime stage.

The `backend` job also runs the real engine releases it resolved through their
adapters, without credentials or inference against a vendor:

- OpenCode executes a turn against a local model server, without loading global
  config. Its session then resumes with the history it stored. Sessions created
  by the oldest supported release resume on the resolved one. Sessions also move
  both ways between the resolved release and the release production serves, as
  after an update or a rollback. The `resolve` job reads the served release's
  versions from that release's own validation evidence
  (`resolve-release.mjs served-tools`, from `CAIRN_BEACON_ORIGIN/health`). Evidence
  recorded before OpenCode was resolved names no version; those images shipped
  1.19.0, the oldest supported release. The same fallback applies when production
  or its evidence is unavailable, and the job log says so.
- Claude Code answers the `initialize` control request that model discovery uses,
  with synthetic credentials, and lists a catalog with its `default` alias.

Validation evidence (`image.json`, schema 3) records all four versions. An image
can be reused for the same source tree only when every resolved version matches
its evidence. Evidence recorded before Claude Code and OpenCode were resolved
names only Codex and GitHub CLI. It is never reused, so the first build after
this change validates a new image. Tools never change after image validation:
Claude Code runs with `DISABLE_AUTOUPDATER=1` and OpenCode with automatic updates
disabled.
Direct Docker builds must pass these build arguments too; see
[deployment setup](DEPLOYMENT.md) and the [agent toolkit](TOOLKIT.md).

### Qualifying an engine change

A new channel release ships through the normal validated build and its automated
checks above. A release that fails them blocks validation until a reviewed change
adapts the adapter, or bounds the engine in `scripts/cli-updates.mjs` with an
explanation. Claude Code behavior those checks cannot reach
without a subscription — `get_usage` quota fields, the OAuth client and token
endpoint, and resuming a real session — relies on Anthropic's stable channel
between qualifications. A change in an adapter that depends on engine behavior,
or a reported engine regression, needs the following checks before merge:

- OpenCode: run the native adapter test with the candidate binary:
  `OPENCODE_NATIVE_BIN=/path/to/opencode pnpm test:backend -p cairn-installation --test opencode -- --include-ignored`.
  Check the `/global/health` version, the session and event protocol, and
  resuming a session that the previous release created.
- Claude Code: run
  `CLAUDE_NATIVE_BIN=/path/to/claude pnpm test:backend -p cairn-installation --test claude -- --ignored --exact native_cli_lists_its_model_catalog_without_inference`,
  and compare a credential-free `initialize` control response from the candidate
  with the current release. Check the model catalog fields and
  aliases, and confirm that stored agent models remain valid `--model` values.
  Check the OAuth client and token endpoint in `accounts/claude.rs`. With a test
  account, check `get_usage` quota fields and resuming an existing session.
- All engines: keep their autoupdaters disabled at runtime and keep their
  private state layout compatible with retained conversations. Engine changes
  never rewrite persisted Cairn data. Follow the
  [installation data compatibility contract](PRODUCTION-CAIRN.md#installation-data-compatibility)
  if one would.

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

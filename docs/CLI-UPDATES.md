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

| Tool | Official source | Channel | Qualified series |
| --- | --- | --- | --- |
| Codex | npm `@openai/codex` | dist-tag `latest` | any |
| Claude Code | npm `@anthropic-ai/claude-code` | dist-tag `stable` | `2.x` |
| OpenCode | npm `opencode-ai` | dist-tag `latest` | `1.19.x` |
| GitHub CLI | GitHub `cli/cli` latest release | not draft or prerelease | any |

Claude Code follows Anthropic's
[stable release channel](https://code.claude.com/docs/en/setup#configure-release-channel):
a version "typically about one week old, skipping releases with major
regressions", published as the npm `stable` dist-tag. Its `latest` channel is
never used. On 24 October 2026 they differed: `stable` was 2.1.308 and `latest`
2.1.310; builds chose 2.1.308. Anthropic notes that a newly launched model can
require a version newer than the stable channel serves. Such a model becomes
available to Cairn when the stable channel reaches that version.

OpenCode documents its npm package `opencode-ai` as the install path and
publishes releases on the `latest` dist-tag. Other dist-tags carry `0.0.0-*`
development snapshots and are never considered.

Each resolved version must be a plain `MAJOR.MINOR.PATCH` release, so prereleases
and snapshots are rejected, and must not be deprecated on npm. A missing channel,
an unreachable registry, malformed metadata, a prerelease, or a deprecated channel
release fails the `resolve` job before any build. No fallback version is guessed.

A qualified series bounds an engine to releases its Cairn adapter was qualified
against. The OpenCode adapter speaks OpenCode 1.19's HTTP session protocol and
rejects other series at startup (`opencode::SERIES`). When a channel moves outside
its series, builds install the newest stable, non-deprecated release inside the
series that is not newer than the channel. The `resolve` job then emits a
warning naming both versions. If the series has no such release, resolution fails.
Widening a series is a reviewed change to `scripts/cli-updates.mjs` and the
adapter, after qualification as described below.

There are no default agent tool versions in the Dockerfile. CI passes the resolved
`CODEX_VERSION`, `GH_VERSION`, `CLAUDE_VERSION` and `OPENCODE_VERSION` build
arguments. The image build installs each tool at that exact version and fails
unless its `--version` output matches. The image records the versions as
`APP_*_VERSION`. `/health` reports them under `tools`. The container smoke test
checks that they equal the versions CI resolved. Native node bundles
(`deploy/nodes/bundle-native-tools.mjs`) refuse an executable whose version
differs from the image's resolved version. MicroVM guests are built from the same
runtime stage. CI also runs the real OpenCode release it resolved through
the adapter's native execution and resume test, against a local model server
and without credentials.

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

A new channel release within a series ships through the normal validated build.
Widening a series, or a change in an adapter that depends on engine behavior,
needs the following checks before merge:

- OpenCode: run the native adapter test with the candidate binary:
  `OPENCODE_NATIVE_BIN=/path/to/opencode pnpm test:backend -p cairn-installation --test opencode -- --include-ignored`.
  Check the `/global/health` version, the session and event protocol, and
  resuming a session that the previous release created.
- Claude Code: compare a credential-free `initialize` control response from
  the candidate with the current release. Check the model catalog fields and
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

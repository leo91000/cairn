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
| Claude Code | npm `@anthropic-ai/claude-code` | dist-tag `stable` | catalog, usage, turn, resumption |
| OpenCode | npm `opencode-ai` | dist-tag `latest` | catalog, turn, resumption |
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
CI also tests that sessions of the release production serves resume on the older
resolved one, and back.

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

- OpenCode lists its provider and model catalog. It executes a turn against a
  local model server, without loading global config, and its session resumes with
  the history it stored. Sessions created by the oldest supported release resume
  on the resolved one.
- Claude Code answers the `initialize` and `get_usage` control requests that model
  discovery and account usage use, with synthetic credentials, and lists a catalog
  with its `default` alias. An account refresh reads its `auth status` JSON. It
  executes a turn through the adapter against a local Messages API with a
  synthetic API key, and its session resumes with its history. Sessions created by
  its `legacy` release resume on the resolved one.
- For both engines, sessions move both ways between the resolved release and the
  release production serves, as after an update or a rollback.

The `resolve` job reads the served versions from the configuration of the
installation image production reports at `CAIRN_BEACON_ORIGIN/install/release`
(`scripts/served-tools.mjs`), and checks registry content against its digest.
Images built before an engine was resolved record no version; they shipped its
`legacy` release in `scripts/cli-updates.mjs` (Claude Code 2.1.280, OpenCode
1.19.0). When production or the registry cannot be read, no version is guessed:
the rollback checks are skipped with a warning, and the evidence records
`served: null`. Such evidence is never reused, so main or the release validates
again. The served release runs through the candidate's adapter, while a real
rollback runs its own image's adapter. When an adapter change needs a newer
engine release, set that engine's `minimum`: an older served release is then
skipped, with a warning. Main and tags reuse evidence only when its served
versions still match the release production serves.

Validation evidence (`image.json`, schema 3) records all four versions under
`tools`, and the served versions the rollback checks used under `served`. An image
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
checks above. A release that fails them blocks validation. There is no hold or
override setting: the remedy is a reviewed change that adapts the adapter or makes
`scripts/cli-updates.mjs` select a working release for that engine. Claude Code
behavior those checks cannot reach without a subscription relies on Anthropic's
stable channel between qualifications: the `auth login --claudeai` flow, quota
values from `get_usage`, the OAuth client and token endpoint, and subscription
inference. A change in an adapter
that depends on engine behavior, or a reported engine regression, needs the
following checks before merge:

- OpenCode: run the native adapter tests with the candidate binary:
  `OPENCODE_NATIVE_BIN=/path/to/opencode pnpm test:backend -p cairn-installation --test opencode -- --include-ignored`,
  and with `OPENCODE_NATIVE_RESUME_BIN` set to the previous release. Check the
  `/global/health` version, the session and event protocol, and the catalog.
- Claude Code: run the ignored native tests in `tests/claude.rs` the same way,
  with `CLAUDE_NATIVE_BIN` and `CLAUDE_NATIVE_RESUME_BIN`. Compare the credential-free
  `initialize` response with the current release's: the model catalog fields and
  aliases, and whether stored agent models remain valid `--model` values. Check
  the OAuth client and token endpoint in `accounts/claude.rs`. With a test
  account, check `claude auth login --claudeai`, `get_usage` quota values and
  resuming a subscription session.
- All engines: keep their autoupdaters disabled at runtime. Engine session state
  in account homes is persisted installation data. The
  [installation data compatibility contract](PRODUCTION-CAIRN.md#installation-data-compatibility)
  applies to it: the served release must still read what the new release writes.

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

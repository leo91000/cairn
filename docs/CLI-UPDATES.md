# Agent tool versions

Every application CI run, including tag releases, uses
`node scripts/cli-updates.mjs build-versions` to resolve the agent tool versions
before deciding whether to reuse an image. It prints JSON and, when
`GITHUB_OUTPUT` is set, appends the exact `codex`, `gh`, `claude` and `opencode`
outputs consumed by `ci.yaml`.

| Tool | Official source | Channel | Native check in CI |
| --- | --- | --- | --- |
| Codex | npm `@openai/codex` | dist-tag `latest` | plan-turn boundary |
| Claude Code | npm `@anthropic-ai/claude-code` | dist-tag `stable` | catalog, usage, turn, resumption, plan-turn boundary |
| OpenCode | npm `opencode-ai` | dist-tag `latest` | catalog, turn, resumption |
| GitHub CLI | GitHub `cli/cli` latest release | not draft or prerelease | none |

Every tool follows its channel into new minor and major releases. There is no
version range to widen by hand: a release that breaks its adapter fails CI
instead of shipping.

Claude Code follows Anthropic's
[stable release channel](https://code.claude.com/docs/en/setup#configure-release-channel):
a version "typically about one week old, skipping releases with major
regressions", published as the npm `stable` dist-tag. Its `latest` channel is
never used, even when it names a newer release. Anthropic notes that a newly
launched model can require a version newer than the stable channel serves. Such
a model becomes available to Cairn when the stable channel reaches that version.
Builds only install a release the stable channel serves.

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
Direct Docker builds must pass these build arguments too; see
[deployment setup](DEPLOYMENT.md) and the [agent toolkit](TOOLKIT.md).

The `backend` job also runs the real engine releases it resolved through their
adapters, without credentials and without inference against a vendor. As in
production, the CLIs may still contact their vendors' public endpoints:

- OpenCode lists its provider and model catalog. It executes a turn against a
  local model server, without loading global config, and its session resumes with
  the history it stored. Sessions created by the oldest supported release resume
  on the resolved one.
- Claude Code ships the OAuth client and token endpoint that account token
  rotation uses (`accounts/claude.rs`). It answers the `initialize` and
  `get_usage` control requests that model
  discovery and account usage use, with synthetic credentials, and lists a catalog
  with its `default` alias, offering exactly the effort levels agents may save
  (`EFFORTS` in `claude.rs`). An account refresh reads its `auth status` JSON. It
  executes a turn through the adapter against a local Messages API with a
  synthetic API key, and its session resumes with its history. Sessions created by
  its `legacy` release resume on the resolved one.
- For both engines, sessions move both ways between the resolved release and the
  release production serves, as after an update or a rollback.
- Claude Code and Codex each run a plan turn through their adapter
  (`tests/native_plan_turns.rs`) against a local model that asks for writes,
  network access, escalation and subagents. None may take effect, the proposed plan
  must reach the conversation, and the turn after a plan must write again. The job
  installs bubblewrap and socat, and lifts Ubuntu's restriction on unprivileged user
  namespaces, which both sandboxes need. `publish` depends on this job, so a release
  whose plan-turn boundary does not hold is not served. The short PR validation does
  not run it.

The `resolve` job reads the served versions from the configuration of the
installation image production reports at `CAIRN_BEACON_ORIGIN/install/release`
(`scripts/served-tools.mjs`), and checks registry content against its digest.
Images built before an engine was resolved record no version; they shipped its
`legacy` release in `scripts/cli-updates.mjs` (Claude Code 2.1.280, OpenCode
1.19.0). When production or the registry cannot be read, no version is guessed:
the rollback checks are skipped with a warning, and the evidence records
`served: null`. Such evidence is never reused, and a tag fails at once, before
building. Before deploying, the release job reads the served release again and
stops unless it still matches the versions its rollback checks used, or already
runs this release's own engines, as when a deploy is retried. The served release
runs through the candidate's adapter, while a real rollback runs its own image's
adapter. When an adapter change needs a newer
engine release, set that engine's `minimum`: an older served release is then
skipped, with a warning. Main and tags reuse evidence only when its served
versions still match the release production serves.

Validation evidence (`image.json`, schema 3) records all four versions under
`tools`, and the served versions the rollback checks used under `served`. An image
can be reused for the same source tree only when every resolved version matches
its evidence. Evidence recorded before Claude Code and OpenCode were resolved
names only Codex and GitHub CLI. It is never reused: such a tree validates a new
image. Tools never change after image validation:
Claude Code runs with `DISABLE_AUTOUPDATER=1` and OpenCode with automatic updates
disabled.

Installations follow approved, paired immutable images through
`cairn-installation-update.timer`. This host updater retains the persisted
deployment lease, runtime verification and rollback.
See [installation updates](INSTALLATION.md#automatic-approved-updates) and the
[production runbook](PRODUCTION-CAIRN.md#installation-updates-and-temporary-offline-status).

## Qualifying an engine change

A new channel release ships through the normal validated build and its automated
checks above. A release that fails them blocks validation. There is no hold or
override setting: the remedy is a reviewed change that adapts the adapter, or that
sets the engine's `channel` in `scripts/cli-updates.mjs` to the last working
version until it does. npm serves an exact version at the same URL as a dist-tag,
so the same validation applies.

Claude Code behavior those checks do not reach relies on Anthropic's stable
channel between qualifications:

- Without a subscription: the `auth login --claudeai` flow, quota values from
  `get_usage`, token rotation against the endpoint, and subscription inference.
- The native turn runs with YOLO access and calls no tool. Apart from plan turns,
  how a release applies the sandbox settings of restricted agents (`settings` in
  `claude_process.rs`), the `--permission-prompt-tool` approval flow, and MCP tool
  calls under `--strict-mcp-config` has no per-release native check. Production
  runner isolation still applies.

A change in an adapter
that depends on engine behavior, or a reported engine regression, needs the
following checks before merge:

- OpenCode: run the native adapter tests with the candidate binary:
  `OPENCODE_NATIVE_BIN=/path/to/opencode pnpm test:backend -p cairn-installation --test opencode -- --include-ignored`,
  and with `OPENCODE_NATIVE_RESUME_BIN` set to the previous release. Check the
  `/global/health` version, the session and event protocol, and the catalog.
- Claude Code: run the ignored native tests in `tests/claude.rs` the same way,
  with `CLAUDE_NATIVE_BIN` and `CLAUDE_NATIVE_RESUME_BIN`. Compare the credential-free
  `initialize` response with the current release's: the model catalog fields and
  aliases, and whether stored agent models remain valid `--model` values. In a
  restricted agent, check that a write outside its writable roots is refused, that
  a permission prompt reaches Cairn, and that an MCP tool call completes. With a
  test account, check `claude auth login --claudeai`, `get_usage` quota values and
  resuming a subscription session.
- Plan turns (Claude Code and Codex): CI runs `native_plan_turns` for the resolved
  releases. To run it elsewhere:
  `CLAUDE_NATIVE_BIN=… CODEX_NATIVE_BIN=… pnpm test:backend -p cairn-installation --test native_plan_turns -- --ignored`.
  It needs bubblewrap, and socat for Claude Code. On a native node,
  `NATIVE_PROBE_PATH` adds a directory to the engine's `PATH`,
  `NATIVE_PROBE_DROP_CAPS` drops inherited capabilities, and
  `NATIVE_PROBE_PRIVATE_TMP` gives Codex its own `/tmp` (with `TMPDIR` outside
  `/tmp`); see [the plan-turn decision](adr/0077-plan-turns.md#qualification).
- All engines: keep their autoupdaters disabled at runtime.

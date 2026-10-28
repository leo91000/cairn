# PR validation

## Before opening or merging a PR

1. Read the complete diff against the current merge base, including relevant
   uncommitted changes. Map each behavior to its callers, indirect dependants,
   shared contracts and regression suites. Include failure paths, authorization
   and web/Android/MCP parity where the behavior crosses those boundaries.
2. Execute the affected suites locally. Include the whole suite when the affected
   area is uncertain; changes to shared infrastructure, dependencies, contracts
   or test launchers require broad coverage. A narrow test filter supplements
   the enclosing regression suite. Fix formatting with the repository commands.
3. Resolve every failure using the ownership rule in `AGENTS.md`. Reproduce an
   apparent pre-existing failure on the base in an isolated checkout with the same
   command and host conditions. Keep separate repairs in separate branches or
   workspaces, and give the repair task the failing command, evidence, base SHA,
   project, and authority to open its PR, run `review-until-clean` and merge.
   Check that the repair merged and rerun the affected checks before treating a
   prerequisite as resolved. Follow independent repairs through merge as well.
4. Validate the final source. A source correction, merge conflict resolution or
   base update invalidates earlier results for the affected suites. Record the
   exact tested commit and whether the checkout was dirty. Test the committed
   source before merge; a dirty-checkout result alone is insufficient.
5. Put the commands, suite selection rationale, results, unexecuted checks and
   linked repair PRs in the PR description. Wait for all selected GitHub checks
   to succeed as well. A timeout, infrastructure failure or missing device is
   incomplete validation, and repeated retries do not establish a flaky test as
   reliable. Preserve the first failure when investigating a flake.

## Shared local and GitHub commands

`pnpm validate:pr` runs the full core: `quality contracts rust backend`.
Select explicit suites to add platform coverage or run the same command as a CI
job. `pnpm validate:pr all` adds Android and the complete browser suite.
`pnpm validate:pr --dry-run all` prints the commands without running them.
The launcher prints the source commit/tree, dirty state, commands and final result;
it stops on the first failure and isolates live agent credentials before tests.
Keep logs outside tracked files. A successful selected plan proves only those
suites; the behavior map determines the additional qualification required.

| Suite | Coverage and prerequisites |
| --- | --- |
| `quality` | Formatting, lint, JS/TS regression tests, type checking and web build. |
| `contracts` | Regeneration without a diff, Kotlin contract corpus, generated API schemas. Requires the pinned Rust toolchain and JDK 21. |
| `rust` | Workspace Clippy on every target, with warnings denied. Requires native build dependencies. |
| `backend` | Workspace Rust tests and documentation examples through the isolated backend launcher. Use a disposable Postgres database for Beacon tests. |
| `android` | Spotless, the full debug unit suite and Android lint. Requires JDK 21 and the Android SDK. |
| `browser` | Builds the real fixture binaries and web app, then runs all Playwright projects. Requires browser binaries, native dependencies and disposable Postgres; see [browser setup](../../tests/e2e/README.md). |

These suites are a baseline, not the complete inventory of tests. Inspect the
affected code and the full qualification workflows for specialist coverage:

- UI changes require the relevant browser and Android device journeys;
  [Android validation](../../apps/android/VALIDATION.md) documents the device tools.
- Networking changes require the affected topologies in
  `tests/e2e/network-qualification.spec.ts` and the native Android direct-network scenarios;
  use the commands and environment in `.github/workflows/ci.yaml`.
- Native storage, process ownership, installation and update changes require
  the `node-checks` and `native-runtime` qualifications in that workflow, plus
  [storage measurements](../STORAGE-COMPRESSION.md#qualification) when applicable.
- Firecracker changes require `scripts/test-firecracker-discard.sh` where
  relevant. Container/release changes require the corresponding image smoke
  tests. Use disposable infrastructure with the required KVM/ublk capabilities.

If the local host cannot execute a required qualification, run the appropriate
GitHub workflow manually on the branch and wait for its result. A manual run can
provide test evidence for the PR; it cannot supply reusable release provenance.

## GitHub responsibilities

`PR validation` runs on every PR and push to main: the `quality` suite always,
contracts and Android unit/lint checks when their inputs change. Its aggregate
`PR validation required` check succeeds only when each selected job succeeds.
Configure that check as required in the repository's main-branch rules. The
targeted Firecracker regression workflow remains on relevant changes.

The complete `Quality and container` and `Android` workflows run nightly, on
manual dispatch, and on version tags. They retain the existing browser/device,
network, native, container and release publication gates. Nightly qualification
does not deploy production. A version tag may reuse successful nightly evidence
for its exact source and build configuration; otherwise it runs full qualification.
Historical complete main/PR evidence remains compatible. The short PR workflow
and manual qualifications never supply reusable release provenance.

Nightly validation catches integration drift; it does not replace the local
pre-merge checks. Keep the same failure ownership through repair PR and merge
for failures discovered by nightly or release qualification.

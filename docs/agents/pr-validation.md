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

For a focused physical backend-migration check, dispatch `ci.yaml` with
`backend-only=true` and `backend-qualification=backend-migration`. The optional
case runs in a separate 30-minute disposable job after the normal backend job
succeeds. The new job checks out the same commit and uses the regular isolated
loopback-S3 launcher to build and run exactly the selected case; it shares no
compiled artifacts with the backend job. The default is `none`; this selected
case does not replace native-runtime or other affected specialist
qualifications.

The backend-only dispatch also runs `rust-check` in its own disposable job
because release resolution is skipped in that mode. Keep the all-target Clippy
build separate from the backend test build so their Cargo artifacts do not
compete for one runner's disk budget.

For a full dispatch that resolves to a reusable image, explicitly selecting
`backend-migration` also runs the backend job and holds reuse promotion until
the selected qualification succeeds. Leaving the choice at `none` preserves
the existing reuse and tag paths.

For a full branch qualification without writing container images or release
tags, dispatch `ci.yaml` with `validation-only=true`. This still runs the
applicable backend, node, native-runtime, browser and device jobs; image build,
publish, promotion and deployment jobs are skipped.

### Bounded deadline diagnostics

When a backend/native job reaches its deadline before preserving evidence,
`ci.yaml` also accepts an opt-in diagnostic dispatch on the observer branch:
`backend-only=true`, `validation-only=true`, `backend-qualification=none`,
`ci-diagnostic=backend` or `native`, and a full `diagnostic-candidate` commit SHA.
Native diagnostics additionally require the full `diagnostic-reference` SHA.
Only the selected job runs; release resolution, builds for the normal matrix,
publication and deployment are excluded. This is evidence acquisition and never
reusable release provenance or a substitute for the complete qualification.

The observer remains at the workflow commit while isolated worktrees hold the
pinned sources. Backend phases are insertion-only additions to that source's
isolated launcher. Native diagnostics build both exact sources, checksum their
binaries and keep each archived private fixture paired with its own binaries.
Both fixtures receive the same insertion-only phase/stack observer; artifact
manifests preserve the original and observed hashes. The comparison does not
silently move to the observer commit's first parent.

The seven native memory/garbage preparation tests run after the candidate
qualifier has been produced and verified. Their explicit
`CAIRN_TEST_NATIVE_QUALIFIER` points to that candidate's existing binary; the
diagnostic route reuses the pinned pair build, and the normal route reuses its
downloaded candidate. Host/venv/ublk preparation and Rust/libclang setup still
precede compilation. Diagnostic preparation tests share the job's existing
clock and evidence reserve.

The existing job limits remain 30/90 minutes. A supervisor uses GitHub's actual
job `started_at`, including setup/service time, and stops the command two minutes
before that limit to leave room for evidence upload. A diagnostic interruption
returns nonzero. It retains bounded phase, CPU/IO, wait-channel/kernel-stack and
cgroup samples from its owned process/session, plus Python stacks without local
values for the native fixture. It reads no process command lines or environment.
Backend trace output remains in `backend-test-trace`; native evidence and source
manifests remain in `native-storage-evidence`.

The private Linux leader enables and verifies child-subreaper ownership before
starting the command. Orphaned descendants remain adopted by that leader even
if they create a new session and close their standard streams. This is a
process-lifecycle mechanism on the disposable runner, not a host sandbox.
Cleanup signals require matching PID/start-time identity. Incomplete process
inventory, an unproven leader, inherited pipes that do not close, or an unclosed
process produce an explicit `cleanupUnknown`/non-success result. SIGKILL cannot
prove termination of a process blocked in kernel I/O. The supervisor closes its
own pipe/IPC handles within a bound so that such a process cannot prevent the
reserved artifact step. Systemd fixture units outside the owned ancestry are
observed separately, not killed by a process-name sweep; their final cleanup is
not proved by the generic supervisor. All of this runs on disposable CI hosts.

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

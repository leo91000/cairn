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

Full qualifications run the resolved OpenCode and Claude Code native adapters,
plus the Plan boundary checks for the resolved Claude Code and Codex releases,
in one separate 30-minute `cli-qualification` job after the normal backend suite.
It checks out the same commit, builds its test binaries on its own runner and
preserves the native adapter, supported-release resume and rollback checks.
The backend job retains its complete test workload, measurements, namespace
qualification and doctests within its existing 30-minute budget. Backend-only
and diagnostic dispatches skip this job; they do not resolve engine releases.
The post-suite bootstrap/terminal measurements and namespace test compilation keep
`--workspace --lib`, retaining the dependency features used by the workspace suite.
Package-only selection changes Cargo's unified dependency graph and rebuilds the
installation test library. Exact case filters and the namespace executable check
still select the intended installation tests.
Image publication requires both jobs to succeed. When physical migration is
selected for a full run, it waits for both jobs; a backend-only migration run
continues to require only the backend suite.

The backend installs nextest with `scripts/install-nextest-ci.mjs`. Its official
GitHub release asset ID, size and SHA256 are pinned; acquisition uses the release
asset API because the public download URL has returned persistent 504s. The
digest also matches the immutable install-action manifest cited in the helper.
One 120-second download budget covers the API response, its single allowed
release-assets redirect and body. No fallback installer or job retry is used.
The three-minute step remains inside the unchanged 30-minute backend job.
Integrity and executable version verification precede replacing the tool.
Closed response metadata on stderr distinguishes the API and asset status/type,
numeric size/quota headers and the failure phase. It records no URL, signed
redirect query, credentials or response body. The success receipt stays on stdout.
When updating the pin, verify the official immutable release metadata and digest,
then run the complete backend qualification. Mock acquisition and workflow tests
do not prove network availability or replace that qualification.

Before backend or CLI test compilation, the disposable GitHub-hosted Ubuntu
runner requires 32 GiB available on the build filesystem. When capacity is
insufficient, it reclaims a fixed allowlist of unused platform SDK directories
(Android, .NET and Swift), stopping as soon as capacity is sufficient. Necessary
reclamation has a 120-second budget; already sufficient runners delete no SDKs.
The fixed preparation refuses local/self-hosted accounts, other job roles, protected build locations,
SDK symlinks or mounts, explicit native toolchain overrides, Cargo configuration
files and active native tools. It changes no compiler flags or test commands.
The threshold follows an observed 22.49 GiB completed-run allocation plus
headroom, not a measured peak bound. An insufficient runner fails before
compilation with its capacity recorded, instead of losing logs to ENOSPC.

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

### Bounded native dependency setup

The five native test-dependency callers (`rust-check`, `backend`,
`cli-qualification`, `backend-migration-qualification` and `node-checks`) use
`scripts/install-native-ci-dependencies.mjs`. Its closed native profile preserves
`libclang-dev age e2fsprogs fakeroot`; update/install have 120/360-second work
budgets, noninteractive sudo and bounded acquisition/lock waits. Phase markers
contain fixed status fields, and owned process groups are cleaned before returning
failure, timeout or interruption. The ten-minute step bound and existing job
limits remain unchanged. Setup is not retried. An independent privileged GNU
`timeout`/Bash guard keeps the APT deadline and group cleanup live when Node's
control loop stalls. A private group-registration/admission exchange precedes
package execution; the controller retains that group for cleanup if the shell
is lost. A private cancellation marker reaches the shell independently of Node's
control socket and sudo's signal relay; after APT cleanup the shell retires a
blocked controller before the launcher can return. If marker acquisition fails,
the launcher retains the admitted owner's original work deadline instead of
abandoning it early. The guard also verifies the controller's PID/start-time
identity and settles its registered APT group if that controller disappears.
A private per-phase closure receipt is published after cleanup; the launcher
waits for it even after a failed sudo/controller exit. Receipt acquisition uses
the existing kill-after allowance clipped to the original outer phase budget.
Missing or unreadable closure remains explicit unknown/non-green, retaining an
original failure code and refusing the next phase. Neither side starts subsequent
phases after failure.

The helper retains a closed `android-network` profile (90/90 seconds), but the
Android direct job uses the newer `prepare-network-tools.mjs` preflight instead;
it must not regain broad APT installation. Other distinct package sets remain
outside this helper.

`pnpm exec vitest run scripts/tests/native-ci-dependencies.test.mjs` exercises
success, failure, timeout, interruptions, delayed privilege entry, orphan cleanup,
stalled Node supervision (including cancellation), lost privileged controllers
or shells, closure-receipt failure and bounded diagnostic output. Repeat with
`CAIRN_TEST_REAL_SUDO=1` on a disposable or authorized sudo-capable host to run the same cases through `/usr/bin/sudo -n`
and assert UID 0 in the synthetic APT executable. No real packages are changed by
these fixture tests. The short PR quality job runs both routes. Full validation
also runs the real package setup in its selected native jobs.

### Bounded deadline diagnostics

When a backend/native job reaches its deadline before preserving evidence,
`ci.yaml` also accepts an opt-in diagnostic dispatch on the observer branch:
`backend-only=true`, `validation-only=true`, `backend-qualification=none`,
`ci-diagnostic=backend` or `native`, and a full `diagnostic-candidate` commit SHA.
Native diagnostics additionally require the full `diagnostic-reference` SHA.
Only the selected job runs; release resolution, builds for the normal matrix,
publication and deployment are excluded. This is evidence acquisition and never
reusable release provenance or a substitute for the complete qualification.

For one targeted Avatar/accepted-authority observation, keep the same diagnostic
inputs above and additionally select `backend-diagnostic-selector=avatar-authority`
with `ci-diagnostic=backend`. The default selector is `full`; the selector is
ignored outside the existing backend diagnostic action. No new workflow needs
registration. Unknown selector values fail closed inside that action.

The fixed wrapper runs `cargo build --locked -p cairn-installation --bin cairn`
from the immutable candidate, then exactly once:

```sh
node scripts/test-backend.mjs --nextest -p cairn-installation --lib --test-threads 8 --no-fail-fast --color never --status-level all -E 'test(agent_avatars::tests::) | test(authorization::tests::) | test(authorization::jobs::tests::)'
```

Both commands stay inside the existing owned-command supervisor and actual
30-minute job clock with its two-minute evidence reserve. The existing source,
dependency, private trace, publication and closure checks still apply. The wrapper
forces this candidate's `target` directory, rejects cross-target builds, checks
source identity before/after execution, and records worker bytes/SHA256 before
launching the selected tests and checks the hash afterwards. The package-scoped
`--lib` launcher deliberately deletes `CAIRN_TEST_CODE_WORKER` and does not build
it; the grant fixture uses its existing candidate-local `target/debug/cairn`
fallback. No live or downloaded worker is substituted.

`backend-diagnostics/avatar-build.json`, `avatar-worker.json` and
`avatar-result.json` retain separate build/worker/test-return evidence when each
boundary is reached. Missing boundaries remain unknown. Rolling/final receipts
project only the closed worker provenance; a zero guard return requires that
provenance. Avatar provider arrival follows the existing fixture's request versus
tracked-generation completion lifecycle, with production RPC deadlines bounding
startup. The case helper delegates that lifecycle and adds closed case/elapsed/state
context on failure before resuming its original panic. It adds no provider-arrival
timer or polling. Publication-state waits retain five seconds and all publication
checks. `resolverCount` measures the
grant fixture's thread-resolver HTTP posts, not total subprocesses or worker time.
No raw agent/run snapshots, prompts, credentials or RPC frames are projected.

Start with one execution and preserve its result. There is no retry/iteration
input or automatic rerun; at most three separate, explicit bounded observations
may be chosen later. Preserve each failure independently. A targeted pass is not
full backend qualification, a causal fix, or reusable release evidence. Run the
normal full eight-thread qualification separately before release/merge; do not
skip or weaken it because the diagnostic passed.

The observer remains at the workflow commit while isolated worktrees hold the
pinned sources. Backend phases are insertion-only additions to that source's
isolated launcher. Native diagnostics build both exact sources, checksum their
binaries and keep each archived private fixture paired with its own binaries.
Both fixtures receive the same insertion-only phase/stack observer; artifact
manifests preserve the original and observed hashes. The comparison does not
silently move to the observer commit's first parent.

Backend-only diagnostics provision the candidate's Node resolution context
after setup-pnpm has installed the observer's dependencies. The bridge requires
identical package/lock/workspace bytes in both pinned Git sources and working
trees, a physical complete pnpm12.3.4 installation, its exact application lock
document and the declared versions of every direct provider, official-client,
stdio and tsx dependency. Root importer identities (including peer versions)
determine the canonical virtual-store package paths. The receipt binds those
paths, manifest bytes, and each required ESM entrypoint's canonical path and
bytes; missing exports/files and in-store link retargets are refused. Node's ESM
resolver probes imports without executing packages. Unsupported store identities
requiring shortening are refused. This binds the required direct entrypoints to
the frozen setup; it is not cryptographic attestation of all transitive code.
Missing/mismatched/unsupported installations, escaping paths and preexisting
candidate node_modules are refused. No package install, NODE_PATH or replacement
fixture is used. The foreground startup revalidates the acquired bridge and
receipt, rechecks resolution from the candidate fixture ancestry, and includes
its hash before the heavy command. Native and ordinary
routes do not use this bridge; its provenance file uses existing artifacts.

The eight native memory/garbage preparation tests run after the candidate
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

Backend diagnostics also use a foreground Node 24 action to publish a small
immutable startup receipt before starting the heavy command. It verifies the
pinned source/API clock and the explicitly checked-out, immutable
`actions/upload-artifact` v7.0.1 publisher. Artifact runtime credentials stay in
the publication process environment; they are removed from the tested child.
The receipt contains source/observer identities, clock, supervisor/launcher
hashes and acquisition state, with no raw output or environment.

At most five rolling checkpoints follow (first after 30 seconds, then six
minutes after each preceding upload completes), plus a final checkpoint. Each
projects the existing bounded sample ring onto fixed phase/process/counter
fields. A publication needs a successful finalized artifact ID and digest;
missing/failed/timed-out acquisition stops diagnostics with a nonzero result.
Seven publication attempts plus the existing before-build, test-trace and
after-build artifacts stay within a conservative ten-artifact job allowance.
Nothing overwrites or deletes earlier checkpoints. Uploads have a 20-second
bound clipped to the actual API clock's remaining time; startup/periodic
publication stays before the reserve, and final publication uses the existing
reserve. Cancellation reaches the credentialed publisher. The clock is checked
again after startup commit before any heavy child can start. The full backend
flags and normal route remain unchanged.

Pending rolling checkpoints are cancelled when command observation settles,
before mandatory owned-process cleanup uses the reserve. An upload already in
progress must still return closed; its failure remains non-green. Final
publication still follows guard cleanup and private trace closure.

The backend foreground action captures all owned stdout/stderr bytes into its
private raw trace independently of the public console. Raw workload output is
not relayed to that console. Its asynchronous file writes, close and byte/hash
accounting must settle within a bound taken from the existing evidence reserve;
write/close uncertainty makes the action non-green. Public progress is advisory,
and stopped, slow or closed public readers cannot gate private capture or the
action's exit after mandatory closure.

The final backend checkpoint includes the typed owned-command guard return and
the private trace's state, bytes and hash scope. A local `action-lifecycle.json`
also records the diagnostic return, publication-return boundary and exit
decision before retiring the public streams and exiting the foreground process.
Neither this marker nor the final artifact proves OS process or runner/job
exit; both explicitly label foreground exit `not-observed`. The runner's actual
exit/step result remains a separate required observation. Raw trace errors,
unclosed owned work, failed publication or original workload failure cannot be
turned into a successful exit. No artifact is added to the existing allowance.

If local filesystem completion remains unknown, Node's own teardown may also
wait for that I/O. A separately typed non-green retirement can signal only the
foreground process's own PID, and only after the guard's final complete
inventory shows zero owned processes and every publisher has returned closed.
That decision claims neither filesystem closure nor a numeric OS exit code.
When either process-closure proof is unknown, this forced retirement is refused;
the non-green decision and OS-exit observation remain separate.

Previously committed artifacts survive loss of the runner without depending on
an after-step upload. A loss before the first commit, the last partial upload,
and the interval since the last committed sample remain **unknown**. Checkpoint
creation proves evidence acquisition, not successful qualification or recovery
of an unexplained earlier failure. The existing backend tail checks still run
after the observed backend command; its checkpoint is not a whole-job PASS.

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

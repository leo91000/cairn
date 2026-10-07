# CI and release performance

Sections are dated; the release measurements below used the v0.1.5 workflow on
2026-09-10. Timings are observations, not guarantees: hosted runner capacity,
package mirrors, cache state, and registry transfers vary. Elapsed time includes
job scheduling; runner time sums job durations.

## Paired production images, 2026-10-07

The `official-image` job builds the `official` Dockerfile target separately from
VM/kernel layers and smoke-tests the exact published digest with disposable
Postgres. It also verifies the bundled SPA/assets, installer release, sign-in,
session persistence after restart and database readiness. Existing installation,
runner, retention, browser, network and quality checks still gate publication.
Both images use immutable digests, SBOM and provenance; one schema-3 artifact
records both digests for the same Git tree and run. Single-image evidence is
ineligible, so its first release falls back to complete validation. Promotion
accepts a repository parameter and tags both digests; deployment verifies official
health and the paired installation approval. Main/PR/manual runs never deploy.
The official job has its own registry cache and no KVM dependency. No elapsed-time
improvement is claimed for this additional job before hosted measurements.

## Pull request to release, 2026-10-04

By October a release paid for the same validation twice:

| Path | Elapsed |
| --- | --- |
| Pull request CI, e.g. [run 37221466077](https://github.com/leo91000/leo-agent-manager/actions/runs/37221466077) | 15–20 min |
| Full re-run of the merged tree on main | 17–21 min |
| Tag reusing main's image, then deploying | 1–4 min |
| Tag on a commit main never validated, e.g. [v0.52.11](https://github.com/leo91000/leo-agent-manager/actions/runs/37221707803) | 23 min |

**Where the time went:**
- The image job was the critical path at 14–19 min:
  - Deleting unused runner SDKs: 65–149 s.
  - Docker build: 8–12 min, including 238 s for the application crate's
    release compile.
  - Serial smoke tests: about 4 min. Main also pulled its pushed image back,
    which took about 73 s.
- The quality job (7–10 min) had to finish before the browser jobs (about
  5 min) could start.
- Playwright had no retries.

**Changes:**
- Main and tags reuse validation for an identical tree, including that of a
  pull request from this repository (see [Changes retained](#changes-retained)).
- Browser jobs start from a dedicated build job, and journeys run on two shards.
- SDK deletion happens only below 40 GB free. Hosted runners had 87 GB
  available, and a full image job used 27 GB.

Measured on PR #74:

| Step | Before | After |
| --- | --- | --- |
| Pull request CI, elapsed | 15–20 min | 8m50s ([run 37243176845](https://github.com/leo91000/leo-agent-manager/actions/runs/37243176845)) |
| Build, then browser tests | 12–15 min (quality, then browsers) | 6.9–7.9 min (build about 2 min) |
| Image job setup before the build | 1–2.5 min | 8 s |
| Image publication | 42 s local load (PR only) | 40 s SBOM + 110 s export and push, so the image can be released |
| Application crate compile | 238 s | 222 s with 16 codegen units; not retained |
| Smoke tests | 3m40s; about 5 min on main with the pull | 3m45s without a pull |

How to read these numbers:
- The 8m50s run changed no Rust source, so its application compile came from
  the shared layer cache. Its image job took 8m16s and the quality job 8m30s.
- A pull request that changes the backend adds the release compile. Its image
  job takes about 12.5 minutes and remains the critical path.
- [Run 37241143537](https://github.com/leo91000/leo-agent-manager/actions/runs/37241143537)
  also rebuilt the dependency layer once (245 s) because the manifest had
  changed. It took 18m17s.

What is left on the image path is mostly serial:
- the release compile;
- pulling cached runtime layers from the registry (about 45 s);
- guest disk creation (44 s);
- publication;
- the smoke tests.

Faster, cache-persistent runners are evaluated in
[CI runner research](CI-RUNNERS-RESEARCH.md).

**Expected path to production:**
1. Pull request CI.
2. On merge, main promotes the same image within about a minute. This requires
   that the branch contained main's tip when its CI passed; GitHub's "Update
   branch" does this.
3. A tag promotes and deploys that image. The deployment step takes about
   3 minutes.

**Rejected:**
- **16 codegen units for the application crate.** About 7% is within
  run-to-run variation and does not justify a less optimized binary.
- **Concurrent smoke tests.** The runner smoke test asserts host-derived budgets,
  and the retention test asserts memory admission, so concurrent runs risk new
  flakes.

## Release measurements

| Run | Release path | Elapsed |
| --- | --- | --- |
| [v0.1.5](https://github.com/leo91000/leo-agent-manager/actions/runs/34419219831) | Original serial quality, image build/load, smoke test, push, deploy | 10m19s |
| [v0.1.6-rc.1](https://github.com/leo91000/leo-agent-manager/actions/runs/34421319030) | Reuse a completed main validation and its exact image | 1m34s |
| [v0.1.6-rc.2](https://github.com/leo91000/leo-agent-manager/actions/runs/34421520389) | Tag deliberately pushed before main existed; full validation fallback | 5m23s |
| [v0.1.6-rc.3](https://github.com/leo91000/leo-agent-manager/actions/runs/34422386090) | Atomic main + tag push; wait for main, then reuse | 5m00s |

The fallback run includes the first cache population for the new Dockerfile.
Main at the same commit completed in
[3m55s](https://github.com/leo91000/leo-agent-manager/actions/runs/34421600808).
The tag-only experiment confirms that reuse is optional: a release still gets all
checks when there is no qualifying main run.

The simultaneous-push experiment spent 239 seconds waiting for main in the
resolver, then completed its deployment job in 48 seconds. Its own quality and
image jobs were skipped. This verifies that a fresh main + tag push shares the
validation; it is distinct from timing a tag after main has already passed.
The original [v0.1.5 main run](https://github.com/leo91000/leo-agent-manager/actions/runs/34419217324)
took **8m53s** (528 aggregate runner seconds), without deployment.

## Changes retained

- Build and smoke-test the candidate image alongside quality checks. Push it by
  immutable digest; publish release/latest tags only when quality, every browser
  group, and the image job succeed.
- On main and on a release, reuse a successful run of this workflow that validated
  the exact same Git tree: a `push` to this repository's `main`, or a pull request
  from a branch of this repository. A pull request validates GitHub's merge commit.
  A run writes its own evidence, so the head commit GitHub recorded for the run
  must have that tree too. The validating workflow is then part of the released
  code, and a pull request qualifies only if its branch contained main's tip. The
  `validated-image-<tree>` artifact must match the repository, tree, run ID,
  schema (now 3), both SHA-256 digests, and Codex/GitHub CLI versions freshly resolved for main
  or the release. Fork pull requests and manual runs cannot supply release proof.
  Missing proof, an outdated branch or older tool versions runs full CI with a new
  image.
- After reusing a pull request image, main still runs the quality checks and
  refreshes the shared BuildKit cache, without gating any release. The quality
  checks also save the Rust cache when `Cargo.lock` changes. Deployment verifies
  the commit baked into the reused image.
- A tag waits for concurrent main validation instead of starting a duplicate build.
  Completed evidence is retained for 90 days. The image wait is bounded at 40
  minutes, covering the main image job's 35-minute limit. If main is still pending
  at that deadline, fail and ask for a retry instead of starting a duplicate build.
- Build once, preserving SBOM and provenance, then smoke-test the published digest.
  Pull requests from this repository build into Docker's containerd store, which
  cannot push by digest. They publish under one candidate tag that moves with each
  run, and the smoke tests check that the local copy is the published digest. Only
  digests are ever promoted. Fork images stay local and use no registry
  credentials. Promotion uses the runner's Buildx client without starting another
  BuildKit daemon.
- Give browser projects separate real applications, SQLite databases, workers, and
  production rate limiters. Keep ordered persistence journeys together; run the
  four Chromium/WebKit light/dark layout matrices independently. This removes
  deliberate waits for one shared rate limiter without weakening that limiter.
- In CI, use separate runners for two journey shards, Chromium layouts, and two
  WebKit shards, with at most two workers per runner. A dedicated build job shares
  the frontend and Rust backend through artifacts, so neither the browser tests
  nor the image job, whose smoke test uses the official binary, wait for the
  quality checks. Retry a failed browser test once and retain evidence.
- Run browser jobs in the official Playwright Noble image, pinned by version and
  digest to the installed `@playwright/test`. Check their versions before tests.
  This removes repeated browser and OS-library installation: Ubuntu package
  downloads took over seven minutes and exhausted the ten-minute job budget in
  two successive attempts on 2026-09-30. Keep the test budget unchanged.
- Run TypeScript checking once through `pnpm build` inside `pnpm check`.
- Set commit metadata after stable Docker tool layers. Keep all runtime tools,
  UID 1000, health checks, persistence checks, and exact deployed-commit validation.
- Check Docker health every second during startup and poll deployment health every
  two seconds. Deployment summaries report update, restart, and readiness timings.
- Cancel superseded main/PR runs; serialize production deployments without cancelling
  them. Keep screenshot/trace artifacts and a manual browser-worker comparison input.

## Controlled experiments

### Android tag baseline, 2026-09-24

The [v0.30.0 Android tag run](https://github.com/leo91000/leo-agent-manager/actions/runs/35983749505)
took **20m26s** despite the same commit already having passed Android CI on main:
the check job repeated **16m20s**, then publication took **4m00s**, including
**3m24s** rebuilding the signed release. The parallel
[server tag workflow](https://github.com/leo91000/leo-agent-manager/actions/runs/35983749704)
reused its validated image and finished in **2m27s**.

Android now archives the optimized unsigned APK after successful validation and
can reuse it for the exact commit and version. A tag verifies the artifact, signs
it with the existing key and publishes it without invoking Gradle. Missing or
incompatible evidence still requires the full pipeline. See
[Android updates](../android/docs/UPDATES.md#fast-tag-publication) for the guards.
The Gradle task-output cache is enabled separately from the dependency cache.

These changes remove duplicated work; a new hosted tag duration has **not yet
been measured**. A tag after main passes should only pay resolver, SDK setup,
signing and publication costs. An atomic main+tag push still waits for actual
validation once. The older server measurements below remain historical results.

### Earlier server experiments

Local Docker builds changed only the commit argument between warm builds. The old
Dockerfile took **21.93s** because commit metadata invalidated tool installation;
moving it after the stable layers took **1.65s**. CI's first run still has to populate
those new cache keys, so the local number is not a cold hosted build prediction.

The isolated local browser suite passed all nine tests with two, four, and five
workers: approximately **102s**, **56.5s**, and **39.2s**, respectively. The original
hosted serial browser step took **317s**. Hosted five-worker trials took **99s** and
**126s**; a same-commit four-worker trial took
[110s](https://github.com/leo91000/leo-agent-manager/actions/runs/34421600152).
Local hardware and hosted runner contention differ, so local timings alone do not
establish the best CI worker count.

A second same-commit hosted comparison took
[129s with five workers](https://github.com/leo91000/leo-agent-manager/actions/runs/34422050574)
and [136s with four](https://github.com/leo91000/leo-agent-manager/actions/runs/34422052319).
Five was retained for local runs: it won locally and in the repeat hosted pair,
but the hosted samples overlap and do not establish a large advantage over four.
Two on one runner was rejected after the slower local trial. The historical
single-runner workflow remains reproducible at the rc.2 tag.

Those warm runs completed their image jobs in **85s** and **71s**, versus **174s**
originally. Their actual build/push steps took **40s** and **34s**; the rest includes
runner setup, pulling and smoke-testing the exact published digest, and cleanup.
The existing GitHub Actions cache was retained after the warm measurements.

Finally, [three browser runners](https://github.com/leo91000/leo-agent-manager/actions/runs/34422700748)
completed main CI in **3m16s**, compared with
[4m04s on one runner](https://github.com/leo91000/leo-agent-manager/actions/runs/34422386493)
in the preceding configuration and **8m53s** originally. Aggregate runner time was
**389s**, versus **357s** in the preceding experiment and **528s** originally.
The selected configuration trades about 9% more runner time than that intermediate
version for about 20% lower elapsed time, while both measures improve on the original.

The three browser steps took **22s** (journeys), **50s** (Chromium layouts), and
**91s** (WebKit layouts). All nine tests passed. WebKit is now the longest group;
the measurements do not claim that further splitting would be cost-free or faster.

The startup health experiment took **6.19s** before and **2.28s** after adding the
startup interval. The original hypothesis that a 30-second health interval alone
explained deployment latency was rejected: the old image was already healthy in
about six seconds. The full rc.2 Coolify operation took **34.5s**, including **32.8s**
waiting for the new commit. Image transfer and restart still matter.

Removing the duplicate type check reduced a local `pnpm check` trial from **9.48s**
to **7.15s**, with lint, all 55 unit tests, type checking, and the production build
still passing. Small single-run differences should be treated as approximate.

## Reproduce

```sh
# Reproduce the historical single-runner comparison; manual runs do not deploy.
gh workflow run ci.yaml --ref v0.1.6-rc.2 -f browser-workers=4
gh workflow run ci.yaml --ref v0.1.6-rc.2 -f browser-workers=5

# Current three-group workflow; compare one or two workers per group.
gh workflow run ci.yaml --ref main -f browser-workers=2

# Completed-run elapsed, aggregate runner, job, and step durations.
node scripts/ci-timings.mjs 34419219831 34421319030 34421520389
# October 2026 pull request baseline and the tree-keyed reuse pull request.
node scripts/ci-timings.mjs 37221466077 37241143537

# Optional experiment budget: fails for a failed run or an exceeded elapsed budget.
node scripts/ci-timings.mjs RUN_ID --budget=300
```

Run `pnpm check`, `pnpm test:e2e`, and `node tests/container-smoke.mjs IMAGE` for local
validation. Browser fixtures use temporary state and the fake Codex executable;
they do not use production credentials. Browser matrices retain the existing
viewport, navigation, editor, virtual-select, activity, fullscreen, OAuth, theme,
and persistence assertions.

The implementation follows Docker's [cache invalidation rules](https://docs.docker.com/build/cache/invalidation/)
and [manifest promotion](https://docs.docker.com/reference/cli/docker/buildx/imagetools/create/),
and Playwright's [headless-shell installation guidance](https://playwright.dev/docs/browsers).

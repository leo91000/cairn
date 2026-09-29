# CI and release performance

Measured on 2026-09-10 against the v0.1.5 workflow. Timings are observations, not
guarantees: hosted runner capacity, package mirrors, cache state, and registry
transfers vary. Elapsed time includes job scheduling; runner time sums job durations.

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
- On a release, reuse only a successful `push` run of this workflow on this
  repository's `main`, at the exact commit. Its `validated-image` artifact must
  match the repository, commit, run ID, schema, SHA-256 digest, and Codex/GitHub CLI
  versions freshly resolved for the release. PRs and manual runs cannot supply
  release proof. Missing proof or older tool versions runs full CI with a new image.
- Wait for concurrent main validation instead of starting a duplicate build.
  Completed evidence is retained for 90 days. The image wait is bounded at 40
  minutes, covering the main image job's 35-minute limit. If main is still pending
  at that deadline, fail and ask for a retry instead of starting a duplicate build.
- Build once, preserving SBOM and provenance, then smoke-test the published digest.
  PR images stay local and use no registry credentials. Promotion uses the runner's
  Buildx client without starting another BuildKit daemon.
- Give browser projects separate real applications, SQLite databases, workers, and
  production rate limiters. Keep ordered persistence journeys together; run the
  four Chromium/WebKit light/dark layout matrices independently. This removes
  deliberate waits for one shared rate limiter without weakening that limiter.
- In CI, use three runners for journeys, Chromium layouts, and WebKit layouts, with
  at most two workers per runner. Share the quality job's built frontend through
  an artifact, install only each group's browser, and retain separate evidence.
- Run TypeScript checking once through `pnpm build` inside `pnpm check`. Install
  the Chromium headless shell used by the tests, plus WebKit and their OS dependencies.
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

# Skill-bundle availability diagnostic checkpoint

Status: **slot explicitly re-ceded by requester; unresolved diagnosis**. No new diagnostic run has been dispatched yet. Independent full-diff harness review is required before dispatch. This branch is diagnostic only. Do not merge it into main.

## Scope and identities

- Only writer for the new availability case; isolated checkout `skill-availability-repair`.
- Product branch `repair/skill-availability-lifecycle` remains at main `fbd0b96e7450948e5f8b7829bdff157733291413`; no product changes or repair PR.
- PR #731 (`repair/browser-mcp-cors`, head `59d805247c872688fa59f84c77d9ffd560d9399a`) and root observer branches are untouched. The parent retains #731, #724 and global validation ownership.
- Native host: source, private trace analysis and lightweight JS only; no local heavy builds, browsers, services or system changes.

## First failure, preserved

[PR #731 failure](https://github.com/leo91000/cairn/actions/runs/42488409838/job/128755277226): unchanged command below; skill-bundle WebKit multi-file round-trip assertion `expect(errors).toEqual([])` received one access-control page error for `/api/installations`. Original artifact 13621096221 was downloaded privately; SHA256 `76e1bdf6c65dece7794f3f705fcf866fc98c22e54328fb08bec45d9523bbc1ce` verified. No trace or auth data is committed or published.

```sh
pnpm test:e2e deliverables.spec.ts skill-bundles.spec.ts beacon-sharing.spec.ts beacon-installations.spec.ts mcps.spec.ts --workers=2 --retries=0 --fail-on-flaky-tests --trace=on
```

Trace facts: reload starts at 81863.186 ms, pageError at 81877.172 ms, reload returns at 81959.778 ms. The navigation document request starts at 81872.711 ms and succeeds. The previous availability request is 78725.088 ms + 101.141 ms, status 200; later availability requests at 82027.215, 85129.592 and 88146.575 ms succeed. The failed fetch is absent from the network archive. Stack identifies `Ie` in the workspace asset at line 9, column 195059; the archive has no asset response body. These facts do **not** establish whether the failed poll started before or during departure, or whether #731 caused it.

## Controlled comparison completed

[Run 42489816063](https://github.com/leo91000/cairn/actions/runs/42489816063), attempt 1, observer `8161b0320001cfa18ecd60970b6fd6fe26ae385f`:

| Source | Tree | Job | Unchanged selected command | Resume |
| --- | --- | --- | --- | --- |
| `fbd0b96e7450948e5f8b7829bdff157733291413` | `ccfd101b0b89d36c973265b344d3864be5849324` | 128759530362 | 40 PASS | 4 PASS |
| `59d805247c872688fa59f84c77d9ffd560d9399a` | `19946a7fcffa5b9539cc3e2a427e5c3fe3fc3c40` | 128759530542 | 41 PASS, 1 applicability skip | 4 PASS |

Separate fresh disposable jobs ran **serially**, preserving the PR browser image, Rust and Playwright pins, Postgres service, fixture build, Vite diagnostics flag, saved-chat warmup and exact selected command. Checkout SHAs and trees were logged before setup. No rerun was used to erase the first failure. These two green observations are **not a diagnosis, regression clearance or global qualification**.

## Next lot prepared, not dispatched

Observer `.github/workflows/webkit-dependency-regression.yaml` now builds each immutable source serially and appends `.github/diagnostics/skill-availability-probe.ts` to the original skill suite **after** the product build. No app source is changed. The prepared command is:

```sh
pnpm test:e2e skill-bundles.spec.ts --project=layout-webkit-skill-bundles --grep 'availability native reload probe' --workers=1 --retries=0 --fail-on-flaky-tests --trace=on
```

Two controlled boundaries exercise the real skills page and native `page.reload()`:

1. A real availability request has reached its successful backend response but its delivery is held when reload starts.
2. The native reload document response is held while the browser clock crosses the existing 3-second polling interval.

The probe records fixed event labels for poll start/abort, beforeunload/pagehide/pageshow and held responses. All page errors retain the strict empty-array assertion. It uses no synthetic page error, error filtering, weakened assertion, retry or production change. The 3100 ms clock advance is the existing polling cadence trigger, not an increased test allowance or a proposed fix.

**Unexecuted and unqualified:** the prepared observer tests have not established a red-capable loop and may need harness corrections. Neither a production root cause nor an appropriate final regression seam is confirmed. Do not infer a defect solely from source similarity or missing lifecycle integration.

Next heavy-slot need: one serial base/candidate diagnostic lot, with the same pinned browser container/fixture builds, using the prepared probes instead of rerunning the original suite for green. Expected job limit 25 minutes each, test step at most 5 minutes each. The parent explicitly ceded the slot again on resumption; dispatch remains gated by the completed fresh full-diff review. Preserve any first probe failure and distinguish native page errors from harness failures. After a real red loop: diagnose, targeted product repair if confirmed, fresh independent full-diff SolHigh review rounds, exact affected qualification, ordinary guarded merge and verified post-merge CI. If #731-specific, send evidence to its sole writer instead of editing #731.

## Harness review round 1

Independent fresh SolHigh reviewer read the full three-file diff at `56a49047154aa23baeafdbec054c7215cafce5e2` against `fbd0b96e7450948e5f8b7829bdff157733291413`. Two P2 findings confirmed: raw boundary promises could prevent cleanup/journal retention when a gate was never reached; the error assertion preceded held-route draining. Corrections replace boundary waits with bounded failure-aware assertions, release gates in encompassing cleanup, preserve a rolling JSON journal before draining, and check every page error after routes settle. The journal now distinguishes host timestamps, browser timestamps and document identifiers. Browser probes remain unexecuted; a new full review is required for this corrected head.

Current main was refreshed and remains `7028fbfdd6d13e27b8f4e7db767899828d631b42`; #732 added an error filter and #727 added conversation renaming. These changes are preserved and excluded from the immutable historical diagnostic pair. Any later product repair must start from the then-current main, and current-main qualification must independently capture all native page errors before #732's filter.

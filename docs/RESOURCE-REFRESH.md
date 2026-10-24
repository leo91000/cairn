# Resource refresh: C4 experiment

This is a measurement and design decision for [#266](https://github.com/leo91000/cairn/issues/266),
against main `d01b519896c62ab3e51295eb702195975a2414ec` after F19 (#289,
`83221f4e20ecb76d833e896899fb8a2681a1b85b`) and F4 (#296). Both prerequisite
PRs were merged and their commits verified as ancestors before the experiment.
Before final validation, the branch also incorporated main
`0aa1d94a960d34f78ae44775d56d2364e0a7151a` (CI5, #310). Its changes are confined
to the exclusive-runner fixture; the measured client and backend sources are
identical to the experiment's starting main.
A later fetch observed main `99e7629eb04f2eafc77ac625607f1cbbacaaad46`
after the wait-card work in #236. A scoped source comparison confirmed that the
C4 resource callers, typed data, diagnostics, transports and backend remain
unchanged. This branch retains its measured base `0aa1d94a`; integration and
final combined-tree qualification remain with the campaign coordinator.

## Current behavior and dependencies

The old audit's “same on web” is no longer accurate. Web `api.refresh()` reads
agents, projects, tasks, skills and (owners only) MCPs concurrently: five reads
for owners, four for members. Android `CairnViewModel.refresh()` additionally
reads overview and all three coding-agent model catalogues concurrently: nine
reads for owners, eight for members. These are reads **after** the write.

F19's agent/project/task editors each call their resource-save helper and then
one global refresh. The helper retains a creation identity through uncertain
responses; its recovery PUT is not a duplicate refresh. An agent's optional
GitHub-token write is a separate operation. Skill bundle uploads, GitHub imports,
deletes, recovery reconciliation, startup and background reads are distinct
paths. No duplicate successful-save refresh was found in their synchronous save/callback
chains. This does not mean the full screens issue no overlapping reads.

The Missions page has separate periodic activity reads. From current source, web
`Tasks.vue` loads `/tasks/activity`, `/runs?limit=100` and `/tasks` on mount and
every five seconds while visible, suppressing overlap with its own activity
loader. Native `MissionsScreen` loads activity and recent runs, then calls the
global refresh, and waits five seconds before the next cycle while its lifecycle
is started. For an owner that native cycle has eleven reads (source-derived, not
a separate timed measurement here). It can overlap the editor save; its `onSaved`
callback only updates selection/filter state. The browser `saved` callbacks also
only update selection/close state. The isolated seam below excludes these polls,
so it must not be reported as all traffic during a tap-to-close screen journey.
A blanket in-flight dedupe could accidentally reuse a pre-write poll result.

| Mutation | State and dependent consumers that a future read set must preserve |
| --- | --- |
| Agent | Agents list and profile/access choices; task editor project/skill choices derive from the selected agent's policy. Creating/deleting an agent changes overview's agent count. Switching provider changes which model catalogue the editor needs; a name edit does not itself change provider models. Dedicated GitHub-token state is a separate request. |
| Project | Projects and task project choices; available skills include project-scoped skills discovered through project paths. Create/delete changes overview's project count. GitHub import also acquires a filesystem path/source identity that the editor must keep on refresh failure. |
| Task | Task definitions, schedule/archival state and overview's embedded tasks. Mission activity/runs use separate subscriptions/queries. A name-only edit does not modify agent, project, skill or MCP definitions. |
| Skill | Global/project skill list and selected/allowed skills in agent/task editors; bundle revision, files and source attachment remain owned by the workbench's explicit reads. |
| MCP | MCP list/tool descriptions and agent policy choices; plugin/account/node changes have additional explicit loaders and are not covered by a resource-name-only rule. |
| Coding-agent account/catalogue | Model availability can change independently of any resource save. Android global refresh currently provides a freshness opportunity; dropping it needs an explicit freshness policy. Web model pickers load catalogues separately. |

Source anchors: `apps/web/src/api.ts`, `views/Resources.vue`,
`components/TaskEditor.vue`, `components/SkillWorkbench.vue`, `resourceSave.ts`;
Android `data/CairnViewModel.kt`, `ui/ResourcesScreen.kt`, `ui/TasksScreen.kt`,
`ui/MissionsScreen.kt`, `ui/Components.kt`; web `views/Tasks.vue` and
`components/FilMission.vue`;
`ui/ResourceSave.kt`, `ui/SkillWorkbenchModel.kt`; backend `api.rs` overview and
record routes, `service.rs` agent skills/removal, `models.rs`, `claude.rs` and
`opencode/mod.rs` catalogues. Paths without a prefix are relative to the relevant
client source directory or `crates/installation/src`.

A refresh is also a reconciliation boundary. Any future narrower strategy must
preserve F4's connection/scope transition cleanup, authenticated-state guards,
web account/installation/CSRF checks, native connection identity checks, logout,
role changes, uncertain mutation reconciliation and the existing visible error
semantics. Sharing an in-flight refresh across a write is unsafe unless it proves
the shared read began after that write and belongs to the same scope generation.

## Experiment

The opt-in browser harness imports the shipped `useResourceSave`, `api` and
`refresh`; it does not reimplement their global request graph. Its only alternative
is an experimental task-name-edit read set (`/tasks`). The native instrumented
measurement imports `ResourceSave`, calls the shipped view-model refresh, and
compares a task-name-edit read set (`/tasks` plus `/overview`). Neither alternative
is connected to product screens. They establish a possible lower cost, not a
complete invalidation algorithm.

Each sample makes an actual PUT changing a synthetic name, then awaits the reads.
Browser agent edits use a minimal name update; project edits include path/source
mode and task edits use the fixture task record. Native edits serialize the
typed editor forms and decode the task response as the current caller does.
The browser checks publication into workspace state and Vue's next tick; Android
starts on the main dispatcher like the editors and measures suspension completion,
with decoding and global state publication. Performance records the thread origin.
The native prototype awaits and checks decoded tasks and overview; it does not
publish them through a new view-model API. Neither time is tap-to-paint latency.
Editor opening, model-picker preloads, live feeds, files, recovery retries,
creation and token writes are outside this timed seam.

There are 21 sequential samples per case: the first is retained separately, then
20 warm samples. Trials are paced by a two-second delay **outside** the timed
window: the installation HTTP API allows 300 authenticated reads per minute
(`http.rs::general_limit`), and nine reads every two seconds stays below that
limit with room for setup requests. The limit is preserved. This pacing is a
workload control, not a user latency budget or injected network delay. “First” is not a cold-cache claim: startup and fixture seeding
already touched the services. Cases run in a fixed order (relay before direct, global before prototype), without randomized pairing
or a statistical significance claim. No network delay, loss or artificial
per-request latency is injected. A small fixture is intentionally insufficient to predict a
large installation. No production calls, accounts or data are used. The control plane/relay use HTTP
on loopback rather than production HTTPS; Android adds the emulator gateway.
Each run uses a fresh installation store and synthetic account; the disposable
Beacon PostgreSQL database is reused across setup attempts and the clients.
Direct traffic uses authenticated WebRTC/DTLS. Negotiation, initial fallback and
route loss are outside the timed samples. An emulator is not a physical phone,
and concurrent reads mean request-count savings need not scale with elapsed time.

## Browser results

The completed run contains 168 actual PUT saves and 840 delivered client application
requests, on two agents, one local project, one disabled task, no skills and no MCPs.
Each row has one separately retained first sample and 20 warm samples. All values
below are milliseconds; p95 is the nearest-rank 19th of 20 sorted warm values.
Write and refresh medians are computed separately, so they need not add to the
median total. The refresh stage includes state publication and the next tick.

| Route | Edit | Read set | Requests/save | Median write | Median refresh | Median total | p95 total |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: |
| relay | agents | global | 6 | 103.30 | 249.40 | 372.10 | 681.40 |
| relay | projects | global | 6 | 97.25 | 207.25 | 305.50 | 493.70 |
| relay | tasks | global | 6 | 337.55 | 217.20 | 582.05 | 998.20 |
| relay | tasks | task-only-prototype | 2 | 189.00 | 68.80 | 310.85 | 674.30 |
| direct | agents | global | 6 | 22.00 | 55.45 | 78.45 | 149.60 |
| direct | projects | global | 6 | 48.25 | 72.70 | 120.45 | 190.50 |
| direct | tasks | global | 6 | 107.30 | 59.50 | 174.65 | 383.50 |
| direct | tasks | task-only-prototype | 2 | 126.00 | 15.50 | 165.35 | 457.30 |

Raw samples: [browser JSON](measurements/resource-refresh-web.json). These counts
cover client application requests, excluding signaling, frame fragments and
installation-internal requests. Every sample checked the delivered route, exact
request count and published saved name. The run used Node 24.21.0, Chromium
153.0.8010.12 and the ordinary unoptimized Rust debug binaries. Host snapshots
record about 19 GiB visible RAM, 7 logical CPUs and about 16 GiB free disk at
sampling. The VM shares node resources; no exclusive reservation was taken.
Compilers were stopped. Routine VM storage/publication activity was not isolated.

The task prototype removed four reads. Its relay median total fell in this run,
but its direct p95 total increased. Write medians also changed between blocks:
these sequential small samples cannot attribute the total change solely to read
invalidation. They measure observable cost on this VM, not production network RTT
or a user budget. The reduction is a useful candidate, not proof of negligible
global cost or a universally faster replacement.

Metadata-only cleanups after collection renamed the os.freemem field to
`freeBytes` (not Linux MemAvailable), clarified the reused Beacon database, and
removed an unused setup probe. The timed
save/read code and all measured numbers are unchanged; executed harness hashes
and snapshots accompany the private evidence.
The controller subsequently received explicit result-array types to satisfy
`vue-tsc`; the pinned TypeScript compiler emitted identical JavaScript before
and after those annotations.

## Android results

The actual API 36 AOSP x86_64 emulator completed the opt-in test on KVM:
126 PUT saves, six cases and 966 measured application requests. Each case retains
one first sample and 20 warm samples, using the same statistics as the browser
table. The selected route matched the last delivered application route after
every sample; this is coarser evidence than the browser's per-request trace.
All stored sample windows report zero background API completions. Setup,
signaling and traffic between samples remain outside these request counts.

The fixture contained two agents, no projects, one disabled task, no skills and
no MCPs. Its end-of-run catalogue snapshot was Codex: three models, not stale;
Claude: three models, stale; OpenCode: zero models, not stale. These are fixture
providers without live provider accounts. Catalogue freshness was not audited
per sample, so these timings do not characterize consistently healthy live
provider refreshes.

Values are milliseconds. Global reads include view-model publication and saved
value checks; the prototype checks decoded tasks and overview without publishing
them through a view-model API. Its timings therefore describe a read-set
experiment rather than an equivalent completed UI implementation.

| Selected/last route | Edit | Read set | Requests/save | Median write | Median reads/checks | Median total | p95 total |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: |
| relay | agents | global | 10 | 163.37 | 470.49 | 609.98 | 896.42 |
| relay | tasks | global | 10 | 246.17 | 441.01 | 763.92 | 1009.71 |
| relay | tasks | task-only-prototype | 3 | 252.79 | 149.02 | 394.33 | 686.35 |
| direct | agents | global | 10 | 110.66 | 373.49 | 497.20 | 751.58 |
| direct | tasks | global | 10 | 485.33 | 358.36 | 871.81 | 1385.95 |
| direct | tasks | task-only-prototype | 3 | 211.22 | 113.33 | 336.78 | 688.89 |

Raw samples: [Android JSON](measurements/resource-refresh-android.json). The run
used the ordinary debug APK and Rust binaries, with compilers and Gradle stopped.
The host had seven logical CPUs, about 19 GiB visible RAM, 0.89 GiB Linux
MemAvailable and 12.34 GiB free disk immediately before sampling. The emulator
used two actual KVM vCPUs; the VM was not exclusively reserved. Different
payloads, runtimes and host headroom prevent cross-client performance rankings.
Only existing-agent and existing-task name edits were measured on Android.

The task prototype removed seven reads. Its observed median and p95 totals were
lower here, but the direct global task block also had much slower writes, and the
prototype omits view-model publication. These blocks do not isolate a causal
invalidation benefit. Both strategies still await the write before a concurrent
read phase; fewer requests do not remove that sequential phase.

## Decision

Retain the existing global refresh in product code for this measurement PR. This
is provisional: the issue’s budget-dependent stop condition cannot be judged
without a user objective. The observed read-set savings remain a candidate for
further implementation, not a claim that the existing cost is acceptable. F19 already supplies one refresh per
successful save, so there is no demonstrated duplicate to remove here. A narrower
task-name read set can avoid reads, but it does not establish a safe shared
invalidation policy for the dependency matrix above. No user latency budget was
defined; this experiment cannot conclude that current performance is within one.

Reopen implementation with a stated user workload and latency objective, measured
production-like direct/relay RTT and loss, representative healthy/empty/stale
catalogue and account states and larger
resource lists, creation/delete/permission scenarios, then behavior regressions
for the proposed read sets and transitions on both clients. Keep explicit
catalogue freshness and workbench loaders. Do not adopt a 300 ms threshold or
extrapolate localhost timings into a production performance guarantee.

## Reproduce

Use the pinned mise/pnpm/Rust/JDK/Gradle tools and a disposable PostgreSQL database
in `CAIRN_BEACON_TEST_DATABASE_URL`. Build the ordinary debug installation and
Beacon binaries with at most two Cargo jobs; install Playwright Chromium. Run:

```sh
cargo build --locked --bin cairn --bin cairn-beacon -j 2
pnpm exec playwright install chromium
pnpm build
```

The launcher strips live agent/provider credential variables **before** importing
fixtures. The fixture creates a real local Beacon and installation, with fake
mail delivery only, an owned temporary data directory and fixture coding-agent
executables. Relay is selected by refusing direct authorization in the browser;
direct is real authenticated WebRTC on the VM’s non-loopback interface, using
a fixture STUN listener in the same VM. Loopback direct aliases are rejected by
the unchanged destination policy. Each timed sample verifies successful application delivery on its selected route;
each completed case is checkpointed atomically, with partial/complete status. Every browser sample asserts the successful
application transport observations' route and request count. Signaling is not
counted as an application request or direct-delivery proof.

For Android, build debug and instrumentation APKs with the project wrapper. Start
an API 36 AOSP emulator after stopping compilers and Gradle daemons. Launch
`scripts/android-direct-fixture.mjs` with `node --import tsx`, a private config
output path and `CODEX_BIN`, `CLAUDE_BIN`, `OPENCODE_BIN` pointing to the existing
fixture executables. Use `adb reverse tcp:4398 tcp:4398`; copy that private JSON
as the app's `files/direct-fixture.json`. Run only
`build.cairn.app.data.RefreshMeasurementTest` with instrumentation argument
`cairnRefreshMeasurement=true`, then extract `files/refresh-measurements.json`.
Do not publish the private fixture configuration: it contains synthetic session
credentials. Native reads/writes/JSON decoding use the existing `Performance`
instrumentation; raw timing summaries retain histogram upper bounds, which must
not be described as exact per-request p95s. The native transport flow reports the
route of delivered application traffic; the experiment checks it after every
sample. The browser records each successful request separately.
Native snapshots do not identify every individual request's delivered route;
the last-delivery observation alone cannot rule out a mixed-route sample.
Native application-request counts use Performance's main-thread-origin counts:
every resource call in the sampled seam starts on Main, while native direct
authorization/renewal runs on IO. `backgroundApiRequests` reports other API
operations completed in the window, and the complete raw snapshots retain them.
This prevents control-plane renewal from being counted as a resource save/read.
For the emulator fixture, explicitly use local STUN settings:
`CAIRN_BEACON_STUN_LISTEN=0.0.0.0:3478`,
`CAIRN_BEACON_STUN_URL=stun:10.0.2.2:3478` and
`CAIRN_DIRECT_STUN_URLS=stun:127.0.0.1:3478` on the installation. The existing
fixture advertises the emulator gateway `10.0.2.2`; inherited external STUN
settings must not replace these fixture settings.

The native test is deliberately opt-in when discovered by ordinary device
suites. It has no fixture credentials unless the measurement is explicitly
selected. Stop both fixtures, the emulator and owned database afterward. Keep
measurements separate from results of ordinary journey/quality suites.

## Validation limits

The local `pnpm check` passed lint, then its unit stage failed: 584 tests passed
and seven failed across three unchanged legacy-fixture files. The failures cover
chat restart (15-second test timeout), account exhaustion (the wait reason did
not become a string), manager SIGTERM/SIGKILL recovery (the expected fixture file
was not observed), missing history and expired-budget recovery (runs remained
running instead of failing), and incomplete-workspace recovery (the run remained
running instead of succeeding). The configured Vitest globs exclude these
measurement runners; the failing tests and their legacy implementation are
unchanged. Their cause is not established. This full check is not green.
A focused diagnostic of those three files with one worker and unchanged
timeouts also failed: 42 tests passed, three failed (account exhaustion, manager
SIGTERM recovery and incomplete-workspace recovery). Reduced concurrency did
not resolve them; this diagnostic does not replace the failed full suite.
The independent production build initially found an implicit array type in this
new controller; explicit types fixed it, and `pnpm build` passed afterward.

The local Android full unit run completed 484 tests with one failure:
`DirectTransportTest.invalid finite response closes direct and safe read recovers on relay`,
at line 530 (`assertTrue(disposed)`), after its relay GET assertion passed. The
test and production Android sources match the starting main; this PR's new
instrumentation test is outside that unit-test classpath. The cause is not
established, and this full run is not green. An earlier two-worker attempt was
stopped when available RAM fell below 200 MiB; the completed run used one worker,
without changing the pinned heap, toolchain or test deadlines. Full reports and
host snapshots accompany the private evidence. Device measurements are separate
from that unit-suite result.

The first emulator boot did not finish within the helper's existing 600-second
window and was stopped. A diagnostic start with the same image, configuration
and deadline recorded guest initialization progress and completed boot. Sampling
began only after `sys.boot_completed=1`, API/ABI verification and actual KVM
descriptor checks. The device measurement then reported `OK (1 test)`; the raw
126-sample file and its request counts were validated separately. This is an
emulator API-seam measurement, not a physical-phone or full-screen journey test.

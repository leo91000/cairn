# Android test startup and Compose readiness (#175)

[Issue #175](https://github.com/leo91000/cairn/issues/175) records three failures
on the same release tree: a missing editable field in `SkillMention`, a timeout
after a swipe in `ChatSwipe`, and activity recreation / ANRs during emulator boot
in `HistoryFollowDeviceTest.compactConversationAndKeyboard`.

## Reproductions and causes

The first-attempt artifacts of runs 38118926078 and 38124510363 locate the
failures more precisely than their job summaries:

| Journey | Exact failure | Missing synchronization |
| --- | --- | --- |
| `emptySkillsExplainsTheMissingSuggestionsAndCanBeDismissed` | First `performTextInput`: no node with `SetText` | The placeholder and history already exist while installation refresh still sets `Workspace.busy`. The disabled field deliberately has no edit action. |
| `notificationReopensItsRequestedChatAfterSwipingAwayFromIt` | `waitForChat` immediately after the first swipe, before the notification | History has scrolled, but `/tasks/activity` has not yet supplied the Fil's neighbors. The pager still has one page and ignores the swipe. |

The original methods passed 25 times each with immediate localhost responses.
Adding deterministic dispatch latency raised the reproduction rate: for seeds
0–24, delay each request by
`Random(seed xor path.hashCode()).nextLong(0, 300)` milliseconds. In one JVM,
50 original journeys produced 3 swipe timeouts, 5 missing-edit-action assertions,
and 5 late connection EOF errors during fixture teardown. These are diagnostic
latencies on a local fixture, not production network measurements.

Minimization then isolated `/skills` (261 ms, seed 3) and `/tasks/activity`
(275 ms, seed 6). The semantic tree showed the disabled composer in the first
case and the original selected page in the second. A controlled probe with
750 ms latency gave these results:

| Probe | Result |
| --- | --- |
| Swipe after `connect` completes (`busy=false`), pager still one page | Timeout |
| Same swipe after all three pager pages exist | Pass |
| Type after placeholder/history appear, refresh still pending | Missing edit action, sometimes obscured by the resulting teardown EOF |
| Same typing after enabled `SetText` exists | Pass |

The correction waits for the actual control used by each journey: an enabled
editable field, and the expected pager page count. It preserves history, draft,
Back, notification routing, and end-of-pager assertions. No animation setting,
Compose clock policy, timeout increase, or test retry is used.

The fixture also owns a `ViewModelStore` and unmounts its composition before
clearing the model and closing MockWebServer. This cancels UI-owned connection
work and ViewModel jobs before the next fixture. Previously a directly
constructed ViewModel was never cleared; closing only its installation streams
left its account connection and scope alive. This cleanup addresses isolation;
it is not substituted for the two explicit readiness conditions.

## Permanent regression commands

From `apps/android`, using the pinned JDK and Gradle wrapper:

```sh
mise exec -- ./gradlew testDebugUnitTest --rerun \
  --tests '*SkillMentionTest' --tests '*ChatSwipeTest'
```

Both suites now include a slow-response variant of the complete original
journey. To check the sensitivity of those variants, a controlled mutation
removed only the two new readiness predicates, retaining the new fixture cleanup
and every scenario/assertion. Both variants failed with the original symptoms;
restoring the predicates made the suites pass. The mutation diff and XML evidence
are retained with the investigation artifacts. Temporary seeded subclasses and
`[DEBUG-175]` probes are not part of the delivered tests.

## Device startup

The issue's [device comment](https://github.com/leo91000/cairn/issues/175#issuecomment-6112634359)
reports 8/8 passing trials after stabilization on both main and the investigated
branch. Boot completion alone was insufficient. Local observations likewise
saw `sys.boot_completed=1` at load 6.17, falling to 2.20 over the next minute on a
two-core API 36 emulator. Load average decays slowly, so the readiness gate uses
CPU counter deltas instead of waiting for that moving average.

After installing both APKs, `android-device-tests.mjs run` now waits for ten
continuous seconds of:

- Boot complete, boot animation stopped (or absent for `-no-boot-anim`).
- Responding ActivityManager, unchanged device configuration and stable
  `system_server` / System UI PIDs.
- No `dex2oat`, `odrefresh`, or `profman` process.
- At most 35% aggregate busy CPU, including I/O wait.

It samples every two seconds, uses a monotonic clock and a three-minute deadline,
and restarts the quiet window after any failed condition or ADB error. It never
starts instrumentation after a timeout. `readiness.json` records every observation
and is included in the existing device artifacts on success and failure.
The persistent `artd` service is allowed to be idle; waiting for that daemon to
exit would never establish readiness. See the [ART Service documentation](https://source.android.com/docs/core/runtime/configure/art-service).

The existing parallel APK build, early emulator boot, shard balancing, four-core
emulators, enabled animations, PR twin selection and publication prerequisites
remain in place. No test failure is made eligible for publication.

## Fixed-count device measurement

A manual Android workflow run can set `stability-repetitions` to 1–20. It still
runs the complete normal validation. Shard 1 then executes the three original
issue journeys together for that many trials. A failed, skipped, missing or
crashed test fails the measurement immediately; there is no retry-until-green.

```sh
gh workflow run android.yaml --repo leo91000/cairn --ref <branch> \
  -f stability-repetitions=10
```

With APKs already installed on a prepared emulator, the same measurement is:

```sh
node scripts/android-compose-stability.mjs 10 device-results/stability
```

Each trial keeps raw instrumentation output and JUnit XML; `stability.json`
records the requested and completed counts, all results and timings. This file
is separate from shard `results.json`, so repeated journeys do not alter shard
coverage or timing estimates. Zero (the dispatch default) adds no measurement;
normal CI always retains its full selected shard.

## Host conditions and limits

The initial local run used Linux 6.12.109, a Xeon VM with 12 visible CPUs and
30 GiB RAM shared with the other missions, API 36 AOSP, and nested KVM. During
contention, available memory fell to about 2.5 GiB. Both a JVM thread and a QEMU
vCPU became stuck in `kvm_async_pf_task_wait_schedule`; QEMU's watchdog stopped
its guest, and ADB went offline. An untouched full suite then had two unrelated
`SkillWorkbench` failures (346 tests, two failures). Those are recorded as
contaminated baseline observations, not device evidence or product fixes.
The node later became unavailable and this run was recovered on another node.

Robolectric and replayed readiness samples are not Android device validation.
Actual instrumentation trials and exact CI commit/run links belong to the PR's
validation evidence. The quiet-window gate reduces observed boot interference;
it cannot guarantee that Android never changes configuration later.

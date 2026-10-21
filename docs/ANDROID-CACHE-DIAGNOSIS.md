# Android cache persistence and responsive live updates

The Android live reader publishes received conversation snapshots without waiting
for an earlier history-cache save. Persistence uses one worker and one latest
pending snapshot per reader, rather than launching a task for every update.

## Correctness and lifecycle

`HistoryCacheWriter` captures the snapshot and cache generation at publication.
It saves sequentially and conflates pending updates. Foreground exit flushes the
last published revision; closing or cancelling a reader drains its last pending
snapshot and flushes disk in a non-cancellable cleanup.
The latest uncompleted write is retained independently of channel delivery, so
cancellation after a channel handoff but before worker resumption cannot lose it.

The cache assigns each offered snapshot an identity shared across readers of the
same key. Superseded requests cannot overwrite later offers. Older cursors cannot
replace newer output in the same history, while a different history can start at
its own cursor. Cache clearing and terminal removal invalidate queued requests.
Reads also reject queued snapshots older than the retained cursor, so reopening
a reader before that stale write runs cannot regress memory or disk.
Automatic cleanup of missing, expired, corrupt or oversized stored snapshots
preserves newer pending requests. Explicit removal and clearing invalidate queued
work; a read that rejects an invalid queued request discards only that identity.

A reader reopening before persistence finishes can restore the queued snapshot.
Reading positions are retained across queued saves and checked against their
history and first event. Offering, reading, persisting and recording positions use
the same cursor ordering rule; an older queued boundary cannot discard a position
for the retained newer snapshot. The cache's JSON version, encryption, expiry, byte
budgets, disk-write throttle and backwards history boundaries are unchanged.

Memory entries retain their estimated size. A warm read moves the entry in the
LRU without stringifying every retained history. Updating an entry recomputes its
own size; unchanged neighbors retain theirs. `recentHistory` still enforces the
existing contiguous byte budget before a snapshot is retained.

The cache still serializes filesystem operations with its existing mutex. A cold
read or explicit flush can wait for an active disk operation. The fix removes
ordinary live publication from that wait and avoids repeated full-cache size
accounting; it does not promise that all Android cache operations are immediate.

## Regression and original reproduction

`CacheRefreshCases` exercises the real Compose reader, `CairnApi.live` SSE parser
and cache on both the JVM and an Android device. A synthetic server sends two
batches, with the first cache write held in its seal callback. `LIVE_REDUCE`
confirms the second batch has already been decoded upstream. The second output
must display while the write remains held, and stay visible through a controlled
300 ms hold. This assertion failed before the fix and now runs in normal CI.

`HistoryCacheWriterTest` covers coalescing a burst to its last snapshot, final
persistence after cancellation, overlapping readers, queued restoration, stale
cursors, generation invalidation, terminal removal and reading positions.
`HistoryCacheTest` checks that warm reads and neighboring saves do not stringify
unchanged payloads again, alongside the existing disk, expiry and budget tests.

The historical investigation at reference
`4b9558e9d90114a663e14562e994df032e96db3c`, after PRs #316, #320 and #325,
showed a received update remaining hidden during a deliberately held 306–309 ms
interval, then appearing 5–8 ms after release. The [reference runs](measurements/android-cache-diagnosis/main-final.json),
[expected failure](measurements/android-cache-diagnosis/expected-failure.json)
and [temporary detached experiment](measurements/android-cache-diagnosis/detached-prototype.json)
remain recorded. The detached experiment was removed; the product implementation
uses a bounded writer with explicit invalidation and cleanup.

## Measurements and reproduction

With the Android SDK configured, from the repository root:

```sh
node scripts/measure-android-cache.mjs /var/tmp/cairn-cache-measurement.json
```

Set `ANDROID_HOME` if needed. The launcher enables the two opt-in cost/contention
cases and runs the normal responsive-delivery regression. It records source
hashes, CPU, host/workload conditions, encoded/disk bytes and actual entry counts.
The [fixed run](measurements/android-cache-diagnosis/fixed.json) can be compared
with the [first](measurements/android-cache-diagnosis/main.json) and
[second reference runs](measurements/android-cache-diagnosis/main-final.json).

For the same 1,250-event, twelve-history workload (714,319 encoded bytes per
history), the recorded reference runs and final fixed run show:

| Operation, p50 | Reference runs | Fixed run |
| --- | ---: | ---: |
| Warm read | 8.22–12.55 ms | 0.14 ms |
| Save, normally below the disk-write interval | 9.08–13.63 ms | 1.66 ms |
| Forced write | 9.25–12.70 ms | 3.99 ms |

The final delivery regression publishes the already received second snapshot in
about 20 ms while the earlier write remains held for over 300 ms. These are
fixture observations, not a device-wide latency budget or a production forecast.

The fixture measures 200, 1,250 and 4,000 events, with one or twelve seeded
histories and eight measured iterations per case in fixed order. Larger cases
reach the existing budgets and retain fewer than twelve histories. A cold read
uses a new cache object, with the host filesystem page cache still warm. Identity
seal/unseal callbacks exclude Android Keystore costs. These shared-host JVM
measurements do not establish production network latency or affected-phone FPS.
`CACHE_SAVE` and `CACHE_READ` include mutex waiting, not only filesystem time.

To run the device regressions:

```sh
cd apps/android
./gradlew connectedDebugAndroidTest \
  -Pandroid.testInstrumentationRunnerArguments.class=build.cairn.app.ui.CacheRefreshDeviceTest,build.cairn.app.ui.SavedConversationDeviceTest
```

The original [findings](measurements/android-cache-diagnosis/findings.json) and
[baseline validation](measurements/android-cache-diagnosis/validation.json) remain
historical evidence. They do not replace validation of the final fix. No claim is
made that every observed phone delay came from cache persistence.

[Fix validation](measurements/android-cache-diagnosis/fix-validation.json)
records 587 unit tests with no failures and two skipped opt-in benchmarks, eleven
passing focused cases after the shared fixture update, and four passing native
emulator cases. Android lint and Kotlin formatting pass.

The review follow-up in the same validation record covers the stale queued-read
interleaving: its regression fails with cursor 9 instead of 10 before the guard,
then passes after it. The full local rerun has 588 cases and the same existing
`NodePlacementTest` timeout recorded in the baseline; that case and the twelve
cache/delivery cases pass together in isolation. A [fresh measurement](measurements/android-cache-diagnosis/review-fixed.json)
records the follow-up source hashes and workload results separately from the
original fixed run above.

A second regression holds an expired disk read while a valid snapshot is offered.
Before the cleanup fix, flushing loses that snapshot and the test reads null
instead of cursor 2. After the fix, all fourteen focused cases pass, including
explicit removal and clearing tests. Formatting and Android lint also pass.

A third regression checks a queued cursor 9 starting at event 2 against a retained
cursor 10 starting at event 1. Before the position fix, the retained snapshot's
position is silently dropped; afterward, all fifteen focused cases pass. The
fresh measurement and validation record cover the shared cursor comparison.

A controlled dispatcher regression cancels an idle writer after its channel
accepts a snapshot but before its continuation resumes. Before the recovery fix,
a fresh cache reads cursor 1 instead of the accepted cursor 2. Afterward, all
sixteen focused cases pass, including both cancellation scenarios. The pending
write reference is cleared once that revision is processed.

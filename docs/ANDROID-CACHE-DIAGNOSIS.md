# Android cache and delayed live updates

An Android reader can receive a new conversation snapshot and still display the
previous one while a cache save finishes. This investigation reproduces that
dependency on `main` at `4b9558e9d90114a663e14562e994df032e96db3c`, after
PRs #316, #320 and #325. It adds opt-in diagnostics and recorded evidence; it does
not change the reader or cache implementation.

## What the fixtures establish

`CacheRefreshDiagnosisTest` exercises the real Compose `rememberLive` reader,
`CairnApi.live` SSE parser and `HistoryCache`. A synthetic server sends two
batches. The cache's seal callback holds the first write while the second batch
arrives. `LIVE_REDUCE` confirms that both batches have already been decoded and
applied upstream before checking the visible text.

- On the reference, the second update remains hidden during a deliberately held
  306–309 ms interval and appears 5–8 ms after the cache write is released.
- Requiring the second update to display before release fails on the reference.
- A temporary experiment that only launches the cache save separately passes
  the same responsiveness assertion: the second update displays while the write
  is still held. Its patch is preserved as evidence, but removed from product
  code. Unbounded launches and late snapshot capture are not a safe final writer.

The first update is published before its own save. The demonstrated wait affects
a subsequent update, or a cache read queued behind another write. The UI thread
is suspended rather than blocked during IO, so the interface can remain usable
while the displayed conversation falls behind.

`HistoryCacheCostDiagnosisTest` also establishes that a writer holds the shared
mutex against a warm read of another conversation. `CACHE_SAVE` and `CACHE_READ`
start their timers before taking this mutex: their durations include lock waiting
and must not be reported as pure disk latency.

## Repeated cache accounting

Every successful cache read and save calls `trimMemory`, which traverses all
retained histories and stringifies their JSON payloads to estimate memory use.
`recentHistory` also serializes each retained event before checking whether a
disk write is due. The one-second write throttle does not avoid this preparation.

With 1,250 synthetic events and 714,319 encoded bytes per history, two runs gave:

| Workload | One retained history | Twelve retained histories |
| --- | ---: | ---: |
| Warm read p50 | 0.88–1.06 ms | 8.22–12.55 ms |
| Save p50, normally below the disk-write interval | 1.84–2.15 ms | 9.08–13.63 ms |
| Forced write p50 | 2.66–3.64 ms | 9.25–12.70 ms |

These are local JVM/Robolectric measurements on a shared Linux i9-14900K host
with 32 logical CPUs, approximately 94 GiB RAM, Java 21.0.12.1 and a 512 MiB test
heap. The fixture uses identity seal/unseal callbacks, excluding Android Keystore
and real device encryption costs. A cold read uses a new cache object, while the
host filesystem page cache remains warm. Each case measures eight iterations in
fixed order after seeding; JIT, GC and other host work limit timing comparisons.
The reports include encoded/disk bytes, actual retained entry counts and CPU time
for `recentHistory`, across 200, 1,250 and 4,000 events. Larger cases reach the
existing cache budgets and retain fewer than twelve histories.

## Reproduce

With the Android SDK configured, from the repository root:

```sh
node scripts/measure-android-cache.mjs /var/tmp/cairn-cache-measurement.json
```

Set `ANDROID_HOME` if the SDK is not otherwise configured. The launcher enables
`CAIRN_MEASURE_ANDROID_CACHE=1` and reruns the three diagnostic cases. Normal unit
test runs skip them, so they do not lock the observed slow behavior into CI.

To require responsive delivery while a cache write remains held:

```sh
CAIRN_CACHE_EXPECT_UNBLOCKED=1 node scripts/measure-android-cache.mjs
```

This command is expected to fail on the reference implementation. It becomes a
regression assertion for a future bounded writer. It does not apply the recorded
prototype or change product code.

## Evidence and next change

The [findings](measurements/android-cache-diagnosis/findings.json) separate
demonstrated behavior from remaining uncertainties. The directory also retains
[first](measurements/android-cache-diagnosis/main.json) and
[second reference runs](measurements/android-cache-diagnosis/main-final.json), the
[expected failure](measurements/android-cache-diagnosis/expected-failure.json),
the [detached experiment](measurements/android-cache-diagnosis/detached-prototype.json)
and [validation](measurements/android-cache-diagnosis/validation.json).

The three opt-in diagnostics pass. Formatting and Android lint pass. The full
Android unit run completed 581 tests with one existing `NodePlacementTest`
timeout and three skipped diagnostics; that existing test passed alone. The
cause of the full-suite timeout is not established.

A production fix should remove persistence from the live delivery path with a
bounded, coalescing writer, preserve generation invalidation, reading positions,
cursor ordering and final flush, and retain per-entry size accounting. This
investigation does not establish how much of the affected phone's latency comes
from this mechanism, nor a production speedup or phone FPS improvement.

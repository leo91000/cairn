# Conversation history cache

Web (IndexedDB) and Android (encrypted atomic files) retain loaded history pages
within the existing byte budget instead of discarding everything except the last
200 events. Reopening displays saved output immediately and resumes the stream
from its validated cursor. The web client skips its initial full HTTP history read
when a saved snapshot exists. Revalidation preserves the earliest loaded page;
a history reset replaces that boundary.

The cache remains session-scoped, expires after seven days, and holds at most
12 conversations / 20 MiB. The history suffix has a 3 MiB budget including state
and headroom; web accounts conservatively in UTF-16 bytes, Android in UTF-8 bytes.
An evicted prefix remains available from server pagination. Evicting a prefix also
invalidates its reading position. Logout and existing revision/deletion handling
still invalidate saved conversations.

Activity groups contain at most 30 folded calls, partitioned by stable event-ID
windows so older pages do not repartition newer groups. Sparse event IDs can yield
smaller groups. Updates to one call still replace its previous state. Consecutive successful reads remain grouped within that limit;
failures, running calls, messages and subagent teams retain their presentation.

## Reproduce the web measurement

```sh
pnpm exec playwright install chromium
node scripts/measure-history-cache.mjs d23a5a52eb76405a4f4327dc61f231cd10305fe0
```

Measured locally on 2026-10-14: Linux x86-64, Intel Xeon, 12 logical CPUs,
31 GiB RAM, Node 24.21.0, Chromium 153.0.8010.12. Shared development VM with
concurrent compilation; these are illustrative local measurements, not production
network latency or performance thresholds.

Workload: 1,250 events with 20 synthetic output lines each; 30 writes, 30 warm
reads and 30 cold reads per version. Cold means a fresh JavaScript module with an
empty memory cache; browser and OS filesystem caches remain warm. Encoded bytes
are UTF-8 JSON payload bytes, not physical IndexedDB disk usage. CPU is the sum of
renderer process CPU time across the measurement, including module imports/setup.

| Version | Retained events | Encoded bytes | Write median | Warm read median | Cold read median | Renderer CPU |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Reference (200-event window) | 200 | 115,769 | 1.2 ms | 0.2 ms | 2.1 ms | 190 ms |
| Loaded pages within byte budget | 1,250 | 719,346 | 4.1 ms | 0.9 ms | 4.4 ms | 340 ms |

The larger record intentionally costs more local serialization and storage while
avoiding downloads of previously read pages. Browser tests assert zero repeated
history/event reads after reopening the loaded pages, including a blocked stream.

## Reproduce the Android measurement

From `apps/android/`:

```sh
mise exec -- ./gradlew testDebugUnitTest --tests build.cairn.app.data.HistoryCacheMeasurementTest --rerun-tasks --no-daemon
```

`HistoryCacheMeasurementTest` compares the same 1,250-event workload with the
previous 200-event suffix, using 5 warmup writes and 30 writes/warm reads/cold
reads. Each cold read opens a new `HistoryCache` against the saved atomic file;
OS filesystem caches remain warm. Its `HISTORY_CACHE` output records encoded file
bytes, latency medians and process CPU. This is a local Robolectric/JVM measurement
with identity sealing, excluding Android Keystore encryption and device/network
latency. The encrypted production adapter and device reopening behavior are
validated separately by the Android tests.

Measured on the same shared Linux/Xeon/12-CPU/31-GiB host, with JDK
21.0.12+101 LTS, Robolectric 4.17, SDK 35 and concurrent compilation. The process
CPU includes all test-worker threads during the three measured phases. JVM/JIT
warmup and OS caching affect these small timings; the larger record is not a
claim of faster local decoding.

| Version | Retained events | Encoded bytes | Write median | Warm read median | Cold read median | Process CPU |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Reference (200-event suffix) | 200 | 111,346 | 0.687 ms | 0.120 ms | 1.984 ms | 220 ms |
| Loaded pages within byte budget | 1,250 | 691,823 | 2.695 ms | 0.069 ms | 1.596 ms | 500 ms |

## Android subagent run identities

Android now retains each event's `runId`, so subagent items from separate runs
remain distinct. The JSON field is additive, the cache version and encryption
remain unchanged, and older clients ignore the field. New clients decode old
records but do not restore a snapshot containing events without run identities:
they start the stream at cursor zero and reload a recent server snapshot. This
one-time refresh also discards the legacy local reading position and loaded-page
boundary; older pages remain available through server pagination. Installation
data formats and their compatibility epoch are unchanged.

The Android measurement above now also compares 1,250 events with empty run IDs
against the same events with a 36-character run ID. This reference uses the current
serializer in both cases and isolates the ID value's cost; the new field name is
already present in the empty-ID reference. A populated ID adds 36 bytes per event
against that reference, or 47 bytes against the former JSON that omitted `runId`.

Measured on 2026-10-16 on the shared Linux x86-64 development VM: Intel Xeon
2.80 GHz, 7 logical CPUs, 19,006 MiB RAM, JDK 21.0.12+101 LTS, Robolectric 4.17,
SDK 35, with Gradle validation active. The workload, five warmup writes, 30
samples per phase, identity sealing, warm OS cache and CPU accounting are the
same as above. These are local file-cache costs, excluding Android Keystore and
production network latency.

| Run IDs | Retained events | Encoded bytes | Write median | Warm read median | Cold read median | Process CPU |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Empty (reference) | 1,250 | 705,573 | 53.105 ms | 1.604 ms | 35.913 ms | 6,460 ms |
| 36-character ID | 1,250 | 750,573 | 41.312 ms | 0.604 ms | 24.459 ms | 4,090 ms |

The populated values add 45,000 bytes (6.4%) against the empty-ID reference.
The populated-ID phase runs last, so its lower measured latency/CPU reflects
JIT, caching and shared-host variation; it does not demonstrate a speedup.

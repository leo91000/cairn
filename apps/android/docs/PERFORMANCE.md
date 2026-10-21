# Android performance diagnostics

Automatic sharing with Beacon is **enabled by default**. The settings panel exposes
**Envoyer les diagnostics de performances** to disable it on this phone. Reports
are associated with the signed-in Cairn account, including when it has no selected
installation. They are not anonymous. No conversation content is collected.

After ten seconds in the foreground, the app sends the latest cumulative report
to `POST /api/account/telemetry/performance`, then at most once per minute. Uploads
stop when the app leaves STARTED, the account signs out or sharing is disabled.
The monotonic upload deadline survives foreground entries, preference toggles and
UI recreation within the process, including cancellation of an in-flight request.
Serialization and upload run away from the UI dispatcher. Requests have a five-second
budget; errors never display a toast or interrupt navigation. There is no background
worker or persistent upload queue. Short foreground visits can produce no upload.
Telemetry requests do not measure themselves.

Beacon stores one latest report per account/window, updating only increasing
sequence numbers. Retries and older arrivals cannot double-count frames. Window
IDs are random and reset with the process, account changes and the local reset
button; they are not stable device identifiers. Beacon retains at most 256 windows
per account for 14 days, with account-deletion cascade and scheduled cleanup.
Opting out stops future uploads; it does not immediately delete existing reports.

Open **Paramètres → Diagnostic des performances**, reset the measurements, reproduce
the lag by navigating or swiping between conversations, then return and copy or
share the report. Reopening the settings refreshes the report; **Actualiser les
mesures** refreshes it while the panel remains open.

Diagnostics are available to installation owners and members. They are collected
locally in memory without disk logging. Force-stopping the app loses measurements
that have not been uploaded. Manual copying/sharing remains available with automatic
sharing disabled; Android's chooser controls the manual share destination.

The report includes app/build version, phone manufacturer/model, Android API,
current direct/relay/offline connection category, Java heap usage and:

- Frame counts by screen category and whether the conversation pager is moving.
  Missed deadlines use Android's per-frame deadline on API 31+, with the display
  refresh period as fallback on older devices. First-draw frames are excluded.
  Frames of at least 700 ms are counted separately. Dropped frame reports are
  included so incomplete samples are visible.
- Maximum total, UI scheduling delay, measure/layout, draw and GPU durations.
  GPU duration is unavailable before API 31. These stage maxima can belong to
  different frames and must not be added together.
- API request wall time, JSON decoding, history cache reads/saves, history merge,
  timeline construction, Markdown parsing/rendering, live event reduction and
  initial live catch-up. Catch-up measures the first complete network snapshot,
  not the moment cached content was painted. API request timings include waiting
  for the server; a request started on the main thread is not evidence that it
  blocked that thread. Decode/render timings measure synchronous work.
- Counts, mean/max and a histogram upper bound for p95 operation duration, plus
  the latest 40 operation/frame samples of at least 50 ms. Counters and fixed
  histograms cover the whole session since reset; the slow sample list is bounded.

Screen and operation categories are fixed enums. No conversation/account/installation
IDs, titles, messages, file names, URLs, headers, cookies, tokens, payloads, error
messages or stack traces are retained. Frame collection uses Android's
[FrameMetrics](https://developer.android.com/reference/android/view/FrameMetrics)
listener on a dedicated handler thread, attached only while the activity is started.
Screen/swipe attribution reflects the category when a callback is processed, so
frames at navigation boundaries can fall into either adjacent category. These are
diagnostics, not a replacement for a release-build Perfetto trace on the affected phone.
The mixed Fil pager reports the settled page's conversation or mission category,
including when a swipe does not change the original navigation route.

## Regressions addressed

1. `get<T>` and `send<T>` previously decoded responses on their caller, usually
   the UI dispatcher used by navigation and polling. Decoding now runs on
   `Dispatchers.Default`, then returns to the original dispatcher. The integration
   test uses real HTTP responses and a serializer that observes its executing
   thread, for both reads and writes.
2. Preloaded conversation pages previously triggered older-history pagination
   even while hidden. Only the selected page now starts automatic pagination.
   Cached content and the first live snapshot remain available for previews.
   The regression opens a paginated conversation, verifies that its neighbor is
   rendered without a history request, then swipes to it and verifies pagination
   starts. The same case runs under Robolectric and on an Android device.
3. Each older page rebuilds the conversation timeline on the UI thread, and that
   presented every tool event by pretty-printing its whole recorded JSON, outputs
   included, for a "Détails techniques" panel that is only shown when opened.
   Tool rows did the same each time they were composed while scrolling. The source
   is now formatted when it is read. `HistoryPagingCostTest` prepends 40 pages of
   100 events (turns of eight 1.5 KB command outputs and a reply) to a recent
   window. On the JVM, run on its own, with 4,100 loaded events, rebuilding the
   timeline went from 10.4 ms to 3.1 ms per page (0.47 to 0.16 ms with 200
   events); merging the page stays under 0.4 ms. The removed cost grows with
   output size, so real conversations save more than this fixture.

The first two regression tests failed before their respective fixes. These prove
removal of UI-thread decoding and hidden-page history requests; none of these
establish a frame-rate improvement on a user's production phone.

From `apps/android/`, with `ANDROID_HOME` configured:

```sh
./gradlew spotlessCheck testDebugUnitTest lintDebug
./gradlew connectedDebugAndroidTest \
  -Pandroid.testInstrumentationRunnerArguments.class=build.cairn.app.ui.ChatSwipeDeviceTest,build.cairn.app.ui.PerformanceDeviceTest,build.cairn.app.ui.FilPagerDeviceTest
```

The device diagnostics test collects real native frame reports, copies and parses
the report, checks its shape and absence of auth fields, and saves synthetic-fixture
evidence under the app's private `files/performance-validation/` test directory.
Tests in `PerformanceStoreTest` cover refresh-rate deadlines, frozen frames, bounded
retention, reset, percentile bounds and concurrent collectors.
`PerformanceUploaderTest` exercises restarted foreground/opt-in collections and
cancelled requests with a virtual clock. `FilPagerDeviceTest` checks screen
attribution while swiping between missions and conversations and returning to Fil.
These synthetic workloads do not measure production network latency or performance
on the affected physical phone.

## Inspecting received reports

For the bounded writer that keeps cache persistence off the live publication path,
its JVM/device regression, and opt-in before/after measurements, see
[Android cache persistence](../../../docs/ANDROID-CACHE-DIAGNOSIS.md).
Measurements exclude Android Keystore and do not establish affected-phone performance.

`GET /api/account/telemetry/performance` returns the signed-in account's latest 20
unexpired reports with `Cache-Control: no-store`. Mutations require the account
session and CSRF token. The receiver rejects unknown fields, screen/operation
names, invalid counters, excessive lists and bodies larger than 64 KiB. No
installation availability or direct connection is needed for telemetry.

Operators can run the following read-only query on the **Beacon** database using
their existing operational access. It aggregates the latest revision of each window
by app version, phone model, screen and swipe state, without exposing account IDs:

```sql
SELECT report->>'appVersion' AS version,
       report->>'model' AS model,
       frame->>'screen' AS screen,
       frame->>'swiping' AS swiping,
       sum((frame->>'count')::bigint) AS frames,
       sum((frame->>'missedDeadline')::bigint) AS late_frames,
       sum((frame->>'frozen')::bigint) AS frozen_frames,
       max((frame->>'maxMs')::double precision) AS worst_frame_ms
FROM client_performance,
     LATERAL jsonb_array_elements(report->'measurements'->'frames') AS frame
WHERE updated_at > now() - interval '14 days'
GROUP BY 1, 2, 3, 4
ORDER BY frozen_frames DESC, late_frames DESC;
```

Android uploader tests cover idle suppression, revision identity, retry without
clearing evidence, cancellation and off-main serialization. The shared app/device
scenario checks an automatic POST to the account endpoint without titles or
credentials in the body. Beacon tests use real PostgreSQL and verify persistence,
idempotency, account isolation, schema limits, throttling, expiry and deletion.

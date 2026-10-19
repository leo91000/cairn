# Chat rendering measurements

This change measures the real web reader and native Android preparation against
main `7b04f7ff`, on the same host and boot for each before/after pair. Earlier audit
numbers from another reference are deliberately excluded. Raw measurements and
source fingerprints accompany the results below.

## Changes and preserved behavior

Web timestamps cache the exact native Date labels within a reader, invalidating
on locale, time zone, calendar, numbering or hour-cycle context changes. Removed
messages do not accumulate in the cache. Markdown still parses and sanitizes the
complete document on every content change, so references, raw HTML and incomplete
syntax retain their existing semantics. A bounded component-owned cache reuses
unchanged code highlighting. Sanitized nodes are moved directly into the reader;
equal top-level prefixes and suffixes retain selection, link focus and table scroll.
Documents with raw HTML tokens keep the historical serialization/reparse after
transformations, preserving SVG namespaces and raw-text container semantics.

Android skips activity presentation for events already classified as messages
while scanning for recovered connection notices. User turn boundaries still clear
pending notices; agent messages still count as recovery. Timeline folding, wire
IDs, display keys, lazy rendering and native Markdown layout remain unchanged.

## Reproduction

Use the pinned runtimes and dependencies. Copy the measurement script and fixture
from this change into a detached checkout of the reference, sharing the installed
dependencies. Run each build serially without other compilation workloads:

```sh
mise exec -- node scripts/measure-chat-rendering.mjs /absolute/path/web.json
```

Character counts are initial UTF-16 code units; they are not encoded byte sizes.
The script builds production components, starts a local preview, and launches a
fresh Chromium process per scenario. Both 1440×900 and 390×844 viewports cover
40/400/2,000 prior messages and 2,000/20,000/100,000-character streamed answers. There are five timed
fresh mounts after one warmup and 30 timed updates after ten warmups. Update
timing includes Vue and any synchronous parent layout; the additional forced
layout interval is reported separately. Scroll intervals use 120 animation frames,
not real touch input. Deep event refs match the application; `--raw-props` opts
into component isolation. Source hashes identify the code actually measured.

For Android, use the project wrapper, the required SDK and an API 36 AOSP emulator
with KVM, then build and install both debug APKs. Copy the identical benchmark device test
into the reference checkout before building it, leaving its production sources
at the reference revision. Run the opt-in diagnostic once per installed app APK:

```sh
adb -s emulator-5580 shell am instrument -w \
  -e cairnRenderingBenchmark true \
  -e class build.cairn.app.ui.ChatRenderingBenchmarkDeviceTest#timelinePreparation \
  build.cairn.app.test/androidx.test.runner.AndroidJUnitRunner
adb -s emulator-5580 exec-out run-as build.cairn.app \
  cat files/chat-rendering-benchmark/timeline.json > timeline.json
```

`nativePipeline` and `openingScrollingAndStreaming` provide the complementary
Markdown/layout and native UI diagnostics. These are synthetic debug emulator
measurements, not release-device or production network latency. The timeline
workload keeps 40/400/2,000 messages, streams 40 replacements with ten warmups,
and checks stable message keys. It reports wall time, thread CPU and reconnection
scan time for user-only, mixed and 100,000-character active-answer histories.

## Web results

Chromium 153.0.8010.12, Node v24.21.0, Linux x64, 12 logical Xeon CPUs, same host boot. All numbers below are milliseconds. Both JSON files record Git HEAD `7b04f7ff544aa50dbb9d25b4ef7e56527a866aec`: the candidate was measured before commit. Their per-file source hashes distinguish reference and candidate; candidate production hashes are checked again against the delivered tree.

| Viewport | Prior messages | Initial characters | Update p50 before → after | Update p95 before → after | Mount p50 before → after |
| --- | ---: | ---: | --- | --- | --- |
| 1440 px | 40 | 2,000 | 3.6 → 2.0 | 4.2 → 3.0 | 18.4 → 17.7 |
| 1440 px | 400 | 2,000 | 26.5 → 8.1 | 62.7 → 13.6 | 155.2 → 232.8 |
| 1440 px | 400 | 100,000 | 80.2 → 99.2 | 136.3 → 107.2 | 208.1 → 237.7 |
| 1440 px | 2000 | 2,000 | 81.8 → 15.2 | 183.2 → 35.6 | 766.4 → 700.5 |
| 1440 px | 2000 | 20,000 | 96.7 → 29.0 | 275.2 → 57.8 | 601.7 → 610.4 |
| 1440 px | 2000 | 100,000 | 119.0 → 66.1 | 222.2 → 157.1 | 579.7 → 669.4 |
| 390 px | 40 | 2,000 | 3.9 → 2.0 | 8.7 → 2.4 | 19.7 → 17.6 |
| 390 px | 400 | 2,000 | 18.7 → 3.8 | 43.3 → 5.1 | 114.6 → 118.0 |
| 390 px | 400 | 100,000 | 59.9 → 45.7 | 63.6 → 132.4 | 239.0 → 408.8 |
| 390 px | 2000 | 2,000 | 80.9 → 13.5 | 127.5 → 21.9 | 560.1 → 510.7 |
| 390 px | 2000 | 20,000 | 103.9 → 23.2 | 224.2 → 31.7 | 615.6 → 527.3 |
| 390 px | 2000 | 100,000 | 164.5 → 54.9 | 298.8 → 73.0 | 686.3 → 589.9 |

With 2,000 prior messages and a 2,000-character active response, update p50 falls 81% on desktop and 83% at phone width. Opening does not show a consistent improvement: native date formatting and complete history construction still run on a fresh mount. Five mount samples are especially sensitive to GC and host variation.

The first 400-message/100,000-character desktop trial is slower after (80.2 → 99.2 ms p50). A serial repeat of that exact scenario gives 56.5 → 43.1 ms; phone width gives 61.0 → 43.3 ms. Repeat mount p50 is 170.9 → 173.7 ms desktop and 187.8 → 169.4 ms phone. The initial slower sample is retained in the report; the repeat does not establish a stable regression or an opening gain.

| Standalone Markdown, 100,000 initial characters | Viewport | Update p50 before → after | Update p95 before → after |
| --- | --- | --- | --- |
| code | 1440 px | 57.3 → 29.5 | 107.9 → 47.0 |
| paragraph | 1440 px | 9.9 → 9.9 | 11.2 → 11.8 |
| code | 390 px | 53.8 → 34.2 | 61.7 → 74.8 |
| paragraph | 390 px | 11.8 → 9.5 | 21.6 → 11.1 |

The code fixture repeats short completed fences; highlighting reuse depends on unchanged code and language. A single growing fence still needs highlighting on each change. The cache admits at most 64 entries and 256,000 UTF-16 code units (keys plus generated HTML) per pass; only current and previous passes are retained. No universal Markdown speedup is claimed. Raw HTML retains its historical parse normalization.

Static scroll p50/p95 remain near 16.7 ms in both builds, with 0–1 intervals over 25 ms before and 0–2 after per 120-frame scenario. This animation-frame-driven sweep is not touch responsiveness or a physical-phone FPS measurement. Every render delivered the final fragment. Unchanged prefix nodes survive streaming in the candidate, and browser tests cover selection, focus, table scroll and full-render equivalence.

Raw measurements: [before](measurements/chat-rendering/web-before.json), [after](measurements/chat-rendering/web-after.json), [repeat before](measurements/chat-rendering/web-before-repeat.json), [repeat after](measurements/chat-rendering/web-after-repeat.json).

## Android results

On API 36 AOSP x86_64 with KVM, preparation (including stable keys) improves as follows. Times are milliseconds per streamed replacement; these are CPU/preparation measurements, not frame times.

| Messages including active answer | Shape | Wall p50 before → after | Wall p95 before → after | Thread CPU p50 before → after |
| --- | --- | --- | --- | --- |
| 40 | mixed | 3.21 → 0.86 | 6.34 → 0.98 | 3.17 → 0.86 |
| 40 | large_answer | 5.09 → 0.64 | 8.06 → 1.01 | 5.03 → 0.57 |
| 400 | mixed | 7.74 → 1.13 | 14.98 → 2.81 | 7.66 → 1.10 |
| 400 | large_answer | 10.26 → 0.97 | 22.93 → 2.65 | 9.19 → 0.95 |
| 2000 | mixed | 39.75 → 4.31 | 49.40 → 8.54 | 38.12 → 4.23 |
| 2000 | large_answer | 40.60 → 4.36 | 63.43 → 7.67 | 38.60 → 4.26 |

At 2,000 mixed messages, p50 preparation falls 89%; the 100,000-character active answer shows the same improvement (40.60 → 4.36 ms). The regression test observes two complete message-payload serializations on the reference and zero after the fix. All 19 timeline tests pass after correction, including reconnection recovery and identity contracts.

Raw data: [before](measurements/chat-rendering/android-before-timeline.json), [after](measurements/chat-rendering/android-after-timeline.json), [provenance](measurements/chat-rendering/android-provenance.json).

The after-only native diagnostics are separate from the timeline comparison.
The Markdown block cache and lazy history reader already existed on main and
are unchanged by this PR. With 100,000-character split across paragraphs, 347 of 348 blocks
retain identity; warmed bind/layout p50 is 0.53 ms (Markdown preparation 46.68 ms
on the benchmark thread). A single 100,000-character paragraph retains no unchanged block:
preparation is 80.77 ms and bind/layout 137.30 ms. This supports keeping the
existing block strategy and recording the giant-paragraph limitation rather
than changing paragraph boundaries or text selection.

The synthetic lazy reader keeps eight native text views for the 2,000-character response
at all three history sizes. Opening observed by the test is 108–147 ms; a 100,000-character
prose response takes 897 ms and creates 348 text views inside that answer. UI
observation includes synchronization/polling and is not a frame latency measure.
Frame diagnostics include setup/opening and streaming warmup; they are not a
before/after frame-rate result.

Raw after-only diagnostics: [Markdown/layout](measurements/chat-rendering/android-after-pipeline.json),
[opening/scrolling/streaming](measurements/chat-rendering/android-after-ui.json).

## Limits

Opening still constructs the complete web history; this change adds no
virtualization. Full-document Markdown parsing remains linear in answer size.
A single giant native paragraph still needs whole-paragraph layout; splitting it
would affect Markdown, selection and reading anchors and is outside this focused
change. Emulator frame observations and test-observed UI latency are diagnostic,
not a guarantee of physical-device frame rate. Browser checks cover Chromium and
WebKit; native tests cover the existing reader contracts.

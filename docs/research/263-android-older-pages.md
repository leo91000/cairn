# Android older-page ownership (#263 / C2)

Reference: main `df81d19c961c2b23ca7ba2ffdbcc632b8a91aeb6` (2026-10-18),
after F13/#293. The audit observation remains valid: `rememberLive` owns the
loaded pages and their boundary, while the retained stream has its own boundary.

## Discriminating experiment

Before changing `rememberLive`, introduce the pure `OlderPages` reducer and run:

```sh
cd apps/android
mise exec -- ./gradlew :app:testDebugUnitTest \
  --tests build.cairn.app.data.OlderPagesTest \
  --tests build.cairn.app.data.LiveTest --no-daemon --max-workers=2
```

`OlderPagesTest` exercises a response after history replacement, a response after
an epoch reset with the same history, a mismatching response revision, a later
stream emission after a page, restored pages followed by a new page, and rejection
of a non-progressing page without committing partial state.

The `LiveTest` reconnect experiment runs the real session and MockWebServer SSE
transport. It applies an older page while collection is active, closes the first
SSE body (the transport reports `IOException`), and resumes at the accepted cursor.
It checks the page and its boundary survive the reconnect. `session.mutex.tryLock()`
fails during collection, including the reconnect: a `prepend` using that mutex
cannot execute while this reader is active. Cancellation releases the mutex and
closes the subscription. This tests an overlay during a real session, rather than
claiming a production `LiveSession.prepend` was implemented.

Result before UI integration: **21 tests passed** (5 `OlderPagesTest`, 16
`LiveTest`, including the reconnect/lock probe), 0 failures/errors, on 2026-10-18.
`LiveUi.kt` still matched main's blob `636faa2b718b9eb29caa03a81042f8feb7cabe46`.
The first cold build reached its 20-minute bound during test compilation after
compiling the app. One warm continuation reused those outputs and completed in
4m12s. This is validation runtime, not a `HISTORY_MERGE` benchmark.

Before the adapter change, both new `OlderPagesScopeTest` cases failed waiting for
the outstanding call to be cancelled and the new scope to be shown. The request
scope had the lifetime of the whole Composable, rather than its path/API keys.

## Decision

Keep the pure `OlderPages` alternative. It owns history/epoch validation, merging,
page progress validation and the displayed pagination boundary. The Composable
keeps the request, loading/error state, cache adapter and lifecycle adapter. Page
application returns the complete displayed snapshot, which the adapter then saves
using the existing cache generation guard. Cached pages remain in the session's
restored events; subsequent pages are added by `OlderPages`.

Moving prepend into the current session would require more synchronization rules:
the existing mutex covers the entire collection, and the stream loop keeps local
snapshot/cursor variables. A second lock would require moving and serializing all
state transitions and emissions; a command channel would require multiplexing
frames/pages plus handling commands while collection is stopped. Neither is needed
for this extraction. Do not mutate the session's accumulator from the UI thread.

`OlderPages` is confined to the same Compose coroutine context as the displayed
snapshot. A child coroutine scope keyed by path and API has an explicit
`DisposableEffect` cancellation when the chat or connection scope changes; API cancellation cancels
the underlying call. Replacing a history or epoch rejects its late page. Lifecycle
STOP/START retains the existing stream resume behavior. No `CairnViewModel` scope
or account/installation refactor is included.

## Contracts, evidence and limits

The web already owns historical pages in `useLiveRun` and validates the connection
generation and replacement count before application. Android keeps native reader
presentation and the same history/epoch behavior. `OlderPagesScopeTest` covers
outstanding-page cancellation on path/API replacement, paging in the new scope and
saving its combined snapshot. Its delayed response body keeps making progress
until cancellation, so an HTTP read timeout cannot substitute for lifecycle
cancellation. Existing recent-window, cache, paging/scroll and
reconnect tests remain relevant; device results belong to the PR delivery evidence.

LiveAccumulator, HistoryCache, wire batches and persisted formats are unchanged.
This changes in-memory ownership and request cancellation, not storage encoding or
the compatibility epoch. Cache read/write algorithms and payloads are unchanged.

The stream and overlay still have separate internal boundaries and merge on each
emission. No latency, CPU or allocation improvement is claimed. Reopen integration
only if a `HISTORY_MERGE` measurement justifies the larger reducer/collection change
and a prototype removes more synchronization rules than it adds. Report workload,
host and local-versus-production conditions for such a measurement.

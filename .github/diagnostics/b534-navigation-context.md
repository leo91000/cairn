# B534 first-attempt native navigation diagnostic

This observer is diagnostic only and must never be merged. Root explicitly ceded the sole heavy slot for this B534 case. The historical fbd/59d skill probes remain preserved but are not selected by this workflow. No new B534 diagnostic run has been dispatched yet; a fresh independent full-diff review is required first.

## Original failure and privacy

- Run42518166046, head `5d1645ea808cb18f7b64aa0dcd92987cc3ceeb2d`, tree `d181075913d3200a9a535ce742dcc43a0e11d706`, base main `7028fbfdd6d13e27b8f4e7db767899828d631b42`.
- Job128847095867 `browser (webkit-4)` failed; 54 PASS and one flaky first attempt, followed by a non-credited retry pass. CLI job128851105686 passed. Other green jobs do not resolve this first failure.
- Artifact13631819247 `browser-evidence-webkit-4`, 30,645,682 bytes, SHA256 `4c946240608f863e049212a448a7ddccb264bc9bfb7c0a9072360d67e0d7d75c`, verified privately.
- First trace: `test-results/mcps-manages-MCP-connectio-d1095-ccess-on-desktop-and-mobile-layout-webkit-mcps/trace.zip`. No retry trace is substituted.
- Bundle artifact13632112584 SHA256 `e2b3d739d197852233a4911165889a3d13926ff56da9f562ce0281653767454a`, verified privately to attribute the recorded stack. Neither archive is committed or published; no headers, cookies, query values or tokens are projected.

## Observed facts, not a root-cause claim

`page.goto` from MCPs to Agents starts at76774.042ms; EventSource chat-stream page error at76783.523ms (+9.481); chat-list fetch page error at76784.389ms (+10.347). The successful Agents document request starts76780.676ms. The availability fetch starts76775.673ms and is cancelled (status-1). Recorded chat streams have HTTP200 plus `Load request cancelled`; their final cancellation timestamps are not present. The failed chat-list fetch has no network entry. No HTTP403 evidence appears for that failed call.

The exact shipped bundle attributes the chat-list error to `EventSource.onerror → liveConnection.retry → useLiveRun reconnecting status callback → api('/chats') → request/sendRequest → native fetch`. A DOM snapshot at74540.210ms records direct transport; this is earlier than the departure and does not by itself prove the route at76774ms. No native pagehide/pageshow timestamps are available in the original trace. These omissions prevent assigning a cause from the trace alone.

The MCP test, chat-list/live-connection/use-live-run/installation-transport/document-lifetime and MCP view source blobs are identical between7028 and5d. B changes App/API/authorization and fixture code elsewhere. Source equality is not a baseline reproduction or proof of independence from B. Root731 changes initial-pageshow restoration in two files; without lifecycle evidence, it is not yet known whether it covers this case. Neither B nor731 branches are edited here.

## Prepared controlled loop

Each immutable source is built and exercised on a fresh disposable job with the same pinned Playwright1.64.1 Noble image, Rust1.97.1, Postgres17 service, build flags and source-specific frozen dependencies. The build commands match the browser fixture commands in the full workflow; Vite has no resume-diagnostics flag, matching that workflow. Source identity is recorded before setup. The matrix is sequential, 25min/job; each source runs both controls serially inside one five-minute step. Both return codes are preserved and any failure keeps the job red. No retry is used.

The probe performs a real MCP OAuth connect and callback, uses the original dark390×844 mobile state, and requires successfully accepted direct chat-list batches before OAuth and again after its callback. It then holds one successful backend availability response from the real periodic poll while direct transport remains selected. It performs the real native `goto('/agents')`, holding its successful HTML response until the old poll rejection and any admitted native chat-source outcome are observed. Native lifecycle, signal-abort, transport and stream/read events are journalled with document identity and separate host/browser times. No query values are logged.

The single experimental variable is whether the *actual availability caller's own controller* is explicitly aborted immediately before the native goto. The native-cancel control does not abort it. This is a diagnostic counterfactual, not a production fix, error filter, or claim that explicit abort avoids every native WebKit diagnostic. Every page error remains subject to the unchanged empty-array assertion. Missing direct success, stimulus, native outcome or closure is non-green. All unknown or unexpected new-document HTTP/body outcomes are non-green.

```sh
CAIRN_B534_CONTROL=native-cancel pnpm test:e2e mcps.spec.ts --project=layout-webkit-mcps --grep '^B534 native Agents navigation' --workers=2 --retries=0 --fail-on-flaky-tests --trace=on
CAIRN_B534_CONTROL=abort-before-goto pnpm test:e2e mcps.spec.ts --project=layout-webkit-mcps --grep '^B534 native Agents navigation' --workers=2 --retries=0 --fail-on-flaky-tests --trace=on
```

The commands are identical across the immutable base/B sources. This selected diagnostic is not the original full `layout-webkit-* --shard=4/4` qualification and cannot replace that workload. No native diagnostic has run, and no red-capable native loop has yet been established. Preserve first failures and distinguish the original EventSource-plus-chat-list symptom from observer failures or a different availability-only error. A later repair still requires current-main source, fresh full reviews, exact affected qualification, ordinary guarded merge and verified post-CI. UI534/679 capture gates, root731 ownership and all production prohibitions remain in force.

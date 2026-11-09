# Focused WebKit gate-retirement diagnostic

Product `b9f1e7010345e5e0d1172c79a540344f7a1b0772` tree `1046270fa120e8a3156f44cfcb63532dd8fe6c7c`, base main `7028fbfdd6d13e27b8f4e7db767899828d631b42`.
Diagnostic only: no Cargo, Cairn runtime, fixtures or product qualification.
Pinned Playwright 1.64.1; real loopback HTTP proxy; actual product availability
transport, BeaconApp polling block and e2e observer. Minimal session/state adapters
are not Vue/application integration coverage. The ordinary MCP WebKit device
settings, Page.goto stimulus and original clock install/pause/runFor/resume
sequence are used in both arms.

Exactly ONE WebKit initial-retired cell. The only new variable after the
unchanged clock.install/pauseAt/runFor3100/resume seed, BEFORE Page.goto, is
window.setTimeout/clearTimeout replaced with bound native originals captured by
pinned Playwright globalThis.__pwClock.builtins before fake install(). Require
both original functions and successful distinct-to-identical identity receipts;
missing/ambiguous originals are red. Do NOT uninstall the clock, change Date,
performance or AbortSignal, or alter the 3000ms poll interval. This private pinned
clock interface is diagnostic only and must not become a product dependency.

Previous same initial-retired control: run42595694869/artifact13663865688 SHA256
947f7915a598303db47fce13f827d3ef3cbf8c9b16bbbab9e5525a376aa57b64.
That control acquired initial HTTP closure+browsercheckpoint before read3, but
still observed native error/no ingress/one pageerror; it remains red. It is NOT
rerun. Source clock.resume resumes the fake-clock pump; this does not prove clock
causality. Compare only actual native read/gate receipts under the single timeout
scheduler substitution; no sleeps, retry, lease, filter or broad cancellation.

Keep actual b9 transport/poll/probe/gate, engine/device, Page.goto, first HTTP
retirement boundary, all journal and closure/error guards. baselineMatched=false
and contrastObserved=false; any causal interpretation must cite both original
receipts and limits. Positive exact body is only scheduler evidence; native error,
pageerror, unknown or closure failure remains red. No product, retained-polling,
physical BFCache or genuine-failure qualification here. The proposed product
branch may contain later test-only corrections but this diagnostic MUST checkout
immutable b9f1e7010345e5e0d1172c79a540344f7a1b0772 for the paired comparison.

The original 7-second observation/closure budgets, 5-minute steps and 25-minute job
remain. Source and diagnostic hashes must agree before/after. All first failures
and traces remain private. No headers/cookies/query values/tokens are projected.

Retained download polling, physical BFCache, genuine failure handling, DirectRelay,
auth and full product qualification remain unqualified here. The exact full
qualification workflow remains preserved in observer history at d393b882bd2cc80b68734263ac453ef1556eba84.

Archive note: the old skill-availability-probe is unselected and must not run.
Its two previously identified P2 lifecycle/body barriers remain open; this
observation does not resolve them. Only availability-reader-native.mjs executes.

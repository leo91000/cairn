# Focused WebKit gate-retirement diagnostic

Product `b9f1e7010345e5e0d1172c79a540344f7a1b0772` tree `4bd409a19890fe30a4ef4d6e8ec5b99eaea6bca4`, base main `7028fbfdd6d13e27b8f4e7db767899828d631b42`.
Diagnostic only: no Cargo, Cairn runtime, fixtures or product qualification.
Pinned Playwright 1.64.1; real loopback HTTP proxy; actual product availability
transport, BeaconApp polling block and e2e observer. Minimal session/state adapters
are not Vue/application integration coverage. The ordinary MCP WebKit device
settings, Page.goto stimulus and original clock install/pause/runFor/resume
sequence are used in both arms.

Exactly ONE WebKit cell: initial-retired. The previously acquired initial-held
cell is NOT rerun. Run42593972210/artifact13662287543 SHA256
869c4eed43fc93f4cb333a3b1b8e9170493389ecab89e4137337cb4665d20f17
retains its native read3 error, zero proxy ingress after owned closure, exact
initial read2 abort, complete read journal, one native access-control pageerror
and strict red exit. That pageerror stopped the second cell. No baseline is
turned green or represented as a new complete two-cell contrast.

The SAME observe() implementation now acquires only the missing cell: release
initial bytes, acknowledge HTTP closure and a synchronous browser journal
boundary BEFORE the next ordinary poll; keep destination held. Actual source,
engine, device, clock, Page.goto, deadlines, error guards and cleanup are
unchanged. A native rejection or any pageerror stays red. Pending, unknown,
missing terminal, unexpected error or cleanup failure stays non-green. The
result has baselineMatched=false and contrastObserved=false; any comparison
with the prior baseline must cite both original receipts and its limitations.
Retained polling, BFCache and product integration are NOT qualified.

The original 7-second observation/closure budgets, 5-minute steps and 25-minute job
remain. Source and diagnostic hashes must agree before/after. All first failures
and traces remain private. No headers/cookies/query values/tokens are projected.

Retained download polling, physical BFCache, genuine failure handling, DirectRelay,
auth and full product qualification remain unqualified here. The exact full
qualification workflow remains preserved in observer history at d393b882bd2cc80b68734263ac453ef1556eba84.

Archive note: the old skill-availability-probe is unselected and must not run.
Its two previously identified P2 lifecycle/body barriers remain open; this
observation does not resolve them. Only availability-reader-native.mjs executes.

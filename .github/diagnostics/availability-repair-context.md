# Focused WebKit gate-retirement diagnostic

Product `b9f1e7010345e5e0d1172c79a540344f7a1b0772` tree `4bd409a19890fe30a4ef4d6e8ec5b99eaea6bca4`, base main `7028fbfdd6d13e27b8f4e7db767899828d631b42`.
Diagnostic only: no Cargo, Cairn runtime, fixtures or product qualification.
Pinned Playwright 1.64.1; real loopback HTTP proxy; actual product availability
transport, BeaconApp polling block and e2e observer. Minimal session/state adapters
are not Vue/application integration coverage. The ordinary MCP WebKit device
settings, Page.goto stimulus and original clock install/pause/runFor/resume
sequence are used in both arms.

Exactly two serial WebKit arms. First retains the initial response gate. Second
releases it and requires its HTTP closure before the next periodic read begins.
Both hold the destination and observe the actual next read's precise body or
unowned native rejection. Only this one gate-retirement variable differs. The
first arm must reproduce the current unowned replacement failure without a
received HTTP request; otherwise the contrast is inconclusive and stops. A
known failed baseline may proceed to the contrast only after complete observation
and all owners close. Unknown, timeout, missing journal or cleanup failure stops
remaining cells. Every native rejection and all pageerrors remain globally red,
including an expected diagnostic baseline failure. There is no green product
inference from a positive contrast.

The original 7-second observation/closure budgets, 5-minute steps and 25-minute job
remain. Source and diagnostic hashes must agree before/after. All first failures
and traces remain private. No headers/cookies/query values/tokens are projected.

Retained download polling, physical BFCache, genuine failure handling, DirectRelay,
auth and full product qualification remain unqualified here. The exact full
qualification workflow remains preserved in observer history at d393b882bd2cc80b68734263ac453ef1556eba84.

Archive note: the old skill-availability-probe is unselected and must not run.
Its two previously identified P2 lifecycle/body barriers remain open; this
observation does not resolve them. Only availability-reader-native.mjs executes.

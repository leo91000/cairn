# Performance evidence

Measured on 2026-09-09 with Node 26.8.1, local SQLite WAL, 5,001 stored runs,
1,000 events, roughly 16 KB of instructions and 2 KB of summary per seeded run.
Each endpoint had five warm-up requests and 50 measured requests through Fastify
`inject`. This measures application/database work, not network or TLS latency.

Reproduce with `pnpm exec tsx tests/performance.ts`. The script creates and removes
an isolated temporary database and never uses real Codex credentials.

| Endpoint | Before median / p95 | After median / p95 | Before → after response |
| --- | --- | --- | --- |
| 40 recent runs | 2.98 / 3.65 ms | 0.60 / 0.75 ms | 96,739 → 14,319 bytes |
| 40 failed runs | 1.59 / 2.22 ms | 0.61 / 0.69 ms | 98,761 → 14,241 bytes |
| Overview | 2.50 / 2.60 ms | 0.35 / 0.37 ms | 18,107 → 3,303 bytes |
| Incremental 100-event page | 0.26 / 0.29 ms | 0.30 / 0.35 ms | 14,301 → 14,301 bytes |

Run lists previously loaded/parsing full snapshots and sent summaries unused by
the list UI. SQL now selects compact list data, filters through applicable indexes,
and uses a creation-order index instead of sorting the full history. The full
summary and snapshot remain available on the detail endpoint. Event-query timing
was already small; the minor difference above is measurement noise, not a claimed
improvement.

## Browser and resource bounds

The production build splits routes and Markdown rendering into lazy chunks. Main
JavaScript chunks are approximately 20 KB and 26 KB gzip; the Markdown renderer
is approximately 24 KB gzip and loads on pages that need it. CSS is approximately
8 KB gzip. Fonts are bundled locally and use subset loading; no external font
request blocks the initial render.

Run lists poll every five seconds while visible. Active run detail polls at 1.5
seconds, prevents overlapping fetches, and requests only events after the last ID.
Finished runs do not keep polling; further event pages can be loaded explicitly.
The UI retains at most 2,000 events for a viewed run. Server event pages contain at
most 100 entries, each capped at 16,000 characters; captured CLI output is capped
at 500 kB for technical logs and diagnostics. Tool items are independently reduced
to summaries above 16 KiB, with no cumulative tool-call budget. All tool identities,
outcomes and small results remain recorded throughout the run. Result-file reads
are capped at 100,000 bytes. Per-run event retention
and 30-day finished-event expiry bound log growth; preserved worktree disk usage
still depends on projects and requires deliberate cleanup.

SQLite WAL, task uniqueness constraints, project serialization, and a configurable positive worker
limit (default 4) avoid accidental unbounded execution. This is a single-process architecture,
not a distributed scheduler. Measurements establish behavior at the stated workload;
they do not imply unlimited scale or optimal performance on every VPS.

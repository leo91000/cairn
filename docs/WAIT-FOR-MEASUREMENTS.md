# Wait metadata measurements

Measured on 2026-10-13 against `cairn_workspace.sleep`, including the new
`github_pr_checks` condition. All cases use the same SQLite schema and JSON
encoding. No codec, compression or journal policy changes.

## Reproduction

```sh
CARGO_BUILD_JOBS=1 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_DEV_INCREMENTAL=false \
  pnpm test:backend -p cairn-installation --test wait_for \
  measure_wait_metadata_against_sleep -- --ignored --exact --nocapture
```

The backend launcher isolates live agent credentials. Each temporary installation
has one caller, a pending run dependency and a registered project. For each mode,
100 distinct registrations/replacements record a wakeup and event, followed by
500 pooled reads and 20 new SQLite pools and their first reads. Identical-request
shortcuts are excluded. The PR case has one required check in progress, one page
of check results and no additional rulesets. Its four read-only CLI requests per
registration use a synthetic Node fixture, without GitHub or network access.

Host: Linux 6.12.109, virtual Intel Xeon CPU, 12 CPU threads available,
32,363,544 KiB reported memory and about 30 GiB available. SQLite files are on
`/tmp`, a tmpfs. Shared VM host workloads are not controlled. Rust 1.97.1, dev
profile, debug info and incremental compilation disabled, one build job.
Compilation finishes before timing; no other agent build/test runs during timing.

Fresh-pool reads start with new SQLite connections and statement/page caches.
OS caches stay warm; they are not flushed. Pool creation is timed separately.
Parent CPU covers all threads via CLOCK_PROCESS_CPUTIME_ID; child CPU is measured
separately via RUSAGE_CHILDREN and captures synthetic CLI processes, not just the
manager. Registration/write latency includes validation, state checks and writes.
For the PR case it also includes four synthetic CLI startups and JSON parsing.

## Results

| Metric | Reference `sleep` | `run_finished` | `github_pr_checks` |
| --- | ---: | ---: | ---: |
| Encoded wakeup JSON bytes | 185 | 268 | 370 |
| Encoded conversation JSON bytes | 576 | 655 | 757 |
| Registration/write median, ms | 0.415 | 2.761 | 101.209 |
| Registration/write p95, ms | 0.758 | 4.279 | 123.610 |
| Registration/write mean, ms | 1.209 | 2.890 | 103.704 |
| Parent process CPU for 100 registrations, ms | 114.203 | 265.063 | 596.178 |
| Child CLI CPU for 100 registrations, ms | 0.000 | 0.000 | 9751.263 |
| Synthetic CLI requests | 0 | 0 | 400 |
| Warm read median, ms | 0.034 | 0.039 | 0.045 |
| Warm read p95, ms | 0.064 | 0.061 | 0.083 |
| First read with fresh pool median, ms | 0.373 | 0.412 | 0.385 |
| First read with fresh pool p95, ms | 0.434 | 1.278 | 0.478 |
| Fresh pool open median, ms | 0.797 | 0.840 | 0.775 |
| User-input index bytes | 4,096 | 4,096 | 4,096 |

The pending PR wakeup adds 185 JSON bytes over sleep; the internal run wakeup
adds 83. Conversation bytes include variable-length generated slugs. Warm and
fresh-pool read latencies remain local metadata measurements. The shared partial
index has one initial chat.user event per fixture; wait bookkeeping adds no
indexed user message, and real histories can allocate more pages.

PR registration is about 101 ms at the median in this synthetic fixture, dominated
by four Node CLI startups. This does not estimate GitHub network latency or the
production gh executable's CPU. It is not a disk-only write benchmark. It also
does not measure model tokens, S3, physical VM resume or production disks.
The native worker journey separately verifies yielding a single execution slot
and resuming the same conversation/session before its fallback deadline.

## Monitoring budget

The separate observer admits at most 16 unique targets per 30-second batch,
with four targets observed concurrently. A workflow takes one request; a PR
typically takes four, with ten maximum after bounded pagination and revalidation.
An entire PR observation has a 20-second timeout. Thus there are at most 1,920
periodic observations/hour, or 19,200 requests/hour in the maximal PR pagination
case, before initial registrations and other GitHub uses. API rate limits can
produce retries until the persisted fallback deadline; this is a workload bound,
not a claim that every installation can sustain that rate.

Checks are capped at 500, distinct requirements at 500 and active rules at fewer
than 200. Returned details share a 100-entry budget across present and missing
checks. Identical watchers share results only for the same agent, granted policy,
registered project origin and condition. No request holds the scheduler tick.

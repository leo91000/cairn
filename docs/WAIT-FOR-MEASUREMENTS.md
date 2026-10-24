# Wait metadata measurements

Measured on 2026-10-13 against `cairn_workspace.sleep`, including the new
`github_pr_checks` condition. All cases use the same SQLite schema and JSON
encoding. No codec, compression or journal policy changes.

The historical reproduction below uses the old `sleep` entry point. For the
current unified interface, use the command and results in
[the 2026-10-22 comparison](#unified-interface-2026-10-22).

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

## Unified interface (2026-10-22)

Compared the removed `sleep` at commit
`eceb949e86b0e0f75721a9afc777a3757277db5c` with `wait_for`'s `delay` and `at`
conditions. The [raw measurements](measurements/unified-wait-for.json) also retain
`run_finished` and `github_pr_checks` results for both versions.

Current reproduction:

```sh
CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 CARGO_INCREMENTAL=0 \
  pnpm test:backend -p cairn-installation --test wait_for \
  measure_wait_metadata -- --ignored --exact --nocapture
```

For the reference, check out the commit above in a separate worktree and run the
same command with `measure_wait_metadata_against_sleep`. Run each measurement
alone, after compilation and other tests finish. The launcher isolates agent
credentials. Both versions use the same fixture schema and SQLite settings:
100 distinct registrations/replacements, 500 warm reads, then 20 fresh SQLite
pools and their first reads. The date case schedules approximately 420 seconds
ahead, matching the duration case. Different reasons avoid the idempotent shortcut.

Host: shared Linux 6.12.109 x86_64 KVM VM, 12 Intel Xeon vCPUs, about 30 GiB RAM,
overlay filesystem (`/tmp` is on that filesystem). Rust 1.97.1, dev/test profile,
debug info and incremental compilation disabled, four build jobs. No other test
or build was active during timing; the physical host was not reserved. Compilation
finished before timing. Fresh-pool reads reset connection/statement/page caches;
OS caches were left warm. They are local metadata reads, not cold-disk or
production-network latency. Process CPU includes manager threads; child CPU is
reported separately. GitHub cases run sandboxed Luau workers against a local HTTP
fixture, with four requests per PR registration and no external GitHub calls.

| Metric | Reference `sleep` | `wait_for` / `delay` | `wait_for` / `at` |
| --- | ---: | ---: | ---: |
| Encoded wakeup JSON bytes | 185 | 232 | 249 |
| Encoded conversation JSON bytes | 679 | 725 | 743 |
| Registration/write median, ms | 0.346 | 0.901 | 0.925 |
| Registration/write p95, ms | 0.429 | 1.304 | 1.070 |
| Process CPU for 100 registrations, ms | 68.639 | 128.652 | 96.180 |
| Child process CPU for 100 registrations, ms | 0.000 | 0.000 | 0.000 |
| Warm read median, ms | 0.038 | 0.038 | 0.035 |
| Warm read p95, ms | 0.062 | 0.064 | 0.056 |
| First read with fresh pool median, ms | 0.315 | 0.327 | 0.272 |
| First read with fresh pool p95, ms | 0.378 | 0.399 | 0.351 |
| Fresh pool open median, ms | 0.636 | 0.675 | 0.684 |

The `delay` request identity adds 47 encoded wakeup bytes; `at` adds 64. The
optional `timeCondition` field distinguishes a repeated request from switching
between a date and duration, while older binaries still consume the original
timer fields and `wakeAt`. Conversation byte counts also include generated slugs.
No codec, compression, journal policy, database schema or compatibility epoch
changes. Time completion observations reuse the existing event payload format.

Registration now uses the common `wait_for` validation and short transactions,
including the message-revision checks used by external conditions. This adds
about 0.56 ms to the local duration median in this sample. Warm read medians are
about 0.04 ms in both versions. These samples establish local costs for this
workload; they provide no production latency or physical-disk performance claim.

## Grouped waits (2026-10-24)

Compared one `run_finished` condition with `all` groups of 2 and 8 distinct run
conditions at the same source tree. The [raw measurements](measurements/grouped-wait-for.json)
record the implementation tree, host conditions and all samples' summaries.

```sh
CARGO_INCREMENTAL=0 pnpm test:backend -p cairn-installation --test wait_for \
  groups::measure_group_metadata -- --ignored --exact --nocapture
```

Each fixture has one caller conversation and 1, 2 or 8 target runs. The workload
records 100 distinct registrations/replacements, reads the pending conversation
500 times with its warm pool, and opens 20 new pools for their first reads. Pool
creation is timed separately. It then measures 100 idle `tick_groups` scans and
20 progress sequences, completing one target before each scan (20, 40 and 160
progress samples). Setup, target-state writes and registration of each progress
sequence stay outside progress timing. Group progress ticks include both the scan
and its transaction; the last result also writes the ready event. Encoded sizes
cover the persisted pending, first-progress and final wakeup.

Host: shared native Linux 7.2.9-arch1-1 x86_64, Intel Core i9-14900K, 32 logical
CPUs and about 94 GiB RAM; temporary SQLite databases under `/tmp` on ext4. Rust
1.97.1, dev/test profile with debug info enabled and incremental compilation
disabled. Compilation and this task's other tests finished before timing; other
host workloads were not controlled (starting load averages 10.91 / 12.05 / 16.07).
Fresh pools reset SQLite connection/statement/page caches; OS caches stay warm.
Process CPU uses `CLOCK_PROCESS_CPUTIME_ID` across all manager threads. The native
run workloads make no external queries or child processes during timing.

| Metric | Single run reference | `all`, 2 runs | `all`, 8 runs |
| --- | ---: | ---: | ---: |
| Pending wakeup JSON bytes | 276 | 410 | 854 |
| Pending conversation JSON bytes | 795 | 930 | 1,373 |
| Wakeup bytes after first progress tick | 276 | 600 | 1,044 |
| Wakeup bytes after final progress tick | 277 | 1,409 | 4,619 |
| Registration/write median / p95, ms | 5.913 / 6.980 | 6.568 / 7.780 | 12.173 / 16.491 |
| Process CPU for 100 registrations, ms | 289.417 | 332.688 | 852.993 |
| Warm read median / p95, ms | 0.048 / 0.067 | 0.065 / 0.080 | 0.085 / 0.114 |
| First read, fresh pool median / p95, ms | 0.546 / 0.589 | 0.515 / 0.584 | 0.631 / 0.690 |
| Fresh pool open median, ms | 4.482 | 4.517 | 168.489 |
| Idle group scan median / p95, ms | 0.189 / 0.271 | 0.583 / 0.884 | 1.679 / 2.744 |
| Process CPU for 100 idle scans, ms | 19.585 | 66.785 | 194.471 |
| Progress group tick median / p95, ms | 0.264 / 0.329 | 5.074 / 6.646 | 7.381 / 87.214 |
| Process CPU per progress tick, ms | 0.279 | 1.668 | 3.616 |

The reference progress tick only scans: single conditions keep their previous
storage contract, so this new observer does not write their results. Its actual
scheduler resumption is excluded. The reference's final wakeup differs by one
byte because the measured reason has a longer sequence number. Group sizes after
completion include the per-member results and the complete group observation;
the 8-run wakeup remains below 5 KiB in this workload. Conversation sizes also
include variable-length generated slugs.

The 8-run phase has much larger pool-open and progress-tail latency. This single
shared-host sequence does not isolate workload costs from host contention; the
observed samples are retained, and no controlled performance conclusion follows
from that spike.

These are local metadata costs against an in-build single-condition reference,
not a comparison with the different hosts used in the earlier sections. They do
not measure cold physical-disk reads, production network latency, plugin response
sizes, or scans across a large installation. No codec, compression, SQLite schema,
journal policy or compatibility epoch changes are involved.

# Wait metadata measurements

Measured on 2026-10-13 against `cairn_workspace.sleep` as the reference operation.
Remeasured after making initial registration checks and the input revision atomic.
Both cases use the same SQLite schema and JSON encoding. `wait_for` adds a named
condition and extra authorization/state checks; it changes no object compression,
disk codec or journal policy.

## Reproduction

```sh
CARGO_BUILD_JOBS=1 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_DEV_INCREMENTAL=false \
  pnpm test:backend -p cairn-installation --test wait_for \
  measure_wait_metadata_against_sleep -- --ignored --exact --nocapture
```

The backend launcher isolates live agent credentials. The ignored test creates a
temporary installation, one interactive caller, one pending run dependency and a
registered project. It performs 100 distinct registrations/replacements, 500
pooled reads, then 20 fresh-pool opens and their first reads for each operation.
Every registration records a new wakeup/event; identical-request shortcuts are
excluded. No GitHub CLI/network request is used by this workload.

Host: Linux 6.12.109, Intel Xeon virtual CPU, 12 CPU threads available to the
process, 32,363,544 KiB memory reported, about 30 GiB available during inspection.
SQLite files reside on `/tmp`, a tmpfs. This is a shared execution VM; other host
workloads and memory pressure are not controlled. Rust 1.97.1, development profile,
debug information and incremental compilation disabled, one build job. Compilation
finishes before timing starts; no other agent-started build/test ran during the
measured workload.

Fresh-pool reads use newly opened SQLite connections and statement/page caches.
The OS cache is warm and was not flushed. Pool creation is reported separately.
Process CPU covers all threads of the measured process during the write phase.
The first registration's validation setup is included, so median and p95 are
useful alongside the mean.

## Results

| Metric | Reference `sleep` | `wait_for: run_finished` |
| --- | ---: | ---: |
| Encoded wakeup JSON bytes | 185 | 268 |
| Encoded conversation JSON bytes | 576 | 659 |
| Write median, ms | 0.340 | 1.489 |
| Write p95, ms | 0.455 | 2.011 |
| Write mean, ms | 0.887 | 1.582 |
| Process CPU for 100 writes, ms | 88.911 | 159.169 |
| Warm read median, ms | 0.031 | 0.033 |
| Warm read p95, ms | 0.043 | 0.047 |
| First read with fresh pool median, ms | 0.279 | 0.275 |
| First read with fresh pool p95, ms | 0.372 | 0.376 |
| Fresh pool open median, ms | 0.541 | 0.543 |
| `events_user_input` index allocated bytes in the fixture | 4,096 | 4,096 |

The conditional wakeup adds 83 encoded bytes in this fixture. Its registration
costs about 1.1 ms more at the median while pooled read latency remains in the
same range. The index is a shared schema cost: one initial `chat.user` event is
indexed in each case, and wait bookkeeping does not add index entries. Real
histories can require more index pages. Whole-conversation bytes include a
generated readable slug; that part can vary slightly between repetitions.

These results characterize local metadata operations on tmpfs. They do not
measure production disks, S3, GitHub request latency, model tokens or a physical
VM resume. The native worker journey separately verifies a single execution slot
being yielded to a dependency and the same session resuming before its 60-second
fallback, in a 4.84-second fixture test run.

## Monitoring budget

Internal run conditions are checked by the existing scheduler without network
calls. Successful non-interactive runs avoid a conversation lookup. GitHub
observation is in a separate task: at most 16 distinct targets per 30-second
batch, four concurrent CLI calls, with identical watchers deduplicated by agent,
granted policy, registered project origin and condition. Initial registration probes are separate from this
periodic budget. Temporary failures retain the persisted deadline and next check.

An illustrative 20-minute GitHub wait uses approximately 40 background status
checks at this cadence and one LLM continuation when resolved. This moves the
observation work out of model inference and the conversation's execution lease;
it does not eliminate the external requests. A webhook adapter could reduce
periodic requests after providing authenticated delivery and reconciliation.

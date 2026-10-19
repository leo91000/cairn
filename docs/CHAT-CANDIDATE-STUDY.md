# C3: conversation candidate selection experiment

Issue: [#264](https://github.com/leo91000/cairn/issues/264), part of #240.
Reference: main `293117d04e9d958aeed6a99d4cd8ff2650dc4f5b` (F22), including
F3 #317 and F8 #315. The issue and its comments were read on 2026-10-19.

**Decision: keep F22 and the current production selection.** No accepted user
latency or writer-occupancy budget was supplied. This limitation was recorded
before measurement in the raw conditions row. These experiments cannot establish
that a product budget is met or missed. They do not justify an index, dirty set,
new persistent state, or a production scheduler refactor. Reopen selection when
a budget and a representative native workload can discriminate those choices.

## Current source and contracts

`Service::chat_tick` still lists all chats in `records.updated_at DESC` order.
For active chats it reads the last run, publishes steering for queued/running
runs, excludes busy/recoveryPending runs, handles attention before pause, then
checks the wait, calls transactional `next_queued`, and considers a wakeup.
`next_queued` decodes the entire history and requeues `sending` messages.
F22's fresh read and transactional cancellation recheck avoid a writer call
when a failed/paused chat has no wakeup; they do not demonstrate SQLite saturation
or remove idle unpaused `next_queued` calls.

The production writer remains one Store actor; reader actors are separate.
The tick is notified and runs every second, independently of the 30-second
GitHub condition monitor. Retention is temporal, configurable and disableable;
it does not cap the chat count. ADR-0037 (durable wakeups), ADR-0038 (conditions),
ADR-0032 (bounded recovery), and the conversation movement ADR remain unchanged.

Manual resume uses `worker/control.rs::resume_authorized` and its `requeue`
transaction. Movement uses `nodes/moves.rs` and can set `recoveryPending` while
fencing/reserving the destination. This experiment does not duplicate those
paths. A candidate policy must preserve their interaction with the active-run
and recovery checks, including steering before the busy check.

There are no production Rust, web, Android, schema, index, wire-format, persisted
record, or epoch changes. Epoch 5 and reviewed legacy tree
`a6aae371f1d76cceb2f510180531f0ef17ec3e60` remain unchanged under the production
compatibility contract. No UI behavior changes; a Claude UI pass, Android
formatting and browser/device visual qualification are inapplicable.

## Discriminating prototype

`scripts/study-chat-candidates.py` is an opt-in Python/SQLite experiment. It first
reads all chats in existing order and the grouped set:

```sql
SELECT DISTINCT chat_id FROM chat_messages
WHERE json_extract(data,'$.status') IN ('queued','sending')
```

It retains per-chat run reads in both arms. A conservative snapshot filter keeps
pending-message IDs, object wakeups, queued/running or recoveryPending runs, and
chats needing attention (even paused chats without messages). It filters the chat
list; it never sorts by DISTINCT's output order. No candidate state or index is
persisted. The first experiment deliberately leaves run reads and the full chat
list in place, so its potential benefit is limited to avoiding idle histories
and empty writer transactions. It is not the proposed production API.

Pending IDs alone are insufficient: wakeups without messages, attention/pause,
and cancelled-question withdrawals on active runs all require work. Publishing
an empty steering array can clear stale input, so all active runs are retained.
Paused question answers must still flow. Admission, permission checks and
transactional rechecks must remain in the scheduler/launch path.

The Python tests assert specific expected actions and compare the reference and
filtered snapshot traces, preserving action and conversation order, across
11,200 combinations of run status, pause/lifecycle, queued/sending/follow-up/
steering/answer input, due/future/conditional wakeups, recovery/fresh-session/
deletion flags and busy state. The native `chat_candidates_study` test checks
the grouped SQL against actual `Db::messages`, existing chat order and equal-time
message order. Neither test proves a live filtered scheduler equivalent.

Important omissions: the model does not run Service admission, permissions,
snapshot preparation, native steering-file delivery, launch transactions,
condition probes, resume or movement. Conditional readiness is a supplied
observation. The production chats, chat_sleep, wait_for, store_notifications and
lifecycle suites only exercise the unchanged production scheduler. No claim is
made that they exercise this Python filter. Error behavior for corrupt JSON or
missing runs is also not qualified: a grouped scan can encounter errors outside
the history that the production tick would reach.

A grouped snapshot can predate committed input or a wakeup; an idle chat can
become actionable after it was filtered out. The model explicitly demonstrates
that limit. A production design needs race tests for notification delivery,
missed-wakeup bounds, stale attention/run state and launch rechecks. Full live
behavior/action/order equivalence is a prerequisite for adopting a filter,
not a conclusion of this study.

## Reproduce and interpret

Use the pinned project tools. The Python experiment only needs the standard
library. Run its model tests before measuring:

```sh
python3 -m unittest discover -s scripts/tests -p test_chat_candidates_study.py -v
C3_FIXTURES="$PWD/.data/c3-candidate-fixtures"
mkdir -p "$C3_FIXTURES"
printf '%s\n' 'C3 synthetic fixtures owned by this run' > "$C3_FIXTURES/.owner"
python3 scripts/study-chat-candidates.py --directory "$C3_FIXTURES" --output raw.jsonl
```

`C3_FIXTURES` must name a dedicated persistent directory. The experimental
resource guard checks at least 12 GiB available on its actual filesystem
(10 GiB production reserve plus 2 GiB margin); this is not a latency/occupancy
budget. `/tmp` in this VM is a 9.3 GiB tmpfs and was not used for fixtures.
The subprocess startup/shutdown deadlines are resource cleanup guards, not
product latency thresholds. No host, cache, swap, pin or profile changes are
needed. Run no heavy compiler, browser or device work alongside measurement.

The matrix is 100/1,000/5,000 chats × 20/200 delivered messages per chat ×
retention setting 0/7 days × synthetic writer off/on × plugin-record proxy
off/on: 48 configurations. Half the chats have a succeeded run, half no run.
All are unpaused and idle; this is an idle-history experiment, not a mixed
production workload. Setup and compilation are outside the timed operations.
The database uses the unchanged relevant table/index definitions from store.rs;
the script guards those definitions against source drift. Other production
tables/indexes, Store actors and async round trips are omitted.

Each configuration uses one warmup and six measured repetitions per policy and
connection mode, alternating policy order. Raw rows retain every repetition,
order, wall/process CPU time, decoded-message count and empty IMMEDIATE
transaction count/section time. That section includes lock wait and history
decode through COMMIT; it is **not measured writer occupancy**. No Store queue
or production occupancy measurement is claimed. Fresh-connection ticks exclude
opening/closing the connection; they do not evict the OS page cache. Fixture
creation and earlier reads already warm the host. All reads are local SQLite;
WAN, production network latency and provider IO are excluded.
Warm means a reused connection with normal SQLite caches, not a guarantee that
the entire corpus fits its page cache.

The experiment continues after individual SQLite errors to retain the rest of
the matrix. A successful process exit means the matrix was attempted, not that
every case produced valid samples. Inspect `case_error` rows and the precise
completed/failed/omitted inventory. Failed cases are excluded from comparisons;
any completed samples preceding their error remain in the raw file. The SQLite
busy timeout remains five seconds; it was not raised to obtain green cases.

The interfering subprocess performs one autocommit-equivalent IMMEDIATE write
then waits 10 ms between writes. Its achieved rate is load-dependent; raw write
latencies and subprocess CPU are separate from the measured reader process CPU.
It uses another SQLite connection, **not** the single production Store writer
queue. This exercises lock interference, not production actor occupancy. The
writer samples span both arms and are not attributed to an individual arm; no
per-policy writer occupancy or achieved write rate is derived. The
plugin proxy updates a 1 KiB synthetic plugin-instance row instead of a small
KV row; no plugin runtime, transport, external network or real secrets run.
Retention is stored as enabled/disabled configuration, with **no cleanup sweep**
and no claimed retention performance effect. The two settings are independent
repetitions on the same corpus shape.

Encoded UTF-8 chat/run/message bytes and post-checkpoint database bytes are
recorded per configuration. Both policies read the same file and format; there
is no encoded-size change. Separate unchanged-chat UPDATE wall/CPU samples
provide a local write reference; they are not full Service writes. Raw concurrent
write samples include lock wait. No causal claim should be drawn from one case,
a case ordering effect, or compilation timing.

An unchanged UPDATE can avoid physical page changes. The interfering writes
change KV data or record metadata, whereas the separate unchanged-chat samples
do not establish durable page-write latency. Neither includes all Store actor,
notification or Service work. The grouped policy still scans every message JSON
and reads every active chat's run; it does not establish sublinear tick work.

## Results and validation

These are **Python SQL/model timings, not native `Service::chat_tick` timings**.
The six comparable reference points below use retention setting 0, writer off
and plugin proxy off. Each cell is a median of six recorded samples; both arms
read the same database. The full configuration inventory and every repetition
are retained in the private raw handoff, including any incomplete/failed cases.

Local host: 2026-10-19, Linux 6.12.109 x86-64 KVM, seven Intel Xeon 2.80 GHz
vCPUs, about 18.6 GiB RAM, persistent local overlay filesystem. Python 3.14.7,
SQLite 3.53.1. No heavy compiler, browser or emulator ran during measurement.
Source export/import, credential scans and orchestration overlapped some trials;
there was no exclusive CPU reservation or controlled OS cache. Per-sample host
load attribution is unavailable. Compilation and fixture creation are excluded.

Measured script SHA-256: `dba988a497ec6f573994567613b123c56031108ec35a548bb71862029f48b808`.
The production reference is `293117d04e9d958aeed6a99d4cd8ff2650dc4f5b`;
the handoff verifies that this exact script is present in the final source tree.

| Chats | Messages/chat | Warm wall ms, reference → grouped | Fresh connection wall ms, reference → grouped | Warm process CPU ms, reference → grouped |
| ---: | ---: | ---: | ---: | ---: |
| 100 | 20 | 43.066 → 8.528 | 41.049 → 11.129 | 43.074 → 8.537 |
| 100 | 200 | 355.268 → 49.387 | 304.777 → 51.307 | 355.266 → 49.395 |
| 1,000 | 20 | 395.449 → 98.660 | 420.069 → 95.065 | 395.415 → 98.669 |
| 1,000 | 200 | 2105.400 → 504.752 | 1888.504 → 402.930 | 2104.651 → 504.760 |
| 5,000 | 20 | 1161.505 → 306.811 | 1482.479 → 413.115 | 1161.330 → 306.821 |
| 5,000 | 200 | 16859.028 → 3808.240 | 18839.152 → 3259.075 | 16858.945 → 3807.366 |

The representation is identical between policies. Encoded bytes below include
only synthetic chats/runs/messages; initial database bytes include the subset
schema/indexes and KV setting after a WAL checkpoint, not peak WAL growth.

| Chats | Messages/chat | Encoded chat/run/message bytes | Initial DB bytes | Unchanged UPDATE wall / CPU ms |
| ---: | ---: | ---: | ---: | ---: |
| 100 | 20 | 559,865 | 794,624 | 0.024 / 0.027 |
| 100 | 200 | 5,511,865 | 7,376,896 | 0.030 / 0.034 |
| 1,000 | 20 | 5,602,115 | 7,540,736 | 0.040 / 0.044 |
| 1,000 | 200 | 55,122,115 | 74,993,664 | 0.025 / 0.027 |
| 5,000 | 20 | 28,026,115 | 37,752,832 | 0.032 / 0.036 |
| 5,000 | 200 | 275,626,115 | 375,697,408 | 0.017 / 0.019 |

In this idle-only corpus, the reference materializes chats × messages message documents
and opens chats empty IMMEDIATE transactions per model pass. The grouped arm
materializes zero message documents and opens zero such transactions. It still scans
every message's JSON status and retains per-chat run lookups. This establishes
an operation-count difference in the model, not native speedup, writer saturation,
a met/missed product budget, or production acceptance.

The final pass entered all 48 configurations. Forty-four completed with all
24 timed samples (1,056 samples used for comparisons). Three recorded
`SQLITE_BUSY` (code 5): two before any timed sample completed and one after
19 partial samples. Those partial samples remain in the raw file and are
excluded from comparisons: 1,075 timed samples are retained in total.
The last configuration aborted without a final case row when its writer failed
at `BEGIN IMMEDIATE` and shutdown raised `BrokenPipeError`. All four unsuccessful
configurations have 5,000 chats × 200 messages and writer interference enabled
(retention 0/7, proxy off/on). Their logs and the exact unrecorded configuration
are retained; this is not full successful interference coverage. No retry or
timeout increase was used. The producer exited 1 naturally; a later stop attempt
found that it had already exited.

Earlier development attempts remain in the evidence archive and are excluded
from these comparisons: missing `/dev/shm` semaphores, an initial SQLite lock
failure with a passing small diagnostic, and an explicitly interrupted pass with the noncanonical message mode
`followUp` (corrected to `queue`). Their exact script hashes/sources and raw rows
are preserved. No timeout was increased and no error was relabelled as success.

The private handoff contains full raw JSONL, conditions, all green/red validation
logs, full source identity and the final independent review. Deterministic model
tests run in the quality workflow; performance measurements remain opt-in with
no timing threshold. Validation outcomes and precise completed/omitted coverage
are recorded with that handoff.

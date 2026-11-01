# Agent output telemetry and credential generations

Routine, strictly recognized Codex history-RPC declarations are counted before
entering the worker's canonical output queue. The parser accepts only the two
history methods and their complete fixed schema. Child request IDs, timing and
queue values are validated and discarded. The summary reports local declaration
counts and host-monotonic reader lifetime, not authoritative RPC success,
provider latency, run progress or model activity. Unknown, partial, oversized,
warning and other meaningful records keep the original fresh-redacted path.
`unsupported_records` counts performance-classified records rejected by this
schema; ordinary diagnostic lines are outside that counter.

The reader sends one cumulative fixed-size watch value at the existing cadence
and final EOF. Coalescing changes publication frequency, not counted coverage.
Only the worker admits summary work to the bounded diagnostic sink. It never
performs a Store lookup for a recognized metric, occupies canonical queue slots,
or creates a reporter task that can hold up canonical persistence. Dropping a
reader never fabricates EOF. Sink rejection counts saturate; summary delivery is
best effort, while canonical output retains its existing backpressure/order.

## Eligibility and active history

Aggregation is enabled only for a managed Codex **account** with a validated live
account lease and a ready credential observer. This is independent of the native,
VM or development-host execution backend. Unmanaged Codex, Claude and OpenCode
keep the legacy reader even when stderr forges the recognized Codex prefix.

Removing per-metric account reads must not remove historical redactions. Each
worker execution shares one append-only exact-value history with its active
account observer and outer failure publisher. It retains plugin/MCP values and
all observed account generations across account switches. It ends with the
execution; no history is persisted or stored in a process-global archive. Memory
is O(distinct observed values), plus in-flight snapshots/operations and active
subscriptions. There is no new total-memory quota, eviction or availability
fault. Fixed metrics/notifications do not imply constant secret-history memory.

The weak registry is owned by the shared Accounts instance. Registration happens
before the observer's initial Vault read and before broker/child start. Initial
snapshots merge, never replace, generations published concurrently. Every actual
Codex credential replacement publishes old/new access, refresh and ID tokens
before it queues the write. A guard is moved into the real SQLite closure and
returned as its result, surviving actual commit even if the awaiter is cancelled.
An already-admitted Store job is never cancelled or rolled back by scope cleanup.
A new subscriber also learns generations from still-live in-flight guards.

The private broker additionally publishes the exact selected response before its
first socket byte: selected B can coexist with current Vault C. It does not
reread C and pretend C was delivered. Account and attempt-scope checks reject new
admission after revoke. Registry, scope and history locks never span Store,
provider, socket or tracing I/O. A snapshot does not hold a lock during output
persistence. No secret container implements Debug/Serialize or emits values.

## Mutation ownership and supported topology

The targeted Codex writer covers capture after manager monitoring/refresh,
discovery, sign-in adoption/verification rollback, leftover session recovery and
host import; rollback deletion and forget's existing transaction are guarded as
well. The private manager auth file remains authoritative for recovery if a
session is interrupted before capture. Refresh/identity locks retain their
existing business contracts. Other Vault keys, OpenCode's set_in transaction and
Claude's private-file rotations do not opt into this observer.

README's one-manager-per-data-volume contract and manager.lock apply. Service
clones share the same Arc<Accounts>. Supported import, reconnect and recovery
paths participate; they are not waived as external writes. Administrator raw DB
edits bypassing that contract are not an active credential mutation API. A future
supported external writer or active-active topology needs its own trusted
publication protocol before it may enable this aggregation mode.

## Revoke and completion barrier

Revoke synchronously closes **new** admission. A response admitted earlier may
finish socket I/O or be stopped while its guard/history is still owned. The
broker NEVER aborts an admitted provider/credential operation. Its independent
owner retains the existing account lock through provider rotation, actual
Codex Session.close/process termination, capture and durable persistence. Stop
is cooperative before admission and in socket reads/writes only. A partial
socket write can stop without abandoning that earlier critical section.

Async close signals stop and joins this owner, so no further socket write is
possible after its completion barrier. If the close waiter or execution is
cancelled, Drop signals stop but does not synchronously join or abort: the
independent broker owner still completes the admitted critical section and
releases its account lock only afterwards. Immediate same-account refresh/home
reuse therefore remains serialized behind actual session termination and
capture. Already-queued Store jobs keep their independent commit guards; there
is no cancellation/rollback of their durable work. Normal legacy close now
waits for an ongoing operation rather than merely dropping its listener. This
can expose the existing provider RPC/filesystem/durability latency; no deadline
or timeout is increased and no bounded total close latency is promised.

Reader handles abort on drop; ordinary EOF is joined, while wait errors/start
failures abort and join both readers. All attempt preparation, wait and reconcile
results pass through scope/broker cleanup before returning. The outer failure
publisher then snapshots the same final history. No reader or broker may produce
additional frames or credential writes after its completed cleanup barrier.
Previously admitted fixed, nonsecret summary callbacks may drain later from the
bounded diagnostic sink.

## Regression and performance evidence

The privacy fixture uses independent, pairwise non-containing A/B/C access,
refresh and ID values. Real capture/broker refresh produces B; real manager
refresh produces C without a canonical B frame. It proves that the actual
redactor with A+C leaves B unchanged while A+B+C fully masks it. Actual readers,
worker wait, Store and outer record_failure then check canonical text/payload,
ordinary diagnostics, unknown performance and outer error separately. The outer
conflict is intentionally injected at the actual wait boundary; this is not a
provider/SQLite-failure cause reproduction. Legacy is a separate process control.

Ownership regressions pause an actual local Claude provider exchange after its
validated response and before atomic persistence, and a real fixture Codex
manager session before its RPC response and during termination. Closing or
abandoning the close waiter must retain the account guard; an immediate refresh
must see the old PID gone before actual materialize/home reuse. Further tests
exercise partial socket delivery and the production attempt cleanup helper
followed by the real account-switch path. Private socket gates control phases;
they do not add product sleeps, deadlines or a replacement credential pipeline.

Additive isolated controls exercise the V2 channel -> Reports -> real bounded
sink/Capture route: one controlled final-only numeric summary, and the actual
worker wait loop's final cumulative 12,180-record/closed/EOF result. Intermediate
summary counts are deliberately not fixed. The historical direct reporter test
remains a separate regression; a new-route pass alone does not explain a missing
direct report. Optional test-owned atomic stage counters observe the existing
enabled gate, admission, callback and Capture decisions without another enabled
call, report dump, global collector or synchronous replacement sink. A flush is
a worker barrier, not evidence that a particular summary was admitted.

The original flood fixture exercises 12,180 recognized records and three actual
canonical records. Its controlled account-work delay uses virtual Tokio time;
that elapsed value is NOT physical production latency. Store-refresh counts,
publication order and privacy assertions are causal regressions, not a CPU or
network benchmark. Comparative physical CPU/wall/encoded-byte measurements need
identical input and host conditions against the feature base, which already
contains the SQLite optimization. Do not attribute that separate optimization to
this pipeline change or claim that the cumulative native log cap is repaired:
native capture occurs upstream, and canonical output dominates the independently
observed cap exhaustion. No native retention policy, timeout or buffer size is
changed here.

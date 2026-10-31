# Writer-owned run progress

Version 19 upgrades transactionally to version 20 by adding `run_progress`, one
small row per run with a cascading foreign key. Existing runs, checkpoints,
conversation data and timestamps are preserved. Existing executions have no
backfilled progress: historical events are not authoritatively bound to their
attempt. Missing evidence is unknown, never recovery.

The supervised launch binds a new record to its execution ID, runner ID (or full
local PID/start/boot identity), and chat message or mission turn. The adapter
consumer updates it in the same transaction as the corresponding event. Lease
renewals, checkpoint writes, diagnostics, retries and unchanged item snapshots
cannot advance its timestamp. Provider completion/failure enters a sticky
finalizing phase; neither a late item nor completed=false clears that phase.
Only a newly launched execution replaces it. Terminal status, process absence,
released reservations and even code zero do not certify settlement. Current native
controllers persist an optional `PhysicalFinalized` marker only after their
workload and physical cleanup complete. The typed client forwards that proof
without interleaving provider output; the writer records its attempt-bound
`finalizedAt`. A confirmed nonzero/SIGTERM result can resolve no-progress, while
failed flushes remain unhealthy even without a progress row, and missing
certificates remain unknown.

Digests cover the supported semantic item fields, excluding duration and transport
metadata. They remain private to the installation. At 128 distinct items the
record stops certifying complete coverage for that execution rather than evicting
old identities and accepting replays as new work. This conservative limit can
leave long turns unknown. The monitor reports a quiet tool as advisory and retains
existing incidents; an open tool is not recovery evidence. Plans and initial
delegated activity use stable semantic fields; spawn-tool
completion does not end a still-running child. Duration/token/tool-use counters
and activity notices do not refresh it. Redaction or preview changes in semantic fields cannot manufacture progress.
Unsupported or truncated items, missing item identities, provisional identity
folding without replay-safe aliases, unbound background waits and reused child
IDs without a child-run identity make
coverage explicitly unknown until a fresh execution. Relay gaps and malformed
or truncated stdout retain the existing durable `outputInterrupted` flag; that
flag also prevents provider-health certification until a fresh launch. They cannot leave the
previous provider state falsely complete.

Updates of the small row are throttled to one second; phase/coverage transitions
publish immediately. Unpublished digest changes remain in memory only after the
event commits and are also persisted by the existing checkpoint heartbeat. A
crash can lose recent evidence and cause an older timestamp, never manufacture
fresh progress. No historical event scan or new timer is involved. The safe Node
SQL helper projects only identities, classifications and times through keyed
lookups; it excludes payloads, text, command arguments, outputs and digests.

The version ratchet deliberately prevents an old manager from opening a database
whose progress writer contract it cannot maintain. As with other installation
migrations, stop the old manager and retain a consistent database/WAL backup
before upgrading. Use a forward fix or an explicitly compatible recovery image;
rolling back the manager binary after version 20 is unsupported. Nodes and guests
do not open this database. Current native nodes emit an optional completion
certificate;
older clients ignore the extra JSON field and older nodes retain their existing
behavior. A new manager cannot certify old-node finalization from a legacy exit
marker, including code zero. A bound progress row on a new manager does not
change this: terminal or registered-wait outcomes without physical proof remain
unknown, preserving existing incidents; explicit failed flushes remain unhealthy.
Local shared-process development has no controller certificate and conservatively
retains unknown terminal finalization. VM completion remains unknown: its current
best-effort shutdown discards
retirement/accounting errors, so a guest exit cannot produce a physical certificate.
VM and guest execution/recovery behavior do not change. Upgrade matching
manager/native-node versions to obtain the new evidence.
A collector on a version-19 manager remains compatible via the helper's missing
`run_progress` fallback and reports unknown. This PR does not deploy software or
apply a migration to any running installation.

A writer-owned row avoids cloning/serializing a potentially large prepared
workspace on every output snapshot. A checkpoint-only alternative would either
add that cost or need the same throttling with more bytes per write. A collector
backfill cannot prove old event ownership, so it is insufficient to certify real
recovery; the conservative compatibility path remains available for legacy runs.

## Reproducible local SQL measurements

Run `node scripts/measure-run-progress.mjs`. This creates and removes its own
SQLite fixture, with the source schemas, 100,000 diagnostics, 128 item digests,
a 256 KiB prepared workspace, 1,000 reads and 60 separately committed writes.
It prints encoded bytes, query plans, wall time, CPU and WAL bytes. Local Linux,
Node 24.22.1 / SQLite 3.53.4, i9-14900K, filesystem WAL with synchronous=FULL,
warm OS cache: baseline identity/latest-event reads averaged 0.067 ms versus
0.080 ms with the progress projection; a reopened connection read took 0.306 ms
with warm OS cache. All candidate plans use primary-key seeks, independent of
the diagnostics tail. The worst-case progress row was 23,142 encoded bytes,
versus 262,207 bytes for the checkpoint fixture. At equal update counts the small
row averaged 0.017 ms/write versus 0.264 ms/write for the large checkpoint;
CPU totals for 60 writes were 1.05 ms and 15.75 ms respectively.

This is a local SQL comparison, not Rust classifier throughput or production
network latency. The original writer does **not** rewrite its checkpoint on every
output event: it already has a heartbeat. The candidate adds progress-row writes
at most once per second plus phase/coverage transitions, and persists current
progress with that existing heartbeat. The comparison to the checkpoint-only
alternative uses equal update counts, not an invented original event-write cost.

# Fresh sessions after a conversation restoration

A failed or interrupted restored run can offer a fresh native agent session.
The installation derives `canRequestNewSession` from `run.restoredAt`, with
`chat.restoredAt` as a historical fallback, and an active chat without an already
pending request. Web and Android consume that availability. The historical offer
lifetime is preserved: a successful turn hides it, but a later failure can offer
it again while the restoration marker remains. This change introduces no expiry.

Confirmation clears the run's native session and resume availability, pauses the
queue, clears its durable wakeup and records `chat.sessionRestartRequested`.
A repeated confirmation returns the pending intention, so a lost HTTP response
can be retried. The handler never reads or rewrites the worker's checkpoint.
The next queued message, after the user resumes, consumes that intention in
`continue_run` before `launch_turn` clears it in the same transaction. Continuation also reads the
legacy checkpoint `freshSession` flag. Both paths transfer the visible transcript,
reset launch/controller recovery state, and preserve the prepared workspace.
A subsequent ordinary message resumes the newly established session normally.

The regression forces a real live `Checkpoint::persist` after the request on an
isolated temporary database. This is a non-VM, forced interleaving, not evidence
of a production incident or its frequency. Continuation coverage separately
checks isolated and nonisolated prepared metadata.

## Compatibility

The installation compatibility epoch advances from **4 to 5** under
[the production contract](PRODUCTION-CAIRN.md#installation-data-compatibility).
The record fields/types, schema, codec, envelopes and disk objects do not change,
but the new writer deliberately relies on the chat intention without setting the
checkpoint marker understood by an old reader. The epoch fences that change of
persisted meaning. The reviewed legacy tree stays unchanged. The computed API
availability is not persisted. F6 already established epoch 4 for plugin setting
constraints. Its validation, package repair and failure recovery contract in
[Setting constraints and compatibility](PLUGINS.md#setting-constraints-and-compatibility-f6)
still applies; this independent boundary does not replace that transition.

The previous epoch-4 continuation (`98da98bd`, including F6) ignores a newly
pending chat intention unless the legacy checkpoint marker is also set. It then
clears the intention without handing off
the transcript or resetting launch state. With a launched checkpoint and a cleared
native session ID, Claude and OpenCode recovery fails; Codex can rediscover the
previous session or fail. Parseable JSON is insufficient to preserve this request.

### Forward transition

The normal compatibility check must stop automatic rollout across epochs 4 and 5.
A separately reviewed transition is required before a release; this PR does not
perform a release or approve deployment. Stop the previous manager before starting
the epoch-5 binary so the two reader/writer contracts cannot operate concurrently.

No eager SQL migration or storage-object rewrite is needed. Existing chat
intentions and legacy checkpoint markers are left intact. The new reader accepts
both, and the next user-approved continuation transfers the transcript and resets
the native launch state before clearing the intention in the same transaction.
Pause, queued messages, wakeup cancellation and prepared workspace remain intact.
An interrupted transaction commits neither the restart nor its consumption;
restart reads the same pending records and safely attempts the continuation again.

### Failure recovery

Stop rollout and preserve the installation directory, private identity, chat/run
records, checkpoints and workspace data. Use an epoch-5 forward fix if startup or
continuation fails. Do not start an epoch-4 binary against data written under this
contract, and do not change the epoch or legacy tree to bypass the check.

If an older binary has already consumed a request, stop further continuation and
inspect the preserved state before repair. Returning to the new binary cannot
reconstruct an intention already erased by the old reader. No automatic backup,
rollback conversion or data restoration is introduced by this change.

## Local metadata measurements

2026-10-18, Linux 6.12.109 x86_64 guest, 12 vCPUs reporting Intel Xeon, 30.9 GiB
guest-visible RAM (32 GiB configured), about 29 GiB available during this probe,
local overlay filesystem.
No concurrent build or emulator ran during measured calls. Rust 1.97.1, default
unoptimized dev profile with debug info and incremental compilation enabled.
SQLite WAL and the real
`Store`/`Service` functions; no agent process, production data, HTTP or S3 traffic.
These are local metadata costs, not production network latency or object
compression results. See [object compression measurements](STORAGE-COMPRESSION.md)
for the separate disk-object workload and format.

Reproduce with the checked-in probe (the reference handler is unchanged through
`98da98bd`, the main delivered immediately before F2):

```sh
CARGO_BUILD_JOBS=2 cargo run --locked \
  -p cairn-installation --example session_restart_measure
```

Each variant uses its own temporary database and the same fixed slug, historical
chat restoration, failed run and checkpoint with 16 KiB synthetic padding. For
1,000 sequential requests it resets the three input records outside the measured
transaction, then measures the handler through commit. Reads load/decode all
three resulting records: 1,000 warm reads and 30 first reads from newly opened
stores. The latter are cold SQLite connections, **not cold OS/storage caches**;
store opening/schema work is excluded. Reference runs first, candidate second;
this is one noisy run, not a speedup estimate.

| Measurement | Reference handler | Durable intent |
| --- | ---: | ---: |
| Encoded chat / run / checkpoint JSON bytes after request | 126 / 117 / 16,513 | 126 / 117 / 16,517 |
| Request write median / p95, µs | 429.846 / 558.852 | 92.073 / 113.586 |
| Warm read median / p95, µs | 89.428 / 166.935 | 91.364 / 177.997 |
| Cold connection read median / p95, µs | 505.901 / 556.108 | 483.733 / 533.918 |
| Process user + system CPU during 1,000 handler calls | 0.45 s | 0.09 s |

CPU is summed from `/proc/self/stat` at 100 ticks/s around each call; it includes
process activity and has tick quantization. JSON bytes exclude SQLite page/WAL
overhead. The candidate leaves the 16,517-byte checkpoint untouched instead of
rewriting it; the reference adds `freshSession` and removes controller recovery
state immediately. Different resulting checkpoint sizes are expected. Reads are
unchanged in kind and their noisy timings do not establish a read regression.

The private delivery checkpoint retains the raw rows, reference commit, build
profile, workload and host conditions, following the repository rule that raw
benchmark samples stay outside the source tree.
These measurements were rerun for this patch; historical F2 probe results are not
validation of this state.

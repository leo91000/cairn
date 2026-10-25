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

The reviewed [automatic transition to epoch 9](AUTOMATIC-MIGRATIONS.md) includes this
epoch-4 → 5 boundary. It qualifies the restart regressions and enforces the database
rollback fence. The installation updater stops the previous manager before starting
the candidate so the two reader/writer contracts cannot operate concurrently.

No eager SQL migration or storage-object rewrite is needed. Existing chat
intentions and legacy checkpoint markers are left intact. The new reader accepts
both, and the next user-approved continuation transfers the transcript and resets
the native launch state before clearing the intention in the same transaction.
Pause, queued messages, wakeup cancellation and prepared workspace remain intact.
An interrupted transaction commits neither the restart nor its consumption;
restart reads the same pending records and safely attempts the continuation again.

### Failure recovery

Stop rollout and preserve the installation directory, private identity, chat/run
records, checkpoints and workspace data. Use a forward fix compatible with the
installation's current data epoch (epoch 5 for F2; native execution advances it to
epoch 6) if startup or continuation fails. Do not start an epoch-4 binary against
data written under this
contract, and do not change the epoch or legacy tree to bypass the check.

If an older binary has already consumed a request, stop further continuation and
inspect the preserved state before repair. Returning to the new binary cannot
reconstruct an intention already erased by the old reader. No automatic backup,
rollback conversion or data restoration is introduced by this change.

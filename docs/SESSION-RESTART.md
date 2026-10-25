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
checkpoint `freshSession` flag used by explicit backend migration. Both paths transfer the visible transcript,
reset launch/controller recovery state, and preserve the prepared workspace.
A subsequent ordinary message resumes the newly established session normally.

The regression forces a real live `Checkpoint::persist` after the request on an
isolated temporary database. This is a non-VM, forced interleaving, not evidence
of a production incident or its frequency. Continuation coverage separately
checks isolated and nonisolated prepared metadata.

# Durable mission waits

Missions and conversations share `waits`: input validation, idempotency, run dependency
cycle detection, authorization and plugin observation. The owner stores the wakeup
on the conversation or the mission run. Existing conversation behavior is unchanged.

```text
running --successful turn with wakeup--> waiting
waiting --time/result/timeout--> queued --> running (same session and workspace)
running/waiting --stop--> cancelled
running --terminal failure--> failed (wait cleared)
```

`waiting` holds the task's unique active slot but no worker process/account slot.
The worker consumes a wakeup only after its preceding execution has finished
release and fencing. Restart recovery preserves it and finishes any outstanding
execution cleanup before the wakeup deadline, including when the wait is cancelled.
Consumption and checkpoint
reset commit together; the remaining execution budget is retained. A unique
`missionTurnId` fences MCP grants, artifact/outcome writes and exclusivity against
the previous turn, and identifies a new provider turn instead of replaying its
completed predecessor. Wakeup observations are data, never additional authority.

The run APIs and both installation tool interfaces expose the stored `wakeup`
and `waiting` state. Existing cancellation handles the wait. Web/Android show a
live wait card and stop action. Mission schedule pause affects future runs only.
Mission waits and explicit node moves exclude each other transactionally. Finish
the current move before registering a wait, or finish the wait before requesting
a move; a rejected move releases its provisional destination reservation. Recovery
may still relocate an interrupted, unfinished turn that registered a wait before
it stopped; this preserves that wait until the turn completes successfully.

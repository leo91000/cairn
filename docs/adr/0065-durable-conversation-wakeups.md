# Durable conversation wakeups

Date: 2026-10-12.

Updated: 2026-10-17 — follow-ups queued before the wait no longer prevent it;
they start after its continuation.

An agent saying it will return after a delay does not schedule another turn.
Native waiting tools depend on their process staying alive. Cairn must own the
deadline when the conversation can release its execution while awaiting a remote
result.

`cairn_workspace.sleep` records one wakeup on the conversation, bound to the
current turn's workspace grant. Its seconds, reason, deadline and unique
continuation ID persist together with the activity event. Identical repeated
calls return the original deadline; changed arguments replace it. The agent
ends its turn after confirmation. This is cooperative deferral, not a forced
interrupt of arbitrary commands. Input the turn has not handled yet (steering,
question answers or a message being delivered) prevents scheduling. Follow-ups
queued for after the current turn do not: the wait continues the current task,
so they start once its continuation finishes.

The existing worker wakes a conversation after its turn succeeds, its execution
is released and its deadline passes. Snapshot preparation rechecks the current
agent permissions; the launch transaction rechecks the wakeup, run, lifecycle,
pause and unhandled input. It consumes the wakeup atomically with queuing a
fresh turn of the same run. Native session, workspace and transcript survive;
no synthetic user message is added to the chat. Capacity, account admission,
fencing, publication protection and bounded recovery retain their contracts.

A new user message cancels the wakeup; earlier queued follow-ups then start in
order. A wakeup that can no longer resume does not hold them. Queue pause holds
the original deadline; resume makes an overdue wakeup eligible. Stop, run
cancellation, deletion and a fresh session cancel it. A terminal failure pauses
the chat and clears its wakeup. Restart reloads the persisted deadline,
including overdue waits, without resetting it or relying on guest timers. Resumption is no earlier than the
deadline and depends on available capacity.

Waiting releases the normal execution/account/MCP leases. Claude's VM can stop;
Codex may use the existing bounded CPU-paused retention policy (ADR-0020), whose
idle RAM and physical slot are reclaimed by normal eviction/admission. We do not
add RAM snapshots or change disk publication. Native local background work must
use its own wait mechanism: stopping or pausing its VM would stop that work too.

Web and Android show the reason and local wake time, expose pause and stop, and
return to ordinary working/account/capacity states when the continuation queues.
Agent instructions forbid promising a return without a confirmed wakeup and
distinguish deferral from completion or requesting user input.

Validation covers grant expiry, argument validation, idempotency, persisted
deadlines, restart, fencing, one-shot consumption, user input, pause/resume,
cancellation, trash/restore and failure. A worker journey uses the native fixture
transport to verify automatic continuation with the same session and working
files, and other work executing during the wait. Browser and Android journeys
check the waiting presentation independently of native background tasks.

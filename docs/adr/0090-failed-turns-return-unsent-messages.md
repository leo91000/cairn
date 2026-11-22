# Failed turns return unsent messages

Date: 2026-11-20. Updates the queue behaviour after a failed or stopped turn in
ADR-0063 and ADR-0065; the bounded automatic retry policy is unchanged.

## Context

A turn that did not succeed paused its conversation. The message that started it
and any follow-up written meanwhile stayed in a paused queue, and the only way on
was to resume: resuming the queue alone paused it again on the next tick, so the
failed turn had to be replayed before anything else could be said. People read
this as a conversation they must repair rather than one they can keep writing in.

## Decision

The dispatcher settles a turn that failed, was interrupted or was stopped once.
It returns the ordinary messages a person wrote that the agent certainly never
received: the turn's own message when no attempt launched a process for that turn
(for example, admission or the controller refused it), steering messages sent to
such a turn, and queued messages written before the turn ended or before Stop was
pressed. Steering sent to a turn whose process launched may have been read, so it
is held. Steering is claimed only once a process runs: a run waiting for
admission, an account or an automatic retry leaves it queued. Returned messages come back in order; a resent message follows any held
message that waited behind it. They become `cancelled` and name the person
they go back to (`returnedTo`). Everything else is held behind a pause, queued
again, as every failure did before: messages from rules and agents, which have no
composer to go back to, questionnaire answers, and a turn message the agent may
have received. A failed compaction returns nothing: a person's messages written meanwhile follow it,
and those of rules and agents are held. When nothing is held, the
conversation is not paused, and the next message starts a new turn on the same
run, session and workspace with a new automatic retry budget. Settling happens
once per turn, so a person's later resumption is never undone.

Web and Android restore returned messages only in the composer of the person they
name, in the conversation shown, ahead of any draft, with their attachments, and
withdraw them so another device does not restore them again. They keep the message's plan
request and agent choices. Only the person a message was returned to may withdraw
it (`delete_chat_message`), from the apps or the public MCP they authorized; agents
cannot. A restored draft is as durable as any other draft. Conversation views carry
`turnReturned`, which the apps use to stop offering recovery for that turn.

A turn whose own message was returned is over: resuming or retrying it is
refused, unpausing does not replay it, and its coding agent is no longer offered
for a switch, because that would deliver a message that is back in its author's
composer. A turn whose message the agent may have received can still be resumed as
before: in the apps through the held queue's Resume, the quota card or the
OpenCode retry, and for agents through `set_chat_paused` and `resume_chat`. After a
Stop or a failure with nothing held, neither app offers to resume: the next
message continues the conversation. Unpausing replays a stopped turn whose message is held, as a
recovery that tells the agent to check what it already did, and messages written
since follow it. When no message from before the turn ended is held, a message
written after the turn ended is sent instead of a replay. A queue that holds messages says
so: the apps ask to resume the queue rather than to send a message.

Explicit pauses keep their meaning: a person's pause, Stop, a rule's cancellation
and a requested fresh session still pause the conversation, and rules still cannot
lift them. After a failed or interrupted turn, rules also wait: they can steer or
answer again once its next turn starts, which a person does by writing or by
resuming it. Cancelling a conversation's run or its scheduled wakeup pauses it in the
same transaction, as Stop does, whether from the conversation or through `cancel_run`.
In the apps, a person's next message lifts that pause; public MCP and agent callers send
the message, then call `set_chat_paused(false)`. Without a pause, any caller who may send
continues the conversation, after a failure as after a success. The launch of a turn is
recorded once its process starts: a manager crash in between can return a message the
process already read. When the last turn
failed or was stopped and the queue is empty, the next message a person sends
resumes it. A queue paused after a successful turn, or holding messages, keeps
holding what is added to it.

Conversation failure messages carry no call to action: the returned message in
the composer, or the held queue and its Resume, shows what continues the
conversation. Missions keep their resume wording. A preparation failure, which happens before any turn exists
(reduced agent access, an invalid configuration), still pauses the conversation
and keeps its message: it needs a repair, which resending would not provide.

While the agent works, the composer sends into its turn (steer) by default.
Queueing for the next turn is the secondary action (Alt+Enter on web). A message
that changes the coding agent, model, reasoning or speed, or requests a plan
during a turn that can change files, is queued, as the installation requires.
Saving an edit keeps the queued message's mode.

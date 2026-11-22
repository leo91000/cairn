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

A turn that failed, was interrupted or was stopped no longer pauses its
conversation on failure. The dispatcher returns the ordinary messages a person
wrote that the turn never delivered: its own message when the agent never
acknowledged it, and messages written before the turn ended or before Stop was
pressed. They become `cancelled` and name the person they go back to
(`returnedTo`). Questionnaire answers are never returned. Messages written by rules
or agents have no composer to return to: when a failure leaves any, the
conversation pauses as every failure did before, and they wait for someone to
resume it. A failed compaction returns nothing. Messages written
after the turn ended start the next turn on the same run, session and workspace,
with a new automatic retry budget.

Web and Android restore returned messages only in the composer of the person they
name, ahead of any draft, with their attachments, and withdraw them so another
device does not restore them again. Withdrawal of a returned message is accepted
for that reason, including through public MCP and `cairn_installation`
(`delete_chat_message`).

A turn whose own message was returned is over: resuming it, retrying it, unpausing
to replay it or switching its coding agent is refused or skipped, because that
would deliver a message that is back in its author's composer. A turn whose
message the agent acknowledged can still be resumed as before.

Explicit pauses keep their meaning: a person's pause, Stop, a rule's cancellation
and a requested fresh session still pause the conversation, and rules still cannot
lift them. Rules also wait after a turn that failed or stopped: they can steer the
conversation again once a person writes in it. When the last turn failed or was
stopped and the queue is empty, the next message a person sends resumes it. A
queue paused after a successful turn, or holding messages, keeps holding what is
added to it. When a message is waiting, resuming sends it rather than replaying
the stopped turn.

While the agent works, the composer sends into its turn (steer) by default.
Queueing for the next turn is the secondary action (Alt+Enter on web). A message
that changes the coding agent, model, reasoning or speed, or requests a plan
during a turn that can change files, is queued, as the installation requires.
Saving an edit keeps the queued message's mode.

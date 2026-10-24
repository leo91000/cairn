# Delegated tasks across coding agents and models

Date: 2026-10-23.

## Context

An agent sometimes wants a second model on part of its work: an independent
review by another vendor, a test sweep on a cheaper model, research in parallel.
Native subagents stay inside one coding agent and one vendor. The installation
access tool can already create a conversation on any coding agent and wait for
its run, but it delegates the whole management catalogue, records no link
between the two conversations and leaves the requester to stitch five calls
together.

## Decision

`access.delegation` is an opt-in agent power, edited with installation management
in Elevated rights and reviewed like it. It gives a conversation turn three
`cairn_workspace` tools, without installation management:

- `delegate_task` creates an ordinary conversation, records
  `requestedBy { runId, chatId, requestId }` on it and queues the task as its
  first message on the chosen coding agent, model and reasoning. The engine is
  validated when the task is delegated, so a bad choice fails the call.
- `task_status` reads the requester's tasks: state, outcome, pending questions
  and the final report (bounded; the conversation keeps the whole reply).
- `task_cancel` stops a task as Stop does.

`wait_for` gains the native `task_finished { chatId }` condition. Like
`run_finished`, it is polled without the LLM. It completes once the task is idle,
not merely when its turn ends, or once it is paused or deleted.
Before its first run, a task with queued input shows as queued. Withdrawing that
input leaves an empty idle task: it completes the wait and releases its active
slot, and both clients show it as stopped.
Task status and wait observations include the paused state and any preparation
or execution error, so a failed start does not look like an empty successful
report. Deleted tasks no longer expose these errors.

Delegated work never widens access. A task runs as the requesting agent, or as
another agent whose access the requester's current access covers (`covers`, the
rule that already guards queued work), with an additional ceiling on execution
nodes, plugin instances and exclusive node reservations at creation. Thereafter
each ordinary conversation uses its own agent's current grants; native work also
retains the parent's revocable host authority. Native administrator admission
reviews are part of that ceiling: a reviewed target cannot grant an unreviewed
requester administrator access, and removing the requester's review revokes its
delegated children's admission on that node. Turning delegation off is a reduction:
live grants fail and queued work stops as for other powers. A delegated
conversation cannot delegate further, a requester has at most five working tasks,
and the requester's run is the native-policy origin of what it creates.
Native lineage records `nativeParentAccess: "delegation"` for these parent edges;
management-created and historical edges still require installation access. Both
the saved and current parent permissions, caller authority and node ceiling are
checked at admission and renewal.

The requester's history records a `chat.delegated` event where the task started.
Clients show these as the existing subagent team card, followed through the
delegated conversation for its live state; opening a task opens its conversation.
The delegated conversation names its requester (`requester { chatId, title }`,
while that conversation still exists) in the Fil and in its header. Public MCP and
`cairn_installation` list the same lineage with conversations.

Each task gets its own workspace. Sharing the requester's workspace would let two
coding agents own one disk; a task starts from what the requester pushed.

## Consequences

Owners decide per agent whether it may start conversations on other models. Each
task is a normal conversation: a person can open it, answer its questions,
continue it or delete it. Requesters must state self-contained tasks and push the
work a task reads. Agents that need a person's answer still route it through the
person: requesters cannot answer their tasks' questions yet.

The persisted lineage fields are additive and the data compatibility epoch stays
unchanged: older releases can read the records, and new releases treat an absent
`nativeParentAccess` as installation-management provenance. Older releases cannot
run a native child authorized only by the new delegation permission; they deny it
under their existing installation-access requirement.

# Side questions use each engine's native context, beside the running turn

Users want to ask "why did you do that?" while an agent works, without steering it,
queuing a turn or polluting the conversation. Three designs were measured against
Codex 0.164.0 and Claude Code 2.1.280 with synthetic model servers:

1. A separate Cairn-side model call over the visible transcript, like title
   generation. It never touches the running process, but it does not see the
   engine's own context (tool results, the step in progress), would answer Claude
   conversations with a Codex model, and duplicates what both engines now offer.
2. A second coding-agent session resumed from the saved session. It can start with
   write access and tools, races the running turn's transcript, and is a worktree-less
   copy of the agent rather than an explanation.
3. The engine's native side mechanism inside the running process. Claude Code's
   stream-json control protocol accepts `side_question` (the same path as `/btw`):
   it answers from the live messages, including the unfinished tool call, denies every
   tool, runs one turn and skips its transcript; `control_cancel_request` withdraws it.
   Codex's app-server forks the running thread with `thread/fork` (`ephemeral`), which
   is how its own `/side` works; the fork can be restricted by sandbox and feature
   overrides and is never written to the conversation's rollout.

We chose 3. Codex 0.164 ignores `developerInstructions` on a fork, so the boundary is
injected with `thread/inject_items`, and it keeps a fork's subagent tools regardless of
`features.multi_agent`. Cairn therefore interrupts a fork when any tool item starts and
reports the attempt; a subagent spawned without that guard inherited the fork's
read-only sandbox and had no shell. OpenCode is left unsupported rather than emulated.

Side questions travel in the existing chat inbox, so microVM and native-node transports
carry them unchanged. Only adapters that announce `chat.side.available` receive them:
an adapter from an earlier release, possibly on a node not yet updated, would otherwise
deliver the question as steering. A finished turn keeps its process for up to 90 seconds
to deliver answers in progress. Side questions are additive installation records
(`chat-side-question:{chatId}:{id}`) removed with their conversation; earlier releases
ignore them, so the data compatibility epoch is unchanged.

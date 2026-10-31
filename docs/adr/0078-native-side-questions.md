# Side questions use Claude Code's native side mechanism; Codex stays unavailable

Users want to ask "why did you do that?" while an agent works, without steering it,
queuing a turn or polluting the conversation. The answer must come from the engine's
own context. Claude denies requested tools, with the managed-hook limitation below. Three designs were measured against the engines
Cairn ships (Codex 0.164.0 then 0.166.0, Claude Code 2.1.280 then 2.1.308) with
synthetic model servers and no account:

1. A separate Cairn-side model call over the visible transcript, like title
   generation. It never touches the running process, but it does not see the
   engine's own context (tool results, the step in progress), and would answer Claude
   conversations with another model.
2. A second coding-agent session resumed from the saved session. It can start with
   write access and tools, races the running turn's transcript, and is a worktree-less
   copy of the agent rather than an explanation.
3. The engine's native side mechanism inside the running process.

We chose 3 for the tested Claude release, where the engine denies requested tools; operator-managed hooks can still run before denial as described below.

Claude Code's stream-json control protocol accepts `side_question`, the path of `/btw`.
Its implementation runs one forked turn from the live messages with a `canUseTool` that
denies every tool, `maxTurns: 1` and no transcript write; `control_cancel_request`
withdraws it. A probe with Cairn's launch flags confirmed it: a side reply calling
`Bash` reached no host permission request, created nothing, returned a synthetic
"tried to call Bash" answer, left the session file unchanged, and the main turn
finished. The same held with `--dangerously-skip-permissions`. Claude Code still runs
`PreToolUse` hooks before that denial. Cairn launches it with no setting sources and
no hooks, and the probe saw no user or project hook run; only an operator's
managed-policy hooks could. The adapter announces side questions only to the exact release
2.1.308, the one probed (as reported by its `system/init` `claude_code_version`);
other releases require qualification before they can receive them. A question about a whole reply carries
the reply's first 2,000 characters, so the forked turn knows which reply it concerns.

Codex's app-server forks the running thread with `thread/fork` (`ephemeral`), which is
how its own `/side` works. Cairn cannot make that fork tool-less:

- Fork overrides are partly ignored. With a gpt-6 model, the fork keeps code-mode
  `exec` with nested `exec_command` and `apply_patch` despite `features.code_mode`,
  `features.shell_tool` and `features.unified_exec` set to false. With
  `features.multi_agent_v2`, it keeps `spawn_agent` and the other collaboration tools;
  `agents.max_depth` and `agents.max_threads` do not stop a spawn.
- `developerInstructions` is ignored on fork, so only a prompt could ask the model not
  to use those tools.
- Interrupting the fork when a tool item starts does not prevent the tool's effect. An
  MCP tool ran within the millisecond of its `item/started`, before the interrupt was
  acknowledged, and a spawned subagent sent its first model request after the
  interrupt was acknowledged.

Codex and OpenCode side questions are therefore reported unavailable in the UI and the
tools, with the reason, until an engine release offers a fork whose tools Cairn can
remove. A read-only sandbox, `approvalPolicy: never` and `mcp_servers: {}` did refuse
patches and remove MCP servers in the fork, but they do not cover the tools above.

Side questions travel in the existing chat inbox, so microVM and native-node transports
carry them unchanged. Only a running Claude Code execution whose adapter announced
`chat.side.available` receives them: an adapter from an earlier release, possibly on a
node not yet updated, would otherwise deliver the question as steering. Each attempt to
run it (a launch, a retry, a recovery or a resume on another account) clears that
announcement and the side questions in its inbox, so its adapter receives them only
once it announces again. A side question belongs to the turn it was asked during and
fails once that turn no longer runs. Side questions never hold the reply: when it ends, Claude Code's
requests still in progress are withdrawn and reported unanswered, and the turn completes
exactly as without them. Answers use the running turn's account and are redacted like
the rest of its output before they are stored. Side questions are additive key-value
records (`chat-side-question:{chatId}:{id}`), with a small
`chat-side-question-open:{chatId}:{id}` marker while the reply may still receive the
question or its withdrawal, so the chat tick and conversation lists never read past
answers. Both are removed with their conversation; earlier releases ignore them. Database version 22 adds a partial index for history turn
boundaries; older binaries reject the upgraded database.

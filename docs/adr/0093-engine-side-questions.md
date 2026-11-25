# Side questions use each engine's own side mechanism, qualified per release

ADR-0081 offered side questions only on the one Claude Code release it probed (2.1.308)
and never on Codex, whose `thread/fork` keeps tools Cairn cannot remove. Shipping Claude
Code 2.1.311 then silently turned side questions off for every Claude conversation,
leaving Antigravity the only engine that offered them.

The requirement stays the same: a side question is answered from the engine's own
context, never runs a tool, never steers the turn and never enters the conversation's
history. What changes is how Cairn meets it:

- **Use the engine's own mechanism.** Cairn does not build its own side conversation
  around an engine that offers one. Claude Code keeps its `side_question` control
  request (`/btw`). Codex 0.172.0 adds `thread/sample` to its app-server: one model
  response sampled from a copy of the thread, here with `allowInProgressTurn` while the
  turn runs. Codex reports no text when that response is a tool call, and never runs
  the call. The sample is not written to the thread's history and is cancelled with
  `thread/sample/cancel`. Its prompt also tells the model that tools are unavailable.
  The adapter samples only once Codex reports the turn's user message completed: a
  sample taken earlier copies a thread without the request the agent is working on.
  Codex's `/side` command, a `thread/fork` that keeps its tools, stays unused.
- **Gate on a minimum release, qualified in CI.** Adapters announce side questions
  from the first release probed (Claude Code 2.1.308, Codex 0.172.0) onwards, for plain
  `major.minor.patch` versions only. Codex names its release in the user agent its
  app-server reports at initialization. Instead of an exact pin, CI's `cli-qualification`
  job runs `tests/native_side_questions.rs` against every Claude Code and Codex release a
  build ships: through Cairn's adapters and a local model, a side question gets its
  answer while the turn runs, one whose answer calls a shell tool settles without the
  tool running, a withdrawn one settles silently, the turn completes, and no side
  question reaches the saved session or the next turn's context. A release that breaks
  this fails the build rather than Cairn's users.
- **Unchanged.** OpenCode offers no tool-less side mechanism and stays unavailable.
  Only a running execution whose adapter announced `chat.side.available` receives side
  questions, so an older adapter never delivers one as steering. Withdrawal, expiry at
  the end of the turn, redaction and storage follow ADR-0081.

Probed on 2026-11-25 with Codex 0.172.0 and Claude Code 2.1.311 against local model
servers: both answered the text question, refused the `touch` the model attempted (no
file was created), dropped the withdrawn question, and kept every side question out of
their saved session.

Servers that report `codex` as the reason (ADR-0081) remain decodable; this server
reports `codex-release` only for a Codex release before 0.172.0.

# Run instruction measurements

Every run that uses the chat protocol (chats, all Claude Code and OpenCode runs,
Codex runs on a managed account) receives the Cairn frame once, in its
instructions: agent instructions, projects, tooling, selected skills with their
paths inside the run, plugin instructions, deliverables, waits and the
interaction rule. Only chat turns are interactive. Scheduled work states that it
cannot answer questions, as [waits](WAIT-FOR.md) require. Its first turn sends
the task brief; a resumed turn sends its resume prompt. Codex runs without a
managed account keep the single legacy prompt.

## Before 2026-10-24

Scheduled work through the chat protocol received the frame twice:

- the instructions said *This is an interactive chat* and listed skills at their
  run paths, without plugin instructions;
- the first turn repeated the whole frame, said *This unattended task*, added the
  plugin instructions and listed skills at installation paths that do not exist
  in the run.

Chats on other backends than native never received plugin instructions, even
though [plugins](PLUGINS.md) add them to isolated runs whose agent is granted.

## Measurements

Measured 2026-10-24 on a native node (12 CPUs, load average around 170 from
concurrent builds), Claude Code 2.1.280, `claude-opus-5-5`, effort `high`. Base
`13d48355` against this change. Fixture: 30 selected skills (4,973 description
bytes), three plugin paragraphs (2,626 bytes), five projects with one opened,
native tooling, delegated run. Frames were rendered with
`run_output::prompt`, `run_output::chat_plan` and the `worker/launch.rs`
composition of each revision.

| Run | Frame bytes before | Frame bytes after | Skill paths missing in the run |
|---|---:|---:|---|
| Scheduled, Claude or Codex | 26,259 (12,090 + 14,169) | 14,249 (14,191 + 58) | 30 → 0 |
| Chat turn | 12,116 | 14,741 (adds plugin instructions) | 0 → 0 |

Each frame pair was sent to Claude Code with the flags Cairn's Claude runner
uses (`-p`, stream JSON, no setting sources, strict MCP configuration, appended
system prompt, accept edits), three repetitions per task:

| Task | Frame | Success | First request input tokens | Median seconds | Mean tool calls | Reads of missing skill paths | Mean cost USD |
|---|---|---|---:|---:|---:|---:|---:|
| Read the `tdd` skill and quote its first heading | before | 3/3 | 24,698 | 17.4 | 5.3 | 3 (one per run, first read) | 0.218 |
| | after | 3/3 | 20,473 | 14.3 | 3.3 | 0 | 0.159 |
| Answer a question about a two-file repository | before | 3/3 | 24,698 | 8.6 | 4.0 | 0 | 0.153 |
| | after | 3/3 | 20,473 | 7.0 | 3.0 | 0 | 0.108 |

Input tokens come from the API usage of the first request and were identical
across repetitions. No run asked a question in either condition. A third task
(rename a helper) was discarded: in this nested harness every shell command
failed before execution, so it measured the harness rather than the frame.

These are local measurements of three repetitions; they do not establish
production latency, cost or success rates.

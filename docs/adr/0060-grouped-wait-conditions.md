# Grouped wait conditions

Date: 2026-10-24.

Extends [durable condition waits](0038-durable-condition-waits.md).

## Context

A conversation or mission stores one wakeup, and a new `wait_for` call replaces it.
An agent that delegated several runs, or waits for CI and another run, could only
wait for one result at a time and was woken for each intermediate result.

## Decision

`wait_for` takes a list of `conditions` and, with several, a `mode`: `all`
resumes once every condition has a result, `any` at the first. This replaces the
single `condition` input; agents receive the new schema with each turn's tools.

- Groups contain two or more distinct external conditions: run dependencies and plugin
  conditions. Time conditions stay alone; the group's `timeoutSeconds` is its
  deadline and never stops the awaited work.
- A result is not a success. Failures, cancellations and `unavailable` count as
  results; `all` waits for each of them rather than stopping at the first failure.
- The observation is `{ state, type: "group", mode, members: [{ condition, result }] }`,
  with `result: null` for a condition that has none yet, including at the deadline.
- Each grouped plugin condition is observed, deduplicated and scheduled on its
  own. Results are recorded on the wakeup as they arrive (`members[i]`), so clients
  show progress, and the group's result is recorded with a `wait_ready` event once
  it can resume. Grouped run results are recorded by the scheduler tick.
- Dependency cycle detection follows every reachable run with a visited set, without
  a cardinality limit.

## Large orchestration groups

Updated 2026-10-24: remove the initial eight-condition and 32-run ceilings. Neither
was a measured technical boundary. Duplicate detection uses a hash set after
normalization; graph traversal stays iterative and visits each run once.

Registration probes at most four plugin conditions concurrently. The remaining
conditions are persisted pending and checked by the existing observer (16 distinct
targets per batch, four concurrent). This bounds initial remote work independently
of the group's size. Observer watchers share their owner/run snapshots rather than
copying the full group per condition. There is no guarantee every remote result
is observed before the global deadline. All result snapshots keep their original
order and complete payload; context size is still proportional to the group.

Both clients show ten rows per page. Small groups retain individual meter segments;
larger groups aggregate meter segments by outcome and keep the global result count.
Pagination is presentation only: public MCP, the installation tool and resumed
agents retain access to every condition and its result, without a new API shape.

## Compatibility

A single condition keeps its stored record exactly. A group is stored as
`condition: { type: "group", mode, conditions }` beside an additive `members`
array. The previous release reads it as an unsupported condition: it resumes the
agent with an `unavailable` observation instead of losing or misreading the wait.
The data compatibility epoch is unchanged.

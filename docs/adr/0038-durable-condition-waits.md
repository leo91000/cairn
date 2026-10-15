# Durable conditions for conversation continuation

Date: 2026-10-13.

Updated: 2026-10-14 — installation access also permits dependencies on other
agents' runs, following [ADR-0036](0036-opt-in-installation-tools-for-agents.md).

An agent repeatedly checking a remote status spends inference and can keep an
execution alive while no useful work is available. ADR-0037's durable wakeup
already supports releasing execution and continuing the same conversation.
Extend it with `cairn_workspace.wait_for` and named, read-only conditions.

The initial conditions are a permitted Cairn run finishing and a GitHub Actions
workflow run completing for an authorized project, run ID and full commit SHA.
Run dependencies require the existing installation grant and may target other
agents' runs in that installation. GitHub observation uses the agent's configured token or
its authorized shared connection, with a repository derived from the registered
project. Credentials are never persisted on the wakeup or returned in results.

Already completed conditions return an observation in the current turn without
scheduling a wakeup. Pending conditions store one turn-bound wakeup with a
mandatory timeout. Identical requests retain the deadline; different requests
replace it. Internal conditions are evaluated by the existing scheduler. GitHub
is observed separately through bounded, deduplicated CLI requests; network waits
never hold the launch tick or an execution/account lease.

A successful interactive turn with another continuation or pending input is not
a finished dependency. Reject self-dependencies, cycles and chains exceeding
32 runs. A completion observation includes unsuccessful terminal states so an
agent can diagnose them without waiting for the deadline.

Registration captures its initial live grant, conversation checks and user-input
revision in one short transaction, then rechecks them after an external probe.
Network calls remain outside transactions. A partial index on `chat.user` events
bounds that revision lookup for long histories. Observers recheck the current
condition, lifecycle and access before
recording a result. The launch transaction rechecks both the wakeup and the
observed state. New input, stop, deletion, failure and manual resume retain
ADR-0037's cancellation behavior. Queue pause retains the deadline.

Conditions persist in the existing conversation record. `wakeAt` is the fallback
deadline for a conditional wait; it retains its earliest-resume meaning for
`sleep`. Native context, working files, admission, fencing and storage publication
keep their existing contracts. Observations are data, never additional user
instructions or permission to act on a target.

GitHub polling is practical with the existing connection and installation
topology. A later webhook adapter can reuse the same scheduler contract while
providing replay, deduplication and current-state reconciliation. MCP Tasks can
provide job interoperability after capability negotiation, while Cairn continues
to own conversation scheduling.

See [exploration and capabilities](../WAIT-FOR.md) and
[metadata measurements](../WAIT-FOR-MEASUREMENTS.md). Further conditions require
explicit source semantics and permissions; arbitrary URLs, shell predicates and
local processes are not accepted by the initial schema.

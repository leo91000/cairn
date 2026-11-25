# Background jobs and live environment waits

Date: 2026-11-16. Updates the execution-release rule in ADR-0065 and ADR-0067;
ordinary waits without useful background work retain that rule.

A provider turn ending and its environment stopping are separate lifecycle
boundaries. A run-owned environment supervises provider turns and explicit
`start_job` commands. Each command has a private receipt channel, a fixed
monotonic deadline, bounded logs and a helper that adopts and reaps descendants.
A completion receipt means the command tree stopped; it never means the disk
was retired. Unknown termination requires the existing outer VM/cgroup fence.

A deferred wait releases the provider account and its turn credentials. While
jobs or useful containers remain, the environment and disk lease remain owned
by the same execution owner. The scheduler observes job state without starting
an LLM turn. A new provider turn gets fresh authorization and credentials in
that same environment; it cannot widen the pinned execution policy. The original
environment deadline bounds all turns and background work together.

Job admission uses the authenticated tool call identity. Repeated delivery of
the same request returns the same job; changed arguments under the same identity
are rejected. An ambiguous response does not authorize another command. Job IDs
are scoped to their run. Stored observations contain a bounded, redacted result,
never the command or credentials. Job completion and elapsed time can be combined
with external wait conditions in `any` mode; elapsed time is anchored to the
persisted registration time.

Loss of supervision invalidates running job outcomes. Recovery fences the old
environment before releasing ownership and reports the interruption; it does
not replay jobs. Existing runs use the legacy single-turn path until a freshly
admitted environment negotiates this protocol. Retained directories and VM images keep the storage lifecycle of ADR-0088;
legacy journal conversion remains subject to that decision. Database version 36 adds job references and extends wait
conditions while preserving prior wakeups and observations; rollback requires
a reader that understands that database version.

The environment deadline is fixed at admission, capped at 13 hours (the existing controller deadline limit), and survives
agent continuations without renewal. The environment keeps its physical node
slot while releasing its provider account. A scheduler-queued continuation wins
atomically over idle closure; cancellation and restart fence the original
attempt and never send a job to its replacement. A missing start acknowledgement
is reconciled by its original request identity rather than by issuing a command.

When the environment reaches its deadline after the conversation has finished,
Cairn keeps the completed turn's outcome and records the deadline in activity.
The controller still closes the environment and any unconfirmed job outcomes
become unknown. The saved outcome survives a manager restart during fencing.
A successful controller receipt also identifies the exact completed chat turn
if the manager restarts before publishing its outcome.
A confirmed workspace finalization failure overrides the completed outcome.
An outstanding wait or in-flight continuation still reports an interruption.

Native continuation refreshes existing transient bind inodes through controller-
owned handles. Managed provider credentials are removed inside the retained environment after
the provider tree is reaped and before its completion is acknowledged. A changed
credential directory or failed removal requires the outer fence. Observation of
job results has separate, revocable authority pinned to the retained attempt and
generation; it does not restore a provider tool credential.

New native credential paths require a fresh environment; continuing cannot add
mount authority. VM turns refresh credentials within their already imported home
after rechecking current grants; this does not add a guest mount. Home imports
resolve directory components without symlinks, pin the destination inode and
extract as the workload UID while retained jobs are alive. Provider/account exhaustion stops this
environment explicitly. Retained runtime configuration is refreshed only after
the previous provider tree is reaped. VM continuation preserves the guest
workspace while refreshing transient credentials and inbox imports. Both routes
retain their existing outer cleanup barrier even when helper observation fails.

Plan/read-only turns cannot start arbitrary background commands. Moving from a
plan turn to execution may restore the original environment sandbox; it never
widens that pinned sandbox. Existing jobs remain subject to the same deadline
and cancellation owner while an agent plans its next action.

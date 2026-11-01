# Observing an admission wait

`scripts/admission-wait.mjs` is a metadata-only helper for an external health
monitor. A queued conversation's age is not its current admission-wait age. A
recent event is not necessarily an admission transition either. The current
projection supplies neither a canonical wait-start timestamp nor its history, so
the helper uses neither timestamp and performs no additional database query.

Call `observeAdmissionWaits(sample, previousState, retainedEpisode)` with each fresh bounded manager
probe and persist its returned `state`. The sample has an epoch-millisecond `at`
and a manager result containing `ok`, `data.runs`, and `data.runsComplete`. Each
run supplies only `id`, `status`, `waitClass`, `attemptId`, `activeMessageId`, and
`missionTurnId`. Technical identifiers are bounded ASCII identifiers; at least
an attempt or logical turn must be known. Unsupported evidence is unknown.

The current producer cannot establish a fresh positive class for a queued row.
The existing `account_required` and `recovery_pending` fields are typed, but no
current generation or activation fact binds them to the current admission gate.
They can remain stale across wait, resume, recovery, launch, move and end
transitions, so this producer preserves them only as context and sets
`waitClass: null` for every queued row. A trusted upstream producer may still pass
a preclassified `waitClass: 'capacity'` to the public observer contract; this
producer has no current positive capacity proof.

The `account_wait_reason` column is also not provenance. `report_capacity_wait`
writes its node marker there, and `report_account_wait` can write a raw provider
error to the same column. Those values can be byte-for-byte identical, including
the complete canonical node message. No prefix, exact message, account-slot
wording, or model wording proves a node-capacity or account gate. Queued rows
remain unknown: they do not age as capacity, set `currentCapacity`, certify
recovery, or rearm/clear sent evidence. Known nonqueued status and absence from a
complete active inventory remain valid positive ends for an existing capacity
observation; a partial inventory cannot establish absence.

| Existing fact or writer | What this producer can establish |
| --- | --- |
| `account_required` from `report_account_wait` | The stored enum is retained as context. It has no freshness binding, so it does not classify the current queued gate. |
| `recovery_pending` from retry, manual resume, restart, failure/fencing, and recovery paths | The stored boolean is retained as context. It can be stale before or after recovery, so it does not classify the current queued gate. |
| `worker/schedule.rs::report_capacity_wait` and `report_account_wait` | Both write the shared legacy reason column. Text alone cannot establish which writer last owned the current wait. |
| `accounts/mod.rs::waiting` | Busy-slot, usage, and unsupported-model outcomes can leave attention null; their reason text has no writer provenance. |
| `worker/launch.rs`, `worker/retry.rs`, `worker/control.rs`, `worker/mod.rs`, `worker/execution.rs`, `worker/checkpoint.rs`, and `nodes/moves.rs` | These paths can replace, resume, recover, launch, move, or end the gate while older typed fields remain. The current producer does not treat those fields as fresh. |

The producer reuses its existing bounded query and account projection. It adds no
query, history scan, schema change, writer change, permission or wait-start
inference. Raw reasons are not returned to the monitor.

Positive node-capacity detection requires a future authoritative writer fact
stored atomically with the run row. The future change must cover every wait,
launch, recovery, move, and terminal writer, bind the fact to a monotonic current
gate generation and writer-contract epoch, and drain older writers before the
producer trusts it. A nullable column's presence is not freshness proof; mixed
version rows remain unknown. The current observation path deliberately leaves
this legacy coverage gap visible instead of treating old typed fields or reason
text as proof.

A current `queued`/`capacity` observation begins its own interval on its first
sample. It becomes `bad` only after more than ten minutes of fresh observations
with the same run, attempt and logical turn. Samples more than two minutes apart
break the measured interval. This is an observed lower bound, not a claim that
every instant between samples was observed. Unrelated event timestamps do not
start or refresh it. Account, recovery and other waits are not disk failures.

An unavailable probe, missing row in a partial inventory, malformed row or
excessive sampling gap breaks age continuity. The episode identity survives
unknown observations for deduplication; unknown never certifies recovery or
rearms a notification. Validated run/turn scope is independent of the gate,
clock and identity allocation: an unknown gate/status cannot restore a positively
ended turn. A present valid row can still be observed in a partial inventory. Known noncapacity state or absence from a complete active inventory
ends only this capacity-wait observation. Its per-run positive end remains in the
same bounded helper history, allowing a new episode even while unrelated evicted
runs remain uncertain and no optional callback is installed. End facts do not
require a usable clock or scope; their next episode still needs valid current
scope and timing before it can age. That does not prove physical cleanup,
successful execution, working admission, backup integrity or a repaired node.

The state contains at most 200 metadata entries, matching the bounded active plus
tracked run projection. Its `limited` flag exposes row/history truncation. Lost
identity uncertainty survives partial inventories even after currently retained
rows end; only a validated complete inventory can establish which omitted runs
ended. Entries present with unknown identity stay unknown until exact identity or
a positive transition is observed. Malformed persisted history is uncertain,
never equivalent to a first-ever monitor observation. The optional `retainedEpisode(runId, scope)` callback reconciles the caller's existing
bounded occurrence/sent evidence. It returns an exact `occurrence`, `{ fresh:
true }` only after known end/scope change, `{ unknown: true }` for unavailable or
ambiguous identity, or null when it has no evidence. `readAdmissionOccurrence`
strictly decodes bounded canonical metadata; a retained value must match the same
run and full scope. Never copy an old owner's scope into a different logical turn.

Clock rollback resets only measured duration, not the retained episode identity.
The first-observed identity timestamp may consequently be later than the current
clock; it is never subtracted to calculate wait duration. A lost history entry
without identity reconciliation remains unknown and carries no new occurrence.
An active coordinator owner alone may no longer prove whether a later same-scope
wait ended after local history loss: the caller must keep that case unknown,
rather than transfer old ownership to a possibly new episode. Inspect every
occurrence within the caller's bounded owner registry: silently dropping later
occurrences must never become positive evidence that a turn is new.

Fresh observed episodes have a UUID discriminator independent of the wall clock.
It is allocated once, persisted with the helper state, and retained through
unresolved gaps/restarts. A positively known-ended recurrence therefore cannot
recreate the previous occurrence even if run, scope and clock timestamp repeat.
The helper uses `crypto.randomUUID()` by default; a fourth `createEpisodeId`
argument can supply deterministic UUIDs in tests. Failed or invalid allocation
stays unknown but preserves the positively observed current scope. Its bounded
`transition` marker records that the previous scope ended even before the new
identity can be allocated. The same marker can preserve a validated known end
from a caller's exact coordinator record after local history eviction. Persist it
in the same bounded caller occurrence map; `readAdmissionTransition` validates it
separately from a real incident occurrence.
A later valid allocation starts a fresh measured interval; a transition marker
never carries old ownership into that episode. An invalid sample clock likewise
breaks measured duration while retaining validated unresolved identity and known
scope/end facts. No optional caller callback is needed for that preservation.
Legacy three-part occurrences remain exact for unresolved history;
only a positively fresh observation receives a new discriminator.

A trusted validated queued-capacity row also carries `currentCapacity: true` in
its observation, independently of `status`, scope, clock and episode identity.
This bit is current gate evidence, never a healthy/bad/mature result or an
occurrence. Ambiguous duplicate rows and unknown gates cannot establish it; it
is not recovered from historical state. The persisted history shape and bounds
are unchanged.

Persist a valid current capacity episode's identity in the existing bounded caller
map before its age matures. Independently, an unknown observation with
`currentCapacity: true` must interrupt the recovery-clear counter even when no
usable identity exists. The caller may use its rearm-only `pending` status for
that purpose without writing an episode or emitting an alert. One following
known-end observation is then one good sample, not the completion of an old
clear streak. This neither deletes sent evidence nor certifies health. Retain outbox, ownership and historical evidence
independently of helper-history eviction. A caller resolving absent identities
must use the returned `complete` flag, not an unvalidated inventory flag.
Reconcile bounded coordinator-only admission IDs too: complete absence or a known
noncapacity observation ends their prior episode even after local eviction. Keep
that positive end in bounded caller occurrence history, rather than only a good
counter that normal compaction may drop. Historical ownership remains exact and
must not suppress the distinct next episode; partial absence proves no end.

Use generic per-run `admission-wait:<runId>` identities and the supplied occurrence
for new alerts. A legacy global `admission-disk-headroom` episode has no proven
per-run start or gate; upgrading the helper cannot certify it resolved. Preserve
that episode as unknown for explicit coordinator reconciliation. Do not carry its
ownership into all future waits or acknowledge its notification automatically.

The helper returns no prompt, command, error text, payload, account credential,
disk reservation or inferred rejection cause. It does not modify runtime state,
historical coverage gaps, log cursors, outboxes, pins or acknowledgements. The
external monitor's deployment, state migration and functional verification remain
separate actions.

Notification acknowledgement tokens must likewise be strictly newer than the
previous acknowledged token, even after clock rollback. The actual delivery time
and observed sample time remain separate metadata. If no larger safe integer can
be encoded, preserve the durable outbox with an explicit delivery-blocked reason;
never silently reuse an old acknowledgement for a new notification.

Validation: `node --test scripts/tests/admission-wait.node.mjs` exercises observed
age, identity transitions, gaps, partial inventory, restart, bounds and unknown
semantics. The Vitest wrapper includes the same tests in normal PR validation.

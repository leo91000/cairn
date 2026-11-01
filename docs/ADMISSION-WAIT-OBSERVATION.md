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

A current `queued`/`capacity` observation begins its own interval on its first
sample. It becomes `bad` only after more than ten minutes of fresh observations
with the same run, attempt and logical turn. Samples more than two minutes apart
break the measured interval. This is an observed lower bound, not a claim that
every instant between samples was observed. Unrelated event timestamps do not
start or refresh it. Account, recovery and other waits are not disk failures.

An unavailable probe, missing row in a partial inventory, malformed row or
excessive sampling gap breaks age continuity. The episode identity survives
unknown observations for deduplication; unknown never certifies recovery or
rearms a notification. A present valid row can still be observed in a partial
inventory. Known noncapacity state or absence from a complete active inventory
ends only this capacity-wait observation. That does not prove physical cleanup,
successful execution, working admission, backup integrity or a repaired node.

The state contains at most 200 metadata entries, matching the bounded active plus
tracked run projection. Its `limited` flag exposes row/history truncation. The
optional `retainedEpisode(runId, scope)` callback reconciles the caller's existing
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
rather than transfer old ownership to a possibly new episode.

Fresh observed episodes have a UUID discriminator independent of the wall clock.
It is allocated once, persisted with the helper state, and retained through
unresolved gaps/restarts. A positively known-ended recurrence therefore cannot
recreate the previous occurrence even if run, scope and clock timestamp repeat.
The helper uses `crypto.randomUUID()` by default; a fourth `createEpisodeId`
argument can supply deterministic UUIDs in tests. Failed or invalid allocation
stays unknown but preserves the positively observed current scope. Its bounded
`transition` marker records that the previous scope ended even before the new
identity can be allocated. Persist that marker in the same caller occurrence map;
`readAdmissionTransition` validates it separately from a real incident occurrence.
A later valid allocation starts a fresh measured interval; a transition marker
never carries old ownership into that episode. An invalid sample clock likewise
breaks measured duration while retaining validated unresolved identity and known
scope/end facts. No optional caller callback is needed for that preservation.
Legacy three-part occurrences remain exact for unresolved history;
only a positively fresh observation receives a new discriminator.

Persist a valid current capacity episode's identity in the existing bounded caller
map before its age matures. A known current capacity observation stops a recovery
clear streak even while its duration is still pending; this neither deletes sent
evidence nor certifies health. Retain outbox, ownership and historical evidence
independently of helper-history eviction. A caller resolving absent identities
must use the returned `complete` flag, not an unvalidated inventory flag.

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

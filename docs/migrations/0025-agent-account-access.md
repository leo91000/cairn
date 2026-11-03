# 0025 — Account access to agents

Version 24 upgrades to 25 automatically in `Store::open`, before the SQL actors
start ([ADR-0075](../adr/0075-plugin-hooks-rules-and-scoped-grants.md), « Accès des
comptes aux agents »).

One immediate transaction creates `agent_account_access`, keyed by agent and member
account, records each entry's membership and grant time, then stores the ids of the
existing agents under the `agent-access-rollout` KV key and advances `user_version`.
No agent, conversation, task or other KV entry changes.

The rollout remains pending until the first successful authoritative membership
snapshot. That snapshot grants each current member access to every agent recorded at
upgrade, except for explicit owner revocations recorded while rollout is pending.
The owner does not need an entry, and agents created after the upgrade are not in the
candidate list. If a Beacon outage delays the first snapshot, members present when it
finally succeeds are included, including people who joined or were re-invited during
the gap. Memberships added after that snapshot receive no automatic access; a
re-invitation never restores access.

An unavailable, unclaimed or otherwise missing membership policy is not a snapshot
and leaves rollout pending. A successful empty membership list completes rollout
without granting any member access. No Beacon/installation clock comparison is
needed. Owner revocations made before the first snapshot are recorded by agent and
account even when no access row exists yet, so the snapshot cannot undo them.

Older pending rollout records with a timestamp cutoff are read using the same
first-snapshot rule; the obsolete cutoff is ignored. A record without a usable agent
list cannot grant access. Older Beacons remain compatible because scheduling uses
only account and membership ids.

Reopening an upgraded database does not replace its agent list. Repeated membership
observations after the first one cannot repeat the rollout or restore a revoked
grant.

A failure leaves the schema, KV and version intact at 24. A successful upgrade
has no downgrade path. Intermediate 11 → 24 migrations remain supported and retain
their own transaction boundaries.

`agent_access` covers the first snapshot after a delayed observation, joins and
re-invitations during the gap, later memberships being excluded, pending owner
revocations, legacy pending rollout records, and a member's scheduled task surviving
the upgrade. Its unit tests check upgrade idempotence; protocol tests check
mixed-version compatibility. Earlier migration suites assert the final version with
the rollout key excluded from their unchanged-KV comparisons.

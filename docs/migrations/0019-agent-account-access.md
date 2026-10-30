# 0019 — Account access to agents

Version 18 upgrades to 19 automatically in `Store::open`, before the SQL actors
start ([ADR-0075](../adr/0075-plugin-hooks-rules-and-scoped-grants.md), « Accès des
comptes aux agents »).

One immediate transaction creates `agent_account_access`, keyed by agent and member
account, records each entry's membership and grant time, then stores the ids of the
existing agents and the upgrade's UTC Unix millisecond cutoff (`upgradedAt`) under
the `agent-access-rollout` KV key and advances `user_version`.
No agent, conversation, task or other KV entry changes.

Membership checks retain access only for current memberships whose Beacon
`joined_at` is strictly earlier than that stored cutoff. Beacon supplies the
current membership's timestamp, rounded up to milliseconds. Invitation acceptance
records insertion time after taking the installation lock, so a lock wait cannot
backdate a new membership to its transaction's start. A removed and
re-invited account has a new membership and timestamp. New memberships, including
re-invitations during a Beacon outage after the upgrade, receive no automatic
access. Equal timestamps also receive no automatic access. The owner never needs
an entry, and agents created afterwards start without member access.

Beacon and installation must use comparable UTC clocks for this timestamp
boundary. This cannot reconstruct an exact historical membership set under
arbitrary clock skew: the installation previously kept no membership list.
There is no clock tolerance or later-observation exception that widens the cutoff.

An older Beacon may omit `joined_at`: scheduling continues to use its unchanged
account and membership ids, but an unknown, negative or out-of-range timestamp
never grants automatic access. The rollout keeps its original cutoff while any
current membership lacks usable evidence and completes when all current
memberships have evidence (including an empty member list). Owner grants remain
available, and revocations record exclusions while the rollout is pending, even
when there is no entry yet, so later evidence cannot reverse an owner decision.
Older installations ignore the additional membership timestamp.

Reopening an upgraded database does not replace its cutoff or agent list. A
pending rollout written by an older build without a usable cutoff is discarded
without granting access; the owner can grant access explicitly. A cutoff is never
invented at first observation.

A failure leaves the schema, KV and version intact at 18. A successful upgrade
has no downgrade path. Intermediate 11 → 18 migrations remain supported and retain
their own transaction boundaries.

`agent_access` covers existing members retaining access, later and re-invited
members being excluded before the first observation, equal and missing timestamp
evidence, pending owner decisions, an unknown legacy cutoff, and a member's
scheduled task surviving the upgrade. Its unit tests check upgrade idempotence;
protocol tests check mixed-version compatibility, and Beacon sharing tests check
the authoritative timestamp and rounding. Earlier migration suites assert the
final version with the rollout key excluded from their unchanged-KV comparisons.

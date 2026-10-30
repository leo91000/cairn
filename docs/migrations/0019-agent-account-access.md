# 0019 — Account access to agents

Version 18 upgrades to 19 automatically in `Store::open`, before the SQL actors
start ([ADR-0075](../adr/0075-plugin-hooks-rules-and-scoped-grants.md), « Accès des
comptes aux agents »).

One immediate transaction creates `agent_account_access`, keyed by agent and member
account, records each entry's membership and grant time, then stores the ids of the
existing agents under the `agent-access-rollout` KV key and advances `user_version`.
No agent, conversation, task or other KV entry changes.

The rollout is completed by the first successful membership check after the
upgrade: each member at that check receives access to every agent in the list,
under their current membership, and the key is removed. The owner never needs an
entry. Agents created afterwards, and members invited afterwards, start without
access. The installation keeps no list of members, so someone invited between the
upgrade and that check, which only a Beacon outage can delay, counts as an existing
member.

A failure leaves the schema, KV and version intact at 18. A successful upgrade
has no downgrade path. Intermediate 11 → 18 migrations remain supported and retain
their own transaction boundaries.

`agent_access` covers the rollout for existing members only, a member's scheduled
task surviving the upgrade, and the earlier migration suites assert the final
version with the rollout key excluded from their unchanged-KV comparisons.

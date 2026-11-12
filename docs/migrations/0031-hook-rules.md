# 0031 — Hook rules

Version 30 upgrades to 31 automatically in `Store::open`, before the SQL actors start
([ADR-0075](../adr/0075-plugin-hooks-rules-and-scoped-grants.md), « Règles de hook »).

One immediate transaction creates three empty tables and advances `user_version`:

- `hook_rules`: one row per rule, with its creator, membership, current version and
  revision, and the time each stored block began (disabled, emergency pause, exhausted
  budget, ended membership).
- `hook_rule_versions`: the immutable versions, with the definition, who saved it, from
  where, in which conversation, and the native provenance of the saving request.
- `hook_rule_uses`: the agents, hooks and tools the current version uses, kept to list
  the rules of an agent or instance and to protect the grants they use.

No rule exists before; nothing is converted. A failure leaves the database at 30. A
successful upgrade has no downgrade path. Reopening an upgraded database changes
nothing.

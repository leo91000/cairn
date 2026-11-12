# 0032 — Hook rule engine

Version 31 upgrades to 32 automatically in `Store::open`, before the SQL actors start
([ADR-0075](../adr/0075-plugin-hooks-rules-and-scoped-grants.md), « Budget du moteur de
règles »).

One immediate transaction creates four empty tables and advances `user_version`:

- `hook_rule_triggerings`: one row per triggering, with its pinned version, its event,
  its interpreter position, the action pending or in flight, its counters and state;
  unique per rule and queued event.
- `hook_rule_steps`: the actions each triggering ran, with input, result and error.
- `hook_rule_values`: the values rules remember, at most 64 KiB each.
- `hook_rule_quiet_hours`: the number of triggerings that ended without effect, by
  rule and hour, kept a week.

No triggering exists before; nothing is converted. A failure leaves the database at
31. A successful upgrade has no downgrade path. Reopening an upgraded database changes
nothing.

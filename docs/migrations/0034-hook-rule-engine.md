# 0034 — Hook rule engine (SQLite schema 33)

Version 32 upgrades to 33 automatically in `Store::open`, before the SQL actors start
([ADR-0075](../adr/0075-plugin-hooks-rules-and-scoped-grants.md), « Budget du moteur de
règles »).

One immediate transaction creates six empty tables and advances `user_version`:

- `hook_rule_admissions`: private per-rule resource-resolution admissions; acknowledging
  the shared transport event and creating these rows is atomic. They are bounded
  together with active triggerings and never exposed as history before authorization.
- `hook_rule_triggerings`: one row per triggering, with its pinned version, its event,
  its interpreter position, the action pending or in flight, its counters, recorded
  byte budget and state;
  unique per rule and queued event.
- `hook_rule_receipts`: delivery deduplication while events are queued and hourly
  accounting independent of history; lifting a budget clears its accounting.
- `hook_rule_steps`: the actions each triggering ran, with input, result and error.
- `hook_rule_values`: the values rules remember, at most 64 KiB each.
- `hook_rule_quiet_hours`: the number of triggerings that ended without effect, by
  rule and hour, kept a week.

No triggering exists before; nothing is converted. A failure leaves the database at
32. A successful upgrade has no downgrade path. Reopening an upgraded database changes
nothing.

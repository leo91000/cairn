# Installation database 16 → 17

Plan turns ([ADR 0077](../adr/0077-plan-turns.md)) add a `plan` flag to queued and
delivered messages, and to questions asked by a plan turn. `Store::open` upgrades
version 16 in one immediate SQLite transaction before starting database actors or
serving requests: it adds `chat_messages.plan` and `chat_questions.plan`, both
`INTEGER NOT NULL DEFAULT 0 CHECK(plan IN (0,1))`, and records version 17. Versions
11 to 15 first follow their existing migrations, including the
[typed questions and waits migration](0016-typed-questions-and-waits.md) to version 16.

Existing messages and questions predate plan turns and keep every other column
unchanged. No other table is touched. A failure rolls the transaction back and
leaves version 16 intact; reopening version 17 performs no further migration.
Version 16 binaries refuse a version 17 database without changing it. Stop the old
manager and retain a consistent `manager.db`/WAL backup before upgrading; never run
two manager versions against the same database. No downgrade is supplied: rolling
back to a version 16 binary means restoring that backup, which loses changes made
after the upgrade.

`chats::migration::tests::version16_messages_gain_plan_requests_without_other_changes`
upgrades the real version-14 fixture of migration 0015 through the version-16
questions migration, then checks the unchanged rows, repeated opens, a stored plan
request and the column constraints.

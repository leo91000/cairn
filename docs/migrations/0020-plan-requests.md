# Installation database 19 → 20

Plan turns ([ADR 0077](../adr/0077-plan-turns.md)) add a `plan` flag to queued and
delivered messages, and to questions asked by a plan turn. `Store::open` upgrades
version 19 in one immediate SQLite transaction before starting database actors or
serving requests: it adds `chat_messages.plan` and `chat_questions.plan`, both
`INTEGER NOT NULL DEFAULT 0 CHECK(plan IN (0,1))`, and records version 20. Versions
11 to 18 first follow their existing migrations, including the
[typed questions and waits migration](0016-typed-questions-and-waits.md),
[attachment reservations](0017-attachment-reservations.md), and
[per-account reads](0018-personal-read-state.md) to version 18, followed by the
version-19 run-projection migration.

Existing messages and questions predate plan turns and keep every other column
unchanged. No other table is touched. A failure rolls the transaction back and
leaves version 19 intact; reopening version 20 performs no further migration.
Version 19 binaries refuse a version 20 database without changing it. Stop the old
manager and retain a consistent `manager.db`/WAL backup before upgrading; never run
two manager versions against the same database. No downgrade is supplied: rolling
back to a version 19 binary means restoring that backup, which loses changes made
after the upgrade.

`chats::migration::tests::version19_messages_gain_plan_requests_without_other_changes`
upgrades the real version-14 fixture of migration 0015 through the version-16
questions, version-17 attachments, version-18 read-state and version-19
run-projection migrations, then checks
the unchanged rows, repeated opens, a stored plan request and the column constraints.

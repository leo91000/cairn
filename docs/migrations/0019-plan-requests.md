# Installation database 18 → 19

Plan turns ([ADR 0077](../adr/0077-plan-turns.md)) add a `plan` flag to queued and
delivered messages, and to questions asked by a plan turn. `Store::open` upgrades
version 18 in one immediate SQLite transaction before starting database actors or
serving requests: it adds `chat_messages.plan` and `chat_questions.plan`, both
`INTEGER NOT NULL DEFAULT 0 CHECK(plan IN (0,1))`, and records version 19. Versions
11 to 17 first follow their existing migrations, including the
[typed questions and waits migration](0016-typed-questions-and-waits.md),
[attachment reservations](0017-attachment-reservations.md), and
[per-account reads](0018-personal-read-state.md) to version 18.

Existing messages and questions predate plan turns and keep every other column
unchanged. No other table is touched. A failure rolls the transaction back and
leaves version 18 intact; reopening version 19 performs no further migration.
Version 18 binaries refuse a version 19 database without changing it. Stop the old
manager and retain a consistent `manager.db`/WAL backup before upgrading; never run
two manager versions against the same database. No downgrade is supplied: rolling
back to a version 18 binary means restoring that backup, which loses changes made
after the upgrade.

`chats::migration::tests::version18_messages_gain_plan_requests_without_other_changes`
upgrades the real version-14 fixture of migration 0015 through the version-16
questions, version-17 attachments and version-18 read-state migrations, then checks
the unchanged rows, repeated opens, a stored plan request and the column constraints.

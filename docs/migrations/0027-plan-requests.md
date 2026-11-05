# Installation database 26 → 27

Plan turns ([ADR 0080](../adr/0080-plan-turns.md)) add a `plan` flag to queued and
delivered messages, and to questions asked by a plan turn. The current main
schema-26 migration first converts typed deliverable and skill-source metadata
from the previous schema 25. `Store::open` then reads the resulting version and
upgrades schema 26 to 27 in one immediate SQLite transaction before starting
database actors or serving requests. That migration adds `chat_messages.plan`
and `chat_questions.plan`, both `INTEGER NOT NULL DEFAULT 0 CHECK(plan IN (0,1))`.

A database at version 25 runs the existing 25 → 26 typed deliverable and
skill-source migration, then this Plan migration. If startup stops between those
transactions, version 26 remains valid and the next startup continues to version
27. A Plan-migration failure rolls back its column changes and leaves version 26
intact. Existing messages and questions keep every other column and row. A fresh
database is created directly at version 27 with the plan columns present.

Version 26 binaries refuse a version 27 database without changing it. Stop the
old manager and retain a consistent `manager.db`/WAL backup before upgrading;
never run two manager versions against the same database. No downgrade is
supplied: rolling back to a version 26 binary means restoring that backup, which
loses changes made after the upgrade.

`chats::migration::tests::version25_messages_gain_plan_requests_without_other_changes`
starts with the real version-14 conversation fixture, applies the intervening
migrations through schema 25, then exercises the complete 25 → 26 → 27 startup
chain. It checks retained rows, repeated opens, a stored plan request and the
column constraints. The deliverable and skill-source migration has its separate
schema-25 preservation suite.

The reproducible two-source qualification runs only on an ephemeral CI runner:
set `plan-storage-reference` to the exact reviewed schema-26 base SHA when
dispatching `Quality and container`. It builds the actual reference and
candidate, runs the same 1,000-record typed message/question workload in three
alternating trials, and executes the old manager against a synthetic upgraded
copy. It never opens production data. Raw timings, encoded/domain bytes, database
bytes, CPU, host and profile evidence are uploaded as
`plan-storage-qualification`; do not confuse cold SQLite connections with a
cold OS/device cache or claim production/network latency.

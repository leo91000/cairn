# Installation database 15 → 16

Plan turns ([ADR 0077](../adr/0077-plan-turns.md)) add a `plan` flag to queued and
delivered messages. `Store::open` upgrades version 15 in one immediate SQLite
transaction before starting database actors or serving requests: it adds
`chat_messages.plan INTEGER NOT NULL DEFAULT 0 CHECK(plan IN (0,1))` and records
version 16. Versions 11 to 14 first follow their existing migrations; the version-14
conversation upgrade already creates the current message table, so this step then
only records the version.

Existing messages were never plan requests and keep every other column unchanged.
No other table is touched. A failure rolls the transaction back and leaves version
15 intact; reopening version 16 performs no further migration. Version 15 binaries
refuse a version 16 database. Stop the old manager and retain a consistent
database/WAL backup before upgrading; never run both versions against the same
database.

`chats::migration::tests::version15_messages_gain_plan_requests_without_other_changes`
rebuilds the version-15 message layout from the real version-14 fixture of migration
0015, then checks the unchanged rows, repeated opens and a stored plan request.

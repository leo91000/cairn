# Installation database 21 → 22

`Store::open` upgrades version 21 before starting database actors or serving
requests. Versions 11 to 20 first follow their existing migrations to version 21, including the runs projection, plugin hooks and progress migrations.
Each version transition is atomic. Stop the old manager and retain a consistent
database/WAL backup before upgrading; never run both versions against the same database.

Version 22 adds the timeline projection: `timeline_entries` (one row per card,
keyed by run and card identity, with its position, change revision and source
event) and `timeline_state` (the reducer checkpoint per run). One immediate
SQLite transaction creates both tables and replays every retained event of every
run, in event order, through the same reducer the event writer uses. The
projection is an index of the events: it stores no copy of message text or
details, which are read from the source event.

No existing row changes. Runs, events (identities, order, text, payloads,
timestamps), the event `AUTOINCREMENT` sequence, checkpoints, conversations,
messages, attachments, credentials, KV values, audit entries, accounts, nodes and
every other table keep their exact content; the upgrade test compares every row of
every pre-existing table before and after. Private answers stay as the placeholder
already recorded in their `chat.user` event.

A failure to create the schema, replay an event or satisfy a constraint or foreign
key rolls back the entire upgrade, including schema changes. The version remains 21
and the database is available for repair and retry. Reopening version 22 performs
no further migration. There is no downgrade path from a committed version 22;
failure rollback leaves version 21 intact.

Clients reset their saved histories once: the history revision becomes `v2:` so a
saved `v1:` cursor receives a fresh window with its cards. This is a cache reset,
not a data change.

`crates/installation/tests/fixtures/timeline-v18.sql` is a dump of a real version-18
database. Main `d5e592ac`'s `Store::open` upgraded the version-14 conversation fixture
of migration 0015, then its run admission, conversation and event writers recorded a
three-engine corpus (successive reply snapshots, a reused item ID in a later turn,
reconnects, intermediate and final errors, Cairn's automatic retry, the engine's own
retries, usage limits, notices, a speed fallback, background tasks, a stop echo, a
runner failure, an interruption and a private-answer placeholder). Main `28bdc87f`'s
real question and wait migration (0016) upgraded it to version 16, then main
`9aa330ac`'s attachment (0017) and personal read state (0018) migrations upgraded it to
version 18. All identities and credentials are synthetic; the fixture vault key is
`[42; 32]`.

The upgrade test first applies the runs projection, plugin hook and progress migrations to that dump, then checks every pre-existing table unchanged, the
interpreted cards of the three runs and a no-op reopen. A second test injects a
conflicting leftover table and checks that version 21 and all rows remain. A third
re-records the corpus through the live writer in transactions of varying size and
checks that it equals a full replay and a restart from every persisted checkpoint.

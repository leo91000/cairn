# 0018 — Per-account read state

Version 17 upgrades to 18 automatically in `Store::open`, before the SQL actors
start. The v17 fixture was produced by the real Store and Service at `f489e1a5`,
starting with the synthetic v16 attachments fixture. It holds three accounts' read
starts, chat and mission marks (including an uppercase identity, an unknown item and
a watermark an older observation could not lower), dismissed and snoozed outcomes,
and a restored outcome. No production data or credentials were used.

One immediate transaction creates `account_read_starts`, `account_reads` and
`account_dismissals`, imports every `read-since:{account}`, `read:{account}:{chat|task}:{id}`
and `dismissed:{account}:task:{id}` entry, then rebuilds KV with those prefixes
prohibited and advances `user_version`. Account identities, item identity spelling,
kinds (`task` becomes `mission`), read finishes, read starts, finish times and snooze
deadlines are preserved exactly; no state is shared with another account and none
is invented. Marks for deleted or unknown items are retained, as before.

Invalid identities, values, expiries or keys fail the transaction: schema, KV
entries and version remain intact at 17, and reopening retries after repair. The
old manager must be stopped and a consistent DB/WAL backup retained before
upgrading. A successful upgrade has no downgrade path; it never resets the
installation. Intermediate 11 → 17 migrations keep their own transaction boundaries.

`personal_state_migration` loads the real v17 dump, checks every previous key maps
to exactly one typed row, that unrelated tables and KV entries are unchanged, that
the removed prefixes are rejected, foreign keys and reopen stability, and that the
real Service lists each account's preserved outcomes. Malformed previous state
proves complete rollback and retry.

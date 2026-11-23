# 0038 — Antigravity coding agent (SQLite schema 37)

Version 36 upgrades to 37 automatically in `Store::open`
([ADR-0090](../adr/0090-antigravity-coding-agent.md)).

Seven tables restrict their coding-agent column with a CHECK constraint listing
`codex`, `claude` and `opencode`: `agents`, `coding_agent_accounts`, `account_leases`,
`coding_agent_configurations`, `runs` (`account_required`), `chat_messages` (which also
allows the empty value) and `conversation_engine_switches`. One immediate transaction
adds `antigravity` to each list and advances `user_version`.

SQLite cannot alter a CHECK constraint, and rebuilding these tables would rewrite every
run and message. A CHECK constraint does not change the on-disk format, so the upgrade
edits only the schema text with `PRAGMA writable_schema`, the method SQLite documents
for such changes, and increments `schema_version` so every connection reloads the
schema. The transaction fails unless all seven tables then admit `antigravity`.
`PRAGMA quick_check` runs after the commit. No row is read or rewritten.

Older binaries refuse version 37, as for earlier upgrades. Rolling back requires the
backup taken before the update.

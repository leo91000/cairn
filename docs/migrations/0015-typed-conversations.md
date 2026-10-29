# Installation database 14 → 15

`Store::open` upgrades version 14 before starting database actors or serving
requests. Versions 11, 12 and 13 first follow the existing inventory, direct-storage and
account-pool migrations to version 14. Each version transition is atomic.
Stop the old manager and retain a consistent database/WAL backup before upgrading;
never run both versions against the same database.

One immediate SQLite transaction moves chat records into `conversations`, message
headers into typed `chat_messages`, ordered upload snapshots into
`message_attachments`, engine-switch progress and idempotency receipts into
`conversation_engine_switches`, and the inactivity policy into
`conversation_retention`. The obsolete chat records, message documents and converted
KV keys disappear only in that same transaction. Unrelated records, KV values and
expiration timestamps, runs, events, checkpoints, credentials and counters remain
unchanged. Account identities, settings, ordered usage windows, exhaustion snapshots
and leases from v14 are untouched. Direct-storage grants, historical read authorizations and journal
acknowledgement/quarantine metadata introduced in v13 are untouched. There is no
runtime fallback or second writer.

Conversation identities, immutable readable slugs, existing revisions and record
ordering timestamps are retained. Historical agent/project assignment IDs remain
even if their resource was removed; live references use nullable foreign keys.
Current run references are checked. Historical account references are retained
without selecting an account or owning its lifetime. Message IDs, delivery states, text, selections,
private answer maps, equal-timestamp row order and attachment order are retained.
UUID spelling is preserved in persisted identities and typed commands, so retained
uppercase message IDs and attachment keys keep their retry/edit/delete semantics.
Messages without a revision receive one. Engine-switch receipts keep their exact
expiration; a current switch retains its separate expiry, pending state, model,
error and retry timestamp. Unreachable switch keys left by version-14 conversation purges are discarded;
no deleted conversation is recreated. Missing inactivity policy still means seven days.

Malformed data, unsupported states, identity mismatches and failed constraints or
foreign keys roll back the entire version-14 upgrade, including schema changes.
The version remains 14 and the old data is available for repair and retry. Upgrade
decoding diagnostics contain no message contents, answers or native provenance.
Reopening version 15 performs no further migration. There is no downgrade path
from a successfully committed version 15; failure rollback leaves version 14 intact.

`crates/installation/tests/fixtures/conversations-v14.sql` is a dump produced by
previous main `57855b96` through its real `Store::open`, legacy conversation writer
and account lease writer. Its account data starts from the real v13 fixture of
migration 0014 and is upgraded by the previous manager. All identities and
credentials are synthetic; the fixture vault uses the fixed test key `[42; 32]`.
It retains three providers, current/exhausted usage, ordered windows/models,
reset credits, one lease, encrypted credentials, historical account references,
private answers, equal-timestamp message order, attachment order, direct-storage
metadata and the audit sequence. It is not reconstructed by undoing candidate DDL.

A complete-fixture test verifies both domains and decrypts the synthetic vault
secret after startup and repeated opens. Focused scenarios replace only the
fixture's conversation inputs before exercising rollback, retained/deleted resource
references, revisions, UUID spelling, multiple switch receipts and independent
lifetimes. Node/direct-storage chain tests keep their existing preceding-version
fixtures and continue through the account and conversation migrations.

# Installation database 13 → 14

`Store::open` upgrades version 13 before starting database actors or serving
requests. Versions 11 and 12 first follow the existing inventory and direct-storage
migrations to version 13. Each version transition is atomic.
Stop the old manager and retain a consistent database/WAL backup before upgrading;
never run both versions against the same database.

One immediate SQLite transaction moves chat records into `conversations`, message
headers into typed `chat_messages`, ordered upload snapshots into
`message_attachments`, engine-switch progress and idempotency receipts into
`conversation_engine_switches`, and the inactivity policy into
`conversation_retention`. The obsolete chat records, message documents and converted
KV keys disappear only in that same transaction. Unrelated records, KV values and
expiration timestamps, runs, events, checkpoints, credentials and counters remain
unchanged. Direct-storage grants, historical read authorizations and journal
acknowledgement/quarantine metadata introduced in v13 are untouched. There is no
runtime fallback or second writer.

Conversation identities, immutable readable slugs, existing revisions and record
ordering timestamps are retained. Historical agent/project assignment IDs remain
even if their resource was removed; live references use nullable foreign keys.
Current run references are checked. Message IDs, delivery states, text, selections,
private answer maps, equal-timestamp row order and attachment order are retained.
UUID spelling is preserved in persisted identities and typed commands, so retained
uppercase message IDs and attachment keys keep their retry/edit/delete semantics.
Messages without a revision receive one. Engine-switch receipts keep their exact
expiration; a current switch retains its separate expiry, pending state, model,
error and retry timestamp. Unreachable switch keys left by version-13 conversation purges are discarded;
no deleted conversation is recreated. Missing inactivity policy still means seven days.

Malformed data, unsupported states, identity mismatches and failed constraints or
foreign keys roll back the entire version-13 upgrade, including schema changes.
The version remains 13 and the old data is available for repair and retry. Upgrade
decoding diagnostics contain no message contents, answers or native provenance.
Reopening version 14 performs no further migration. There is no downgrade path
from a successfully committed version 14; failure rollback leaves version 13 intact.

Tests reconstruct the affected version-13 DDL from pre-conversion main `bc8bdf64`,
seed persisted conversations, retained/deleted resource references, private answers,
ordered attachments and credentials, then open through the production startup
path. They verify preservation, repeat opens and complete schema rollback after a
late malformed message or broken run reference. Engine-switch tests exercise
multiple receipts and a pending current switch with independent lifetimes.

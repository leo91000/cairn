# 0017 — Conversation attachment quota reservations

Version 16 upgrades to 17 automatically in `Store::open`, before the SQL actors
start. The v16 fixture was produced by the real Store and Service at `b9cd3c2a`,
starting with the synthetic v15 questions/waits fixture. Its 22 uploads include
uppercase identities, optional image dimensions and a conversation using 200 MiB.
No production data or credentials were used.

One immediate transaction creates `attachments`, `attachment_quotas` and
`attachment_reservations`, imports every `chat-attachment:{chat}:{id}` entry, then
rebuilds KV with that prefix prohibited and advances `user_version`. The metadata
columns preserve identity spelling, conversation, sanitized name, size, media
kind/type, digest, creation time and optional dimensions. Existing files keep their
original name and location; message snapshots and ordered relations do not change.
Quota counters are reconstructed from those uploads, with SQL constraints and
foreign keys. No reservation is invented for a previously committed upload.

Invalid identities, metadata, references or quotas fail the transaction: schema,
KV entries, counters and version remain intact at 16. The old manager must be
stopped and a consistent DB/WAL backup retained before upgrading. A successful
upgrade has no downgrade path; it never resets the installation. Intermediate
11 → 16 migrations remain supported and retain their own transaction boundaries.

Runtime reservations have unique tokens and a five-minute deadline. File bytes
are written and synced privately before SQL publication; the authorized commit
rechecks the token, deadline and current conversation lifecycle. A cancelled
caller cancels publication while an owned task settles I/O and releases its
reservation. Crashes leave reservations charged until expiry; startup and ordinary
conversation ticks reconcile expired reservations. Startup also removes orphaned
unique speculative filenames. A stale writer cannot overwrite a replacement's
file. The domain coordinates uploads and purge per conversation; unrelated files
and conversations perform I/O independently. Legacy stable filenames are never
classified as speculative files.

`attachments_migration` verifies metadata, quotas, idempotence, retained file
paths, sandbox delivery, FK integrity, reopen stability and preservation of every
unrelated table. Malformed prior metadata proves complete rollback and repair /
retry. Domain tests cover competing uploads, cancellation, revocation, expiry,
stale publication and purge during preparation.

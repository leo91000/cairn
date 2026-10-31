# Installation database 22 → 23

`Store::open` upgrades the version-22 placement state before starting its actors or
serving requests. Versions 11–21 first use their existing sequential startup conversions to reach
version 22. Stop the old manager
before opening its database with the new binary; retain a consistent database/WAL
backup and all installation/node identities, credentials and durable files.

`placements` owns attempt identity, run/node references, execution/destination
role, selected runtime, resources and creation time. `attempts` owns its existing
fencing identity, lease bounds, native privilege generation, release acknowledgement
and disk-materialization accounting. Expiration never releases ownership by itself.
The virtual local development runner still works before its first inventory row.

`exclusive_reservations` stores the queue, lifecycle, controller epoch, deadlines,
confirmed/proposed expiry and immutable request scope. Ordered `exclusive_members`
and `exclusive_paused_runs` retain the corresponding attempt/run identities.
`exclusive_sequence` preserves both high-water counters, including values above
all retained requests. Historical identities remain after a conversation is purged;
nullable live-reference foreign keys are cleared by deletion without rewriting those
identities or dropping a still-unconfirmed controller reservation. The reconciler
continues to fail closed when a referenced execution is missing.

Allocation disk allowances preserve their original values, including values below
128 MiB after native admission subtracts pool and journal reserves. The 128 MiB
minimum still applies to configured node budgets, not their derived allocations.

Acquisition reasons retain secret masking before storage and projection. The raw
request keeps its 500-character limit; a stored mask can be longer when replacing
a short token. The migration preserves these already-redacted reasons intact. Reasons that contain
NUL are preserved too: the nonempty SQL check counts bytes after trimming, avoiding
SQLite text-length truncation at the first NUL. Rust retains its existing raw-input
validation, and empty/space-only reasons still fail.

Run placement fields move out of the unconverted `boundary` document into named
columns. The JSON wire projections remain read-side views; mutable rows never
regain the generated JSON columns removed by version 19. Resource, native-mode and move-request snapshots are serialized from Rust
structs; the existing run projections expose them to neighboring domains without a
second stored document. A11's volumes, publications and disk state keep their
existing storage. Node inventory derives its exclusive summary from the authoritative
reservation and no longer stores another copy.
The live move-reservation foreign key has a partial index so attempt deletion
checks its references without scanning unrelated runs.

The transaction copies the old attempt and exclusive-request records, preserves
ordered members and paused runs, migrates run placement and counters, rebuilds the
read-side run projection view and records constraint, then validates foreign keys before setting
version 23. Unrelated records, events, checkpoints, secrets, counters and snapshots
remain intact. Invalid input or a constraint failure rolls back the entire upgrade,
leaving the version-22 input unchanged. Errors never contain source
records or secrets. Reopening version 23 does no additional work. No downgrade is
provided; old managers reject the new schema.

Qualification includes the frozen historical run DDL, populated placement/attempt/
reservation state, preserved private boundaries and high counters, idempotent reopen,
and a late failure followed by a successful retry. The version-11 inventory
qualification also runs through the continuous startup upgrade chain.

The ordinary migration suite also loads `tests/fixtures/placements-v15.sql`, a
synthetic database produced by the real previous manager Store/domain writers at
`ff73512030c0e9109eec96be51269617a99b5534`. It contains the prior conversation and
account fixture upgraded by that manager, plus placements, attempts, ordered
reservation membership and high-water counters. The actual version-15 → 16 questionnaire and version-16 → 17 attachment and version-17 → 18 personal read state
and version-18 → 19 read-side run projection, version-19 → 20 plugin hooks and version-20 → 21 writer-owned progress and version-21 → 22 timeline projection
migrations from main are applied first,
then a `VACUUM INTO` copy of that previous-version database is upgraded;
all previous run values and attempt/reservation fields are compared, along with
every unchanged table/column, including account/conversation relations, encrypted
envelopes, expiries, event/checkpoint state and direct-storage metadata. The input
fixture remains unchanged. No production data or credentials are used.

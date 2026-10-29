# Installation database 12 → 13

Direct node storage adds coordination metadata without rewriting disk payloads:

- `direct_storage_grants` retains issued PUT windows and ambiguous outcomes.
- `direct_read_authorizations` retains exact historical grant/object/hash scope
  for idempotent integrity reports, without bearer credentials.
- `direct_journal_acknowledgements` pins an unresolved journal acknowledgement
  through its existing shared references, bounded to one inventory per disk.
- `direct_object_quarantines` retains accepted reconciliation preparations across
  manager restart and blocks reuse until outstanding acknowledgements are fenced.

`Store::open` uses an immediate transaction and rechecks the version under its
write lock. It creates the metadata and advances `user_version` only after that
transaction succeeds. Version 11 first completes the separate
[11 → 12 inventory migration](0012-typed-node-inventory.md), then continues here.
Each step is atomic. Credentials, node identities, grants, runs, other records
and existing object references remain intact. Concurrent startup rechecks each
step rather than repeating its DDL.

Stop the old manager and retain a consistent database/WAL backup before upgrading.
Pause new scheduling, finish pending publications with the old controllers, and
preserve every node's journals and runtime state. Verify the manager database
before enabling background work, then update all active controllers before new
v3 publications. An older controller cannot serve the new transfer operation.

Earlier binaries reject version 13. After v3 publications begin, rollback requires
the corresponding installation database **and** object storage; changing
`user_version` is not a downgrade. Unsupported versions never authorize deleting
an installation. See [direct transfer operation](../DIRECT-STORAGE-TRANSFERS.md).

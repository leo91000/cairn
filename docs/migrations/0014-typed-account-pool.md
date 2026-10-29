# Installation database 13 → 14

`Store::open` upgrades version 13 before starting database actors or serving
requests. Stop the old manager and retain a consistent backup of `manager.db`,
its WAL and `mcp-encryption-key`. Never run both manager versions against the same
database. Versions 11/12 first follow the existing inventory and direct-storage
upgrades to version 13, each a separate atomic step. An account conversion failure
retains the completed version 13 metadata and all previous data.

One immediate SQLite transaction copies `agentAccounts` records to
`coding_agent_accounts`, current and exhausted `account_usage`, ordered
`account_usage_windows` and `account_exhaustion`. It preserves account IDs,
provider identity fingerprints, names, enabled state, timestamps, parallel limits,
OpenCode provider binding and priority, quota windows and model order, exact
integer/decimal readings, reset credits and exhaustion snapshots. Each account
receives a settings revision; the authoritative records update time is retained.
The records table is rebuilt with its indexes and a constraint excluding accounts.
Other records, runs, checkpoints and KV entries remain unchanged. References keep
their account IDs. Encrypted credentials remain in the existing vault under the
same key; no credential is decrypted or re-encrypted by the upgrade.

Version 13 leases were process-local, so the new `account_leases` starts empty.
Existing restart recovery still invalidates run credential homes and excludes
recovering runs from admission. Sign-in challenges remain ephemeral and are not
replayed. The migration does not change credential isolation or driver behavior.

Malformed or unsupported records, duplicate identities, SQL constraints and
foreign-key failures roll back all schema and data changes. Version 13 stays
available for repair/retry and can still be opened by the previous manager after a
failed conversion. Errors do not include account data. The upgrade commits version
14 only after checking foreign keys; reopening is idempotent. Successful upgrades
have no automatic downgrade.

The regression fixture `crates/installation/tests/fixtures/account-pool-v12.sql`
is a dump of a real disposable database created by `Store`, `Account` and `Vault`
at `4af96bbe7ab5086f815bc360f3db730294869ba7` (version 12). It contains three
synthetic providers, paused/unknown usage, ordered windows/models, exhaustion,
reset credits, encrypted synthetic credentials and a retained account reference.
Tests verify preservation, post-upgrade writes/reopen, empty pools, rollback after
partial conversion and repair/retry. No production data or credential is used.

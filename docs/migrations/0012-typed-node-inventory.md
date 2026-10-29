# Installation database 11 → 12

The installation upgrades version 11 automatically during `Store::open`, before
starting database actors or serving requests. Stop the old manager and retain a
consistent backup of `manager.db` and its WAL before upgrading an installation.
Do not run both manager versions against the same database.

The upgrade runs in one immediate SQLite transaction. It copies JSON node records
into the typed inventory and its ordered tag, trusted-account and reviewed-agent
relations, then removes the obsolete inventory records and installs version 12's
records constraint and indexes. Node UUIDs, runtime identities, configuration,
native authorization, observations and exact byte counters are retained. Existing
agent grants, priorities, runs and disk references keep their node UUIDs. Other
records and all `kv` rows, including enrollment challenges, credential digests and
expiry timestamps, remain unchanged.

Version 11 had no configuration revision or persisted stale-disk summary. The
upgrade creates a revision and initializes that derived summary; absent storage
settings use the same defaults as the old runtime. Legacy null heartbeat flags
and native-mode protocol use false/zero, matching their previous interpretation.
Obsolete response fields (`sharedResources`, `releasedPlacements`, `runtimes`,
`updatedAt`) and the historical `fuse` observation have no version-12 inventory
column; the authoritative record update timestamp is retained in `nodes`.
Malformed or unsupported input and foreign-key failures abort the entire
transaction; the original schema and version remain available for repair and
retry. No node record or credential is included in upgrade errors.

Version 12 is committed only after conversion and foreign-key validation. Reopening
an upgraded database performs no further migration. Earlier layouts still need
their explicit migrations (including `0011-mixed-node-agents.sql` for version 10);
newer layouts require a compatible release. This change supplies no downgrade.

Releases with direct node storage continue through the separate
[12 → 13 metadata migration](0013-direct-node-storage.md).

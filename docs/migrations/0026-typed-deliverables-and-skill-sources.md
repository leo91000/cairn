# Typed deliverables and skill sources (25 → 26)

Schema-24 databases first run the existing coding-agent configuration upgrade
to schema 25. This A16 migration then upgrades schema 25 to 26 automatically at
startup in one immediate SQLite transaction. `artifacts` identifies a run/key;
`artifact_versions` keeps each immutable file version and preview metadata;
`artifact_visibility`
keeps its revocable public token and URL. Runs own artifact metadata through
foreign keys. File bytes and previews stay in `data/artifacts`.

`skill_sources` stores imported bundle provenance and the plugin-defined location
object. `skill_source_refs` holds the live plugin instance reference. A removed
project or instance leaves its recorded scope/identity intact, with a nullable
live reference: removing a resource must not erase where existing skill files
came from or prevent an upgrade. Plugin ID/version/folder and bundle revision
remain historical facts. Skill contents stay with the existing bundle writer.

The conversion checks every previous `artifact:`, `artifact-share:` and
`skill-source:` document, key identity, public-link target and current foreign
reference. Only after all inserts succeed does it remove those KV keys, exclude
them from future KV writes and advance the schema version. Unknown fields,
malformed metadata, expiring domain keys, dangling public links and invalid
version/size constraints fail the transaction. Original rows, schema and version
remain intact for repair and retry. Other KV data is preserved.

Key, title and group constraints count UTF-8 bytes, matching the publication
validator. SQLite's text length stops at U+0000; the constraints therefore count
the value cast to BLOB, preserving legal leading NULs and multibyte strings.

Stop the previous manager and retain a consistent database/WAL backup before an
upgrade. An older binary cannot open schema 26: recovery requires the retained
backup or a compatible forward fix. This PR authorizes no production action.
Follow the private upgrade rehearsal in `docs/PRODUCTION-CAIRN.md` before a future
release; synthetic qualification does not replace it.

## Qualification

`deliverable_metadata_migration` restores a schema-25 fixture derived from the
committed real v24 Store snapshot from main `c2e72b39` (the test-only #661
commit uses identical native sources). The fixture applies base `f599b77c`'s
24→25 coding-agent-configuration schema step and adds one explicitly synthetic
configuration row. Opening this schema-25 fixture through the current Store
runs A16's 25→26 migration. The configuration row and all prior rows/schema
objects must remain unchanged. Artifact versions, a public link and skill
sources originate in the real `b3ebdc4b` writer; the retained timeline card
originates in the actual v22 event writer. Additional synthetic project,
plugin instance, contribution/resource grant and credential supplier rows are
explicitly seeded with SQL into that previous schema and survive migration.
No production data is used.
Two additional artifact records, with a leading NUL in the key or title, are
persisted through that previous binary's real `Db::set` writer and reopened at
version26. This is the writer used by its publisher; the publication validator's
bounded-string function is unchanged. Qualification retains those exact values.
The run projection view, progress, timeline, placement and contribution tables,
and every unrelated row/schema object must also survive. It checks metadata and
unrelated rows, nullable
references for removed resources, the resulting FK/quick checks, reopening and
rejection of future domain KV writes. Corruption scenarios compare all original
table schemas/rows and `user_version` after failed startup.

Artifact, skills, lifecycle, live state, public Beacon sharing and installation
MCP suites exercise the existing domain behavior and the new run-based tools.
Reference/candidate metadata measurements and exact tested source are recorded in
the PR, with no measurement artifact committed. These are local SQLite costs;
they do not measure production network latency.

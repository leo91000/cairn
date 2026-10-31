# Typed deliverables and skill sources (18 → 19)

The manager upgrades the installation database automatically at startup in one
immediate SQLite transaction. `artifacts` identifies a run/key; `artifact_versions`
keeps each immutable file version and preview metadata; `artifact_visibility`
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

Stop the previous manager and retain a consistent database/WAL backup before an
upgrade. An older binary cannot open schema 19: recovery requires the retained
backup or a compatible forward fix. This PR authorizes no production action.
Follow the private upgrade rehearsal in `docs/PRODUCTION-CAIRN.md` before a future
release; synthetic qualification does not replace it.

## Qualification

`deliverable_metadata_migration` restores the committed synthetic SQL snapshot
produced by the real Store at `b3ebdc4b`: the previous v17 fixture upgraded by that
binary to v18, with artifact versions, a public link and skill source records
written by its `browser_fixture`. It checks metadata and unrelated rows, nullable
references for removed resources, the resulting FK/quick checks, reopening and
rejection of future domain KV writes. Corruption scenarios compare all original
table schemas/rows and `user_version` after failed startup.

Artifact, skills, lifecycle, live state, public Beacon sharing and installation
MCP suites exercise the existing domain behavior and the new run-based tools.
Reference/candidate metadata measurements and exact tested source are recorded in
the PR, with no measurement artifact committed. These are local SQLite costs;
they do not measure production network latency.

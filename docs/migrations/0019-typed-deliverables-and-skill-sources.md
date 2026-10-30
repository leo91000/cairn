# Typed deliverables and skill sources (18 → 19)

The manager upgrades the installation database automatically at startup. Artifact
versions, revocable visibility and skill source references move from business KV
keys into constrained SQL tables. File contents and skill bundles stay in their
existing storage. The migration copies and checks all metadata in one immediate
transaction before deleting the old keys and advancing the schema version.

Stop the previous manager and retain a consistent database/WAL backup before an
upgrade. A failed conversion leaves the previous database intact for repair and
retry. An older binary cannot open the new schema; recovery requires the retained
backup or a compatible forward fix. This change authorizes no production action.

Qualification and exact final schema number will be recorded with the PR after
integration with current main.

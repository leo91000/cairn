# Slack foundations: installation-data compatibility

Status: implementation review for draft PR #226, **not deployment approval**. The
Slack package and final qualification remain unfinished. Do not merge or deploy
this draft. This inventory must be revisited as later lots add persistent data.

## Compatibility boundary

The plugin foundations introduce an incompatible persisted plugin contract. A
manifest containing `webhooks`, `resources`, `operations`, `oauth`, or an operation
binding on a command/tool is rejected by the earlier `deny_unknown_fields`
manifest parser. An older host also lacks the requester and resource-authority
checks required by newly admitted work. Treat starting that host on this data as
an unsafe downgrade, even when some JSON records are syntactically readable.

The installation epoch advances to **7**. PR #226 inherited epoch 4; integrated
main `176969f5` reserves epoch 5 for durable session restart and epoch 6 for trusted
native execution. This plugin boundary uses a distinct higher epoch rather than
reusing those numbers. `legacyTree` remains unchanged. The production release gate
must refuse an unequal served/candidate epoch. This metadata is an operational deployment
fence, not a runtime promise that an old executable cannot open the directory.
No deployment, automatic backup, restoration or epoch transition is performed.

## Persistent inventory and forward behavior

| Owner | Changed data and forward behavior |
| --- | --- |
| Plugin registry | New manifest declarations are stored with approved packages and contribute to their digest. Absent contributions keep their existing serialized shape through serde defaults/skip rules. Existing API-v2 manifests remain the only supported old package form; no API-v1 path is added. Installed files are revalidated. A changed or invalid version stays on disk for review and cannot silently run under another approval. |
| Resources, grants and routes | New resource, grant and route records carry explicit instance/collection/parent identity and revisions. Encrypted resource fields remain in the existing vault envelope. Existing instance grants are not converted into broad resource grants, and no agent receives new rights automatically. |
| OAuth | New encrypted pending/completed authorizations, account/external subject links and generation records bind owner, package, resource and requester. Setup authorizations expire with their root and are promoted only in activation's transaction. External-consent event receipts contain only the pending key and requested scopes, are created with the pending consent, and retire atomically with their native event (including instance deletion); a superseded replay cannot acquire a fresh consent. Provider-consumed codes and rotated refresh tokens cannot be rolled back by restoring local JSON. |
| Setup | Expiring root drafts and inert resource namespaces retain exact approval/owner provenance and TTLs. Activation atomically publishes original resource IDs and finished owner credentials; pending consent is never promoted. Discard and deletion retire corresponding secrets and references. |
| Conversation and execution | Per-message requester proofs, shared-source bindings, generation/revision fences and new-turn bookkeeping preserve each author's authority through queue/preparation/wake. Existing un-attributed messages never gain another person's personal OAuth. An authenticated account captured while Beacon is unavailable has no personal membership-generation proof; later recovery cannot upgrade that turn. Verified proof records retain their existing serialized generation string. Exact owner-answer provenance is stored under the existing chat-control KV prefix, bound to the run, initiating turn and answer; edits retire it and conversation deletion clears that prefix. It permits only that answer's delivery and never restores suspended personal credentials. Older execution code cannot be used to resume work that depends on these checks. |
| Clients | Android's ordinary setup-input snapshots are local, backup-excluded and bounded. They contain no declared secret fields; session/scope retirement clears them. They are disposable editor state, not installation data or an offline mutation queue. Web state remains browser-local. |

No SQLite schema, existing storage envelope, installation identity or old package
file is rewritten merely to add these declarations. Forward installation opens
the existing store and validates packages normally; new records are written only
by explicitly authorized configuration/admission. Existing integrations must be
checked with the candidate before a separately reviewed transition. This does
not certify every future Slack format or every previous production binary.

## Separately reviewed transition and recovery

Before release, inventory the served epoch and installed plugin versions, verify
current package validation and approval digests, and qualify existing agents,
conversations, encrypted credentials and the unchanged package paths. Reconcile
upstream compatibility metadata when main is integrated. Preserve the existing
installation directory and private identity; never place credentials in reports.

Use a validated matched Beacon/installation pair. The ordinary production gate
must remain blocked until the epoch transition and its forward recovery procedure
have been reviewed. Do not lower the epoch, move `legacyTree`, silently rewrite
manifests or reapprove a package to bypass that gate.

If validation or rollout fails, stop new rollout/admission, preserve the current
data and deploy a reviewed forward fix. Repair an incompatible package as a new
version under its existing ID, explicitly review/approve it, and retain its
instance/resource IDs and secrets. Reconnect a provider after an uncertain
exchange or interrupted rotation; do not replay a consumed code/token. Restoring
an earlier image is not a supported recovery once epoch-7 data has been written.
No automatic data restore point or reversible provider-side transaction exists.

## Evidence and remaining qualification

The old-reader manifest failure follows directly from the base parser and the
new registry's persisted declarations. Existing native package/resource/OAuth/
activation/turn suites exercise forward behavior, while browser and Android
journeys qualify client boundaries separately. Record exact results and commit
identities in the PR; fixtures do not establish a live-provider migration.
Full storage measurements, prior-production-data qualification, actual Slack
validation and the final persistent-format inventory remain delivery gates.

See [production compatibility](PRODUCTION-CAIRN.md#installation-data-compatibility),
[plugin contract](PLUGINS.md), [Slack delivery](SLACK-PLUGIN-IMPLEMENTATION.md),
and [OAuth lifecycle](SLACK-TOKEN-LIFECYCLE.md).

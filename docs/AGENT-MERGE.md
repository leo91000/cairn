# Merging agent profiles

Use `merge_agents` to consolidate two agent profiles, for example Claude
into the main Omni profile. Discover its schema through MCP `search`, then call it
through `execute` on the public management MCP or `cairn_installation`:

```text
return await cairn.merge_agents({
  sourceId: '11111111-1111-4111-8111-111111111111',
  targetId: '00000000-0000-4000-8000-000000000001',
  confirm: true,
});
```

The equivalent owner API is `POST /api/agents/{sourceId}/merge` with
`{ "targetId": "...", "confirm": true }`. Members cannot call this route;
external MCP clients need `manage`. The internal tool rechecks the calling run's
installation grant inside the merge transaction, after waiting for worker and
database locks. There is no new web or Android control in this change.

The transaction transfers every source task, including archived tasks, and every
conversation, including trash. It changes stored run profile IDs so failed or
stopped sessions can still resume. Run IDs, messages, events, workspaces, selected
engine, model, instructions and effective access are preserved. Legacy access
snapshots are made explicit before changing their identity. The destination
profile, its defaults, credentials and portrait remain unchanged. Future task
runs use the destination's defaults; existing conversations keep their last
engine and model unless the user chooses another one. Subsequent turns use the
destination's current access and preapproved live grants. The source profile,
portrait and redundant live grants are removed. An `agent.merged` audit entry
records the source, destination and transferred task/chat/run counts.

The operation requires two existing, different profiles. Main can only be the
destination. Before changing anything it refuses:

- A source with queued, running, recovering or still settling execution, including
  a live backend conversion after its worker has stopped. Wait
  for it to finish and retry; a merge never cancels work.
- A destination that does not already cover the source's access, node selection,
  plugin grants (including GitHub credentials) and exclusive node grants. Grant
  access explicitly first.
- A destination missing its own owner confirmation for an administrator node the
  source is approved to use. Confirm the destination's access to that host first;
  merging never transfers administrator approvals automatically.
- Any plugin delegation still pointing at the source, even when disabled.
  Reassign and reapprove it using the normal owner workflow first.

A rejected request leaves all references and permissions unchanged. A successful
request returns `sourceId`, `targetId`, `deleted: true` and `transferred` counts.
The source no longer exists, so an identical retry returns a not-found error;
read the destination and references before retrying an ambiguous timeout.

## Data compatibility

The installation data epoch remains unchanged. This operation writes only
existing record fields and IDs through the current store contracts. It adds no
record format, identity alias, schema, secret envelope or checkpoint format.
A build using the same data epoch reads the transferred records and can resume
their sessions; it simply does not expose the merge operation. The audit records the original
profile identity, while saved snapshots keep the original name and settings.

Integration coverage exercises the owner API, both MCP interfaces and their
authorization, archived and trash references, session resumption and reopening
the database, retained live grants, blocked cases and transaction rollback.

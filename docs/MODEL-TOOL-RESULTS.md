# Per-call model results

Cairn limits each complete encoded MCP `tools/call` result to **1 MiB**
(1,048,576 bytes). This includes JSON escaping, content blocks, structured
content, protocol metadata and the truncation notice. Existing smaller producer
budgets, including 256 KiB history/content pages, still apply.

The limit formats the result of one completed call. It does not repeat the
operation or undo its effects. This result budget counts each call separately.
Successful calls remain successful; failed calls
retain `isError`. The JSON-RPC request identity stays unchanged. Protocol and
transport failures keep their separate unknown-outcome semantics.

Results within the budget are returned unchanged. An oversized result has a
model-visible notice and structured `truncated=true`, `retained=false` metadata.
Complete content blocks are kept while they fit; a text block can become a
UTF-8 prefix preview. Binary, media and resource blocks are kept whole or
omitted. Cairn never slices serialized JSON or base64 to meet this limit.

`originalResultBytes` counts the original complete encoded result when known.
Code Mode can discard an oversized successful return value in its isolated
worker, before sending an internal result frame; its `originalValueBytes` counts
only that encoded value. It does not claim to know the omitted final envelope's
size. Code Mode's deadline, CPU, heap and call-count limits are unchanged.

Cairn creates no raw-result artifact, spool or new readback grant. A truncation
notice must not be interpreted as a promise that the omitted data is available.
Existing explicit paginated readers and immutable publications keep their
existing authorization and quotas. Do not repeat a side-effecting operation to
recover its omitted result.

Opaque optional metadata and extension fields are kept whole when they fit.
When necessary, whole oversized fields are omitted and `metadataOmitted=true`
is reported. Small authentication challenges survive alongside omitted large
siblings. An oversized challenge itself can be omitted, so its original
authentication hint is unavailable; the original error outcome remains.
Oversized mandatory routing metadata, or a non-object result, is an
unsupported protocol result with a fixed bounded error and unknown outcome.
Cairn does not manufacture a replacement challenge or infer new permissions.

## Discovery compatibility

Owned output schemas advertise an original-or-truncated union. The twelve
workspace schemas use the verified generated draft07 profile: root
`definitions` and `$schema` stay at the root, preserving their local
`#/definitions/...` references. Search and execute have simple object schemas.
The source schema corpus checks this profile and complete/truncated values.

For external tools, only the **advertised gateway copy** withdraws the optional
`outputSchema` and explains why in its description. This deliberately reduces
schema precision for gateway clients rather than claiming a truncated object
satisfies an arbitrary upstream schema. Stored/upstream canonical descriptors,
input schemas, names, capabilities and grants are unchanged. No schema is
resolved over the network or rewritten across dialects, IDs, root references,
anchors, recursive references or external resources.

The adaptation occurs at HTTP `tools/list` response delivery. Direct MCP
discovery/storage, typed operation descriptors inside Code Mode's `search`, and
the source catalogs retain their canonical schemas. The workspace listing adds
its granted plugin/delegation tools before the adaptation; authorization and
tool filtering still happen in their original layers.

The advertisement boundary requires an object catalog, a tools array, and
object descriptors with nonempty string names, object input schemas and
optional string descriptions. Malformed catalogs produce a fixed bounded
protocol error rather than a partial advertisement. External output schemas
remain opaque; this validation neither resolves them nor infers permissions.
Gateway shape validation precedes the existing grant filter, so a malformed
unlisted entry also rejects the catalog without revealing its name or details.
Policy annotation and schema withdrawal occur only on the filtered response.

## Coverage

This policy covers Cairn's owned `/mcp`, `/mcp-installation`, `/mcp-workspace`
and proxied `/mcp-gateway/{id}` call results, plus Code Mode's final return.
Code Mode's hidden intermediate values keep their independent worker wire
guards. Existing upstream MCP wire ceilings remain in force: a response rejected
before parsing needs a semantic producer/streaming solution, not raw JSON cutting.

Native Codex, Claude Code and OpenCode builtins, and command MCP connections
configured directly as stdio inside those coding agents, are outside this
coverage. Cairn's activity adapters operate after model consumption and do not
enforce this model-facing budget for those paths. The separate
16 KiB timeline preview and native/VM canonical transport limits are unchanged.
This change does not establish an unacknowledged transport window or remove a
legacy lifetime transport cap.

Authorized credentials returned to a model retain their existing permissions.
Timeline/log redaction remains a separate policy; this formatter neither logs
payloads nor globally redacts authorized model data.

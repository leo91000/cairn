# Slack foundations: installation-data compatibility

Status: implementation review for draft PR #226, **not deployment approval**. The
Slack package and final qualification remain unfinished. Do not merge or deploy
this draft. This inventory must be revisited as later lots add persistent data.

The integration with Main's retry-aware replies retains an optional canonical
`reply.status` (`completed`, `failed`, `cancelled`) in a frozen predecessor range.
The existing `reply.failed` boolean is retained, and an earlier marker without
`status` still reads through that boolean without searching later turns. New
ranges use the same terminal selector as live replies before the next requester
can start; they retain the final attempt and its stop status. This adds no key,
table, index or envelope and remains inside the unqualified epoch-13 / SQLite-12
boundary below. The higher boundary, forward transition and recovery gates stay
required; no deployment or downgrade is qualified by this change.

## Compatibility boundary

The plugin foundations introduce an incompatible persisted plugin contract. A
manifest containing `webhooks`, `resources`, `operations`, `oauth`, or an operation
binding on a command/tool is rejected by the earlier `deny_unknown_fields`
manifest parser. An older host also lacks the requester and resource-authority
checks required by newly admitted work. Treat starting that host on this data as
an unsafe downgrade, even when some JSON records are syntactically readable.

The installation epoch advances to **13**, with SQLite `user_version` **12**.
Main through `1e735587` already reserves epochs 5–7 for restart/native storage,
epoch 8 for GitHub's plugin migration, and epoch 9 / SQLite 8 for the reviewed
automatic transition. Main `64d92fe9` then uses epoch 10 / SQLite 9 for mission
waits and their active-slot index. Main through `0a46294c` then reserves epoch 11 /
SQLite 10 for native concurrency and node access. Main `d81a3a87` adds guided
plugin setup at epoch 12 / SQLite 11. Integrating it raises this unreleased
branch's previous 12/11 reservation to 13/12, preserving a distinct higher boundary. `legacyTree` remains
unchanged. Opening the candidate advances SQLite before service startup; earlier
binaries reject that newer database instead of resuming work without Slack's
requester checks.

The historical automatic migration to epoch 9 remains documented in
[AUTOMATIC-MIGRATIONS.md](AUTOMATIC-MIGRATIONS.md). Its evidence does **not**
qualify this new boundary, nor does main's mission-wait transition. This draft declares no `automaticMigration`, so the
production release gate refuses any earlier epoch. Final delivery must qualify
and review a separate forward transition before enabling automatic deployment.
No deployment, automatic backup, restoration or production transition is performed.

## Persistent inventory and forward behavior

| Owner | Changed data and forward behavior |
| --- | --- |
| Plugin registry | New manifest declarations are stored with approved packages and contribute to their digest. Absent contributions keep their existing serialized shape through serde defaults/skip rules. Existing API-v2 manifests remain the only supported old package form; no API-v1 path is added. Installed files are revalidated. A changed or invalid version stays on disk for review and cannot silently run under another approval. |
| Resources, grants and routes | New resource, grant and route records carry explicit instance/collection/parent identity and revisions. Encrypted resource fields remain in the existing vault envelope. Existing instance grants are not converted into broad resource grants, and no agent receives new rights automatically. |
| OAuth | New encrypted pending/completed authorizations, account/external subject links and generation records bind owner, package, resource and requester. Setup authorizations expire with their root and are promoted only in activation's transaction. External-consent event receipts contain only the pending key and requested scopes, are created with the pending consent, and retire atomically with their native event (including instance deletion); a superseded replay cannot acquire a fresh consent. Provider-consumed codes and rotated refresh tokens cannot be rolled back by restoring local JSON. |
| Setup | Expiring root drafts and inert resource namespaces retain exact approval/owner provenance and TTLs. Activation atomically publishes original resource IDs and finished owner credentials; pending consent is never promoted. Discard and deletion retire corresponding secrets and references. |
| Conversation and execution | Per-message requester proofs, shared-source bindings, generation/revision fences and new-turn bookkeeping preserve each author's authority through queue/preparation/wake. Existing un-attributed messages never gain another person's personal OAuth. An authenticated account captured while Beacon is unavailable has no personal membership-generation proof; later recovery cannot upgrade that turn. Verified proof records retain their existing serialized generation string. Exact owner-answer provenance is stored under the existing chat-control KV prefix, bound to the run, initiating turn and answer; edits retire it and conversation deletion clears that prefix. It permits only that answer's delivery and never restores suspended personal credentials. Older execution code cannot be used to resume work that depends on these checks. |
| Issued question controls | Nonsecret plugin-storage values retain their `preparing`/`posting`/`sent` write intent, canonical fields/partial selections and exact delivery/person/provider-message binding across worker restart. Optional host-derived `deliveryId`/`chatId` fields bind their lifetime; all delivery-removal paths delete them atomically, including source revocation and conversation purge. Ordinary instance values and quotas keep their existing format. A queued interaction's existing `nextAttemptAt` also represents a definite provider cooldown without spending unexpected-error attempts. Native RPC question IDs include the provider turn as well as thread/item; reopening a session withdraws old requests and a new turn cannot inherit their answers or withdrawals. `blockingQuestion` is derived adapter inbox input, not a stored message field. Unknown POST outcomes are not silently replayed; unsupported/secret questions stay in Cairn. These additions remain inside the unqualified epoch-13 draft boundary. |
| Ordinary progress controls | One delivery-owned `progress:<deliveryId>` plugin-storage value retains an eight-second `readyAt` wall deadline, `preparing`/`posting`/`sent` intent, exact source/person/delivery/message binding, confirmed short native step label and optional `updateStatus = sending`. Only definite quota rejection clears the relevant in-flight intent for retry. The confirmed Stop binding remains usable after an uncertain edit; an uncertain initial creation is not replayed. Final reply handling deletes the reference before optional provider cleanup; native delivery removal still retires it atomically. Values use the existing store envelope and quota; no new table, index, native capability or credential shape is introduced. Epoch 13 / SQLite 12 and the unchanged legacy tree remain the draft boundary. [Local measurements](performance/SLACK-PROGRESS-STATE.md) compare the representative full record with ordinary instance state. |
| Clients | Android's ordinary setup-input snapshots are local, backup-excluded and bounded. They contain no declared secret fields; session/scope retirement clears them. They are disposable editor state, not installation data or an offline mutation queue. Web state remains browser-local. |

Saved-root OAuth renewal in resource forms reuses the encrypted credential key
and per-instance lock. Root preview/save/resource-form owner authority and original
package approval remain runtime snapshots. Encrypted root `oauth` metadata now
has an optional `refreshing = true` consumed state, committed with `refreshToken`
removed before provider I/O. A confirmed, authorized response replaces that state
with the ordinary credential shape. Missing flags on older records retain normal
behavior; an interrupted state requires reconnection. Earlier readers also refuse
to refresh a record with no refresh token, rather than replaying it. Consumption
and completion compare the prior credential in the authorization transaction.
No table, key count, encryption envelope, epoch, legacy tree or migration changes;
the additive metadata stays inside this unreleased epoch-13 / SQLite-12 draft.
The [native record observation](performance/ROOT-FORM-CREDENTIAL-RENEWAL.md)
includes all three states. Inert root OAuth activation and whole-PR production
transition/recovery qualification remain open.

Slack workspace settings now retain the nonsecret app ID verified by `bots.info`.
Operational resource-authenticated requests compare it with the worker's trusted
root app and workspace before any token-bearing I/O. A missing or different app
ID is refused, including a root edited before activation. Older workspace rows
are not assigned the current root app automatically: the owner must reload,
preview and save their workspace so its bot is checked again. The existing vault,
resource IDs, routes, table layout and epoch-13 / SQLite-12 draft boundary remain.
This is an authority restriction inside the unreleased candidate, not a qualified
production migration or permission to run an earlier feature-branch binary.
[Local resource measurements](performance/SLACK-WORKSPACE-APP-BINDING.md) record
the added field's bytes and store timings separately from the operational RPC.

Resource-form events also expose the previous nonsecret settings from their
existing revision snapshot (`resource.settings`, null on creation). This additive
API-v2 event field introduces no stored field, read, key, table or envelope; it
reuses the resource already loaded for form authorization. Slack compares that
trusted workspace ID with the provider's identity before preview/save, preventing
submitted hidden values from moving an existing resource's rules to another
workspace. Same-workspace token renewal and creating a distinct workspace resource
remain supported. No automatic identity migration or production transition is
implied.

Installed-package approval reads retain the same files, manifest parser, digest
and original approval snapshots. Synchronous loading/validation now uses the
blocking pool so nested OAuth forms do not exhaust their default stack. No new
record, encoded payload or write is introduced; [local read/write and CPU
samples](performance/PACKAGE-APPROVAL-READ.md) compare the same loader before and
after this scheduling boundary. Effect/commit approval checks remain mandatory.

The SQLite version fence advances; the existing application tables, storage
envelopes, installation identity and old package files are not rewritten merely
to add these declarations. Forward installation opens the existing store and
validates packages normally; new records are written only
by explicitly authorized configuration/admission. Existing integrations must be
checked with the candidate before a separately reviewed transition. This does
not certify every future Slack format or every previous production binary.

Remote-choice fields add a boolean inside the existing untyped settings schema;
earlier API-v2 form readers retain it while ignoring its semantics. Lookup query,
cursor and page/echo metadata use the existing opaque configuration-operation
payload/result, with its existing lifetime; no new stored kind or envelope is
added. A recovered old-host operation may produce a full-form result instead;
new clients reject its missing lookup echo. This addition therefore keeps the same Slack compatibility boundary
and the legacy tree unchanged. It does not relax the already incompatible
pre-Slack downgrade boundary or authorize a production transition.

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
an earlier image is not a supported recovery once Slack data has been written.
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

## Pending questions from before turn attribution

Base-release question records do not identify the turn that issued them. A run
and its conversation may already have advanced to another author or turn, so
neither the current execution nor a matching run ID proves that attribution.
The candidate does not assign those questions the current turn's rights.

Before workers or plugin delivery start, a one-time store transaction retires
unattributed open questions (`pending` and `answering`). It keeps their fields,
history and unknown properties, marks them cancelled/nonblocking with
`retiredReason = "missing-turn-attribution"`, and cancels only their exact
still-queued/sending answer messages. Already delivered answers, closed questions
and questions with an explicit turn ID remain unchanged. A single visible error
notice in each affected conversation instructs the owner to ask the agent to
repeat the questions; it contains no question or answer content and does not
change the run status. Ordinary cancelled-question delivery withdraws a matching
outstanding provider prompt. Retirement, the notice, audit and the one-time
`migration:question-turn-attribution` marker commit together, so another startup
cannot repeat the action.

The separately reviewed epoch transition must inventory these open legacy
questions and their queued answers. The owner may answer or withdraw them with
the served version before transition; otherwise the candidate uses the visible
retirement above. Preserve the original store and identity. A stored fixture
regression exercises restart, exact answer cancellation, unchanged attributed and
closed questions, content-free recovery instructions and idempotence. It does
not replace qualification against actual prior production data. The
[storage-shape observation](performance/QUESTION-TURN-MIGRATION.md) records the
bounded additional records and its measurement limits.

## Socket slice and optional queued-work hint

The first Socket slice adds no credential envelope or persistent socket ticket.
Temporary connection URLs and the event payload budget remain in memory. Its
optional `eventTimeout` connect argument is runtime input, not a stored manifest
field. Slack settings add the optional `transport` selection and `appToken` secret
using the existing settings/encrypted-secret formats. Existing HTTP settings still
select HTTP when the transport field is absent.

A new inbound `startsWork = true` hint adds the optional same-name boolean to the
native queued-event record. It is absent for controls and existing packages. The
new reader defaults absent hints to false; the previous event reader ignores the
unknown top-level property and processes the existing name/payload/revision fields.
No authority is derived from the hint. Limits remain mandatory at submission;
source, route, person, owner and approval proofs still apply. Capacity postponement
updates the existing `nextAttemptAt` field without incrementing error attempts or
rewriting identity. It never recreates a deleted or changed admission. Earlier
readers can process these records but retain their earlier finite-retry behavior.
The additive hint alone therefore does not change epoch13 / SQLite12 or the legacy
tree. The full-PR inventory, forward migration/recovery and final qualification
remain required. See the separately recorded pending-event storage observation.

## Accepted native continuation delivery

`plugin-deliveries` gains an optional `continuationOf` input ID. Absent values keep
ordinary delivery behavior. A native wait reserves one row under its wakeup ID,
retaining the original instance/revision, resource/route/person proof and reply
metadata, without copying per-reply retry state or a predecessor's checkpoint.
Repeated registration keeps the same ID; replacement removes the previous unused
reservation in the same transaction. The existing 20-row ceiling includes these
reservations, and failed reservation rolls back the wait. Progress persistence
compares its original row before updating, preventing a delayed callback from
recreating a removed reservation. Activity follows the logical task's newest
reservation. Existing new-turn intent is committed before native admission, so
an independent external contribution queues behind the task without cancelling
its wakeup; its serialized shape is unchanged. Existing revision/deletion cleanup applies to all its rows.

A `chat.wakeup.delivered` event stores only `{ messageId }` at the actual native
acknowledgement, requiring both the current execution ID and exact `chat.wakeup`
launch provenance. Repeated acknowledgements do not create another boundary.
Ordinary human message rows and events keep their shape; no synthetic human row
is inserted. Reply selection and frozen predecessor ranges recognize both input
receipt kinds. Clients explicitly omit the internal receipt from presentation.

Accepted delivery retention now follows native work rather than a 24-hour age
cutoff. Fresh-message age policy is separate. No pending limit, authority fence or
finite handler-error budget is removed. Earlier development snapshots' already
settled/orphaned deliveries are not reconstructed from arbitrary later output;
start a new source request when no forwarding record remains.

These are additive records inside the already incompatible, unreleased epoch-13
/ SQLite-12 contract above; no additional table, vault envelope, transition,
automatic migration or legacy-tree change is introduced. Pre-Slack downgrade and
production deployment remain fenced. This is a checkpoint inventory, not final
whole-contract compatibility qualification. Local overhead is recorded in
[continuation measurements](performance/SLACK-CONTINUATION-DELIVERY.md).

## Root OAuth on inert setup drafts

Root consent reuses the in-memory credential flow, existing setup metadata and
encrypted credential key/envelope. It introduces no persisted field, key family
or table. A completed root code-grant record has the existing OAuth shape; refresh
uses the existing consumed-intent metadata inventoried above. Both intent and
completion apply the original setup TTL through the existing KV expiry column.
Activation removes that TTL in its existing promotion transaction. Client/scope/
secret edits retire the saved credential without changing the setup metadata
shape or extending the draft deadline. No token is returned to a client.
Earlier draft OAuth credentials containing only `{token}` remain stored, but
forms and activation require new consent instead of promoting a pasted token.
Activation also refuses a concurrent consumed intent in the promotion transaction.

Older normal OAuth records remain unchanged. Earlier development readers lack
the setup sign-in endpoint and have incomplete root OAuth activation/renewal;
they are not a supported way to operate this draft feature. The already
unreleased epoch-13 / SQLite-12 boundary and legacy tree stay unchanged, and this
inventory does not qualify production transition, physical recovery, downgrade,
deployment or release. [Local measurements](performance/SETUP-OAUTH-EXPIRATION.md)
separate encoded-value sizes from KV expiry and production latency. Web/Android
sign-in and equivalent public-MCP/agent-tool mutations remain required before
complete delivery.


Resource configuration MCP parity reuses the HTTP resource representation,
encrypted secret values, OAuth records and cleanup transactions. Delegated
configuration retains additional in-memory run/requester/instance authority;
owner-only calls retain their existing storage reads and writes. The credential
guard adds authorization reads for delegated calls, not a persisted cache or
format. There is no new table, key, serialized field, envelope, epoch or migration.
The unreleased epoch-13 / SQLite-12 candidate still requires whole-PR forward
transition and recovery qualification before any production migration.

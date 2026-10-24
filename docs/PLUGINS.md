# Installation plugins

A plugin is a package: a `manifest.json` or `manifest.toml`, a `main.luau` and, optionally, other Luau
modules it loads with `require`. Cairn runs each package
in its own long-lived `cairn plugin-worker` process and reaches it only through
events; the package reaches Cairn only through host capabilities its manifest
declares and the owner approved. Plugin code never receives `Service`, SQLite, the
vault key or a connection's credential, and identities always come from the host.

Cairn has no code specific to a plugin. Discord's Gateway session, its setup form,
its panel and its test are Luau, like 1Password's commands and tools and GitHub's
sign-in, run credentials, project and skill sources and CI waits: a package written
outside this repository can do everything the bundled ones do.

## Code layout

```text
crates/plugin-api/      Manifest, capability list, frames between host and worker
crates/plugin-host/     Worker processes: sessions, per-event deadlines, capability gate
crates/plugin-runtime/  Luau sandbox, handler table, prelude (cairn.storage, http, …)
crates/installation/src/plugins/
  registry.rs           Bundled and installed packages, approvals, versions
  connections.rs        Generic instances: settings, secrets, agent grants, delegation
  runtime.rs            One supervisor per instance that needs a worker: socket, pump
  socket.rs             `socket`: a package's own WebSocket, driven by its transport worker
  forms.rs              `instanceForm`: the package completes the owner's instance form
  device_flow.rs        `credential.deviceFlow`: the host signs the owner in (RFC 8628)
  run_credentials.rs    `runCredentials`: files and Git credentials in granted agents' runs
  projects.rs           `projectSources`: projects the owner imports through an instance
  conditions.rs         `conditions`: what granted agents wait for with `wait_for`
  capabilities.rs       storage, http, timers, exec, inbound and conversations for scripts
  scope.rs              One-shot handlers: who acts, which instances, rechecks
  agents.rs             Commands, Cairn MCP tools and prompt instructions for agents
  panels.rs             Declarative views, actions and tests for the owner
  programs.rs           `exec`: declared programs without a shell
  work.rs               Admission, access requests, event queue, replies
  cli.rs                `cairn plugin <plugin> <command> …` inside conversations
  legacy.rs             Moves earlier 1Password accounts into instances
  legacy_github.rs      Moves the earlier GitHub connection and tokens into instances
crates/installation/src/skills/sources.rs
                        `skillSources`: skills imported from a package's source
plugins/                Bundled packages (1Password, Discord, GitHub), embedded by build.rs
packages/plugin-types/  Luau declarations for package authors
apps/web/src/           Catalogue, review, generic plugin page, form and view renderers
```

Dependencies point one way: `installation → plugin-host → plugin-api ← plugin-runtime`.
The host and runtime know nothing about Cairn's storage or agents; installation
supplies the callback that turns an approved capability call into an effect.

## Packages and the registry

Bundled packages are embedded in the binary and always active. Installed packages
live in `<data>/plugins/<id>/<version>/`, either uploaded from Atelier → Plugins
(“Install package”: the files, or the whole folder) or copied there by the owner. A
version runs only after the owner approves it. Approval records a digest of the exact
manifest and every Luau file, so any later edit on disk, an added module or a link
withdraws the package until it is approved again.

### Files and modules

```text
plugins/discord/
  manifest.toml        or manifest.json; same validated contract
  main.luau             returns the handler table, nothing else
  gateway.luau          socket: the Gateway session
  setup.luau            form, test and view
  conversation.luau     message, interaction, progress, question, reply…
  lib/discord.luau      REST calls, reactions, identities
  lib/text.luau         mention removal, UTF-16 lengths, chunks
  lib/permissions.luau  Discord's permission resolution
```

`main.luau` is the entry point and returns the table of handlers. Other `.luau` files
are modules: `require("./gateway")` or `require("../lib/text")` resolves relative to
the requiring file, Luau style, to `name.luau` or `name/init.luau` inside the package.
Each module runs once per worker, in its own environment over the frozen globals, and
returns a value (usually a table); cycles, paths above the package and anything else
(`@aliases`, the disk) are refused. Names are lowercase segments (`a-z`, `0-9`, `-`,
`_`), four levels at most; a package has at most 64 modules and 384 KB of Luau in
total. Workers receive every file when they start; nothing is read from disk at run
time. Each worker of an instance (transport, events, one-shot handlers) loads its own
copy, so module state is never shared between them. Small packages can stay in one
`main.luau`, like 1Password. A new
version waits for approval while the previous one keeps running, and the owner can
switch back to any earlier approved version. Installed packages can't reuse
built-in IDs, and nothing is reserved to built-in packages: 1Password is an
ordinary package. Uninstalling requires removing the package's instances first.

A manifest declares `"apiVersion": 2` (the only version Cairn loads) and:

- `capabilities`: `conversations.submit`, `conversations.control`, `storage`, `http`, `files`,
  `timers`, `exec`, `socket` and `inbound`;
- `hosts`: the hostnames `http` (HTTPS) and `socket` (WSS) may reach. `*.example.com`
  covers every subdomain of `example.com`, not the domain itself;
- `credential`: how Cairn adds an instance's token to authorized requests
  (header and prefix); the script only sets `auth = true`. With `deviceFlow`, the
  owner can also sign in instead of pasting it: see [Device sign-in](#device-sign-in);
- `settings`: the instance form, in order (`key`, `title`, `type`, `required`,
  `hidden`, `description`, `placeholder`, `section`, `default`, `maxLength`,
  `minimum`, `maximum`, `pattern`, `options`, `dynamic`, `remote` and `refresh`).
  Types: `string`, `number`, `boolean`, `text`, `url`, `select`,
  `multiselect` (with `options`, or `dynamic: true` for choices the package's form
  handler provides) and `secret`;
- `instances`: `{ singular, plural }`, how the interfaces name an instance;
- `brand`: `{ color: "#rrggbb", path }`, the package's mark (an SVG path on a
  24×24 grid);
- `programs` (with `exec`): `[{ name, path?, installer?, description }]`; exactly
  one of an absolute administrator-installed `path` or a package-relative Luau
  `installer`. Installers also require `http` and declared `hosts`; see below;
- `agentAccess`: `{ description }`; the owner grants instance-wide access on each
  agent's page (`plugins`), or individual resource operations on the resource. Only
  granted agents receive the corresponding commands, tools and instructions;
- `commands`, `tools`, `instructions` (need `agentAccess`), `instanceView`,
  `instanceTest` and `instanceForm`: see [Contributions](#contributions);
- `runCredentials` (needs `agentAccess`), `secretPatterns`, `projectSources`,
  `conditions` (needs `agentAccess`) and `skillSources`: see
  [Integrations](#integrations).

## Connections and delegation

Scripts receive validated settings in their `start` event; credentials stay in the host.

Every plugin uses the same connection model: name, settings, optional credential
(encrypted in the vault), and, when the package can start conversations, an owner
delegation: one dedicated agent (one project at most, no installation access), the
people allowed (listed, approved as they ask, or anyone), and an hourly message limit.
“Anyone” is meant for public channels such as customer support: it requires an agent
with a read-only sandbox and no installation, MCP or plugin access (GitHub, 1Password…)
(rechecked before every effect, so a later grant suspends the connection), adds a
per-person hourly limit, and lets only listed people or a conversation's starter
answer for it or stop it. The
connection is bound to the owner's current access and the agent's recorded policy.
External plugin identities do not supply the Cairn origin required by
[trusted native execution](NATIVE-NODES.md). Delegation configuration and route
eligibility apply that native policy without borrowing the configuring owner's
host permissions. Agent tool grants remain a separate contract.

The web and Android render this delegation, the people allowed and pending requests,
hourly usage and recent activity for every package with `conversations.submit`.
Routes are generic: `/api/plugins/{id}/connections[/{connection}]` with `enabled`,
`test` and `people/{user}/{approve|deny|remove}` actions, `/api/plugins/{id}/form`, and
`/api/plugins/packages[/{id}/approve|activate]` for the registry. Approval includes
the digest displayed during review; changes require reviewing again.

Owner package reads also have separate bounded contracts:

- `GET /api/plugins/packages?cursor=…`: up to 32 compact cards, with a last-ID
  `nextCursor`, `packageReads: true`, `instanceReads: true`, `accessReads: true`, `agentReads: true`, setup support and capability descriptions.
  A card's `presentation` contains labels, its brand, capability names and
  contribution counts; it is deliberately not a complete manifest. Cards include
  neither settings, credentials, instance configuration nor version history.
  Each card also has a volatile `summary`: configured instance count, counts by
  live state (paused instances remain paused), pending request count and distinct
  agent IDs with saved instance or resource rules. `observedAt` dates this single
  SQL snapshot. Saved rules include inactive grants and do not prove usable access.
- `GET /api/plugins/packages/{id}`: the selected display manifest and package
  approval status, without all version manifests. An approved active version is
  preferred, otherwise the last valid version in lexical order is displayed.
  Served configuration support is explicit: `instanceReads`, `accessReads`,
  `setupDrafts` and capability descriptions.
- `GET /api/plugins/packages/{id}/versions?cursor=…`: up to 32 version metadata
  records in lexical order, with a last-version `nextCursor`. Digest, approval,
  source byte/file counts or an invalid-version diagnostic remain available.
- `GET /api/plugins/packages/{id}/versions/{version}`: the exact manifest, digest
  and file list to review before approval. The existing approval endpoint still
  checks that digest against the current files. Invalid local versions return a
  diagnostic; missing packages or versions return 404.
- `GET /api/plugins/packages/{id}/connections?cursor=…`: up to 32 full owner
  instance configurations, selected by package and UUID cursor before decoding.
  Status, root hourly usage, pending requests (including route requests) and the
  last eight activity entries share a read transaction. The encoded-byte limit
  can end a page before its count limit. This response advertises `instanceReads`.
- `GET /api/plugins/packages/{id}/connections/{instance}`: one exact owner
  configuration with the full current agent ID list and the same live adornments.
  This supports grant-preserving saves, including paused or unapproved instances.
  Missing source files do not hide saved configuration. Neither read exposes
  vault contents or relaxes the checks on saving or executing an instance.

- `GET /api/plugins/instance-access?cursor=…`: up to 32 compact saved instance
  identities, delegated agent ID, diagnostic status and root/resource agent IDs
  across packages. The query projects these
  fields before decoding the result in Rust; it returns no settings and does not
  load credential or activity records. SQLite still reads the instance JSON.
  Paused and unavailable packages remain represented. Resource memberships are
  distinct from root grants. Display names use the bundled name or stable package
  ID; the selected package uses its complete manifest name. These saved rules
  support owner labels and public-agent restrictions; native checks still decide
  every write and effect.
- `GET /api/plugins/agent-grant-counts?cursor=…`: up to 32 saved resource-rule
  totals keyed by agent UUID. Rules whose instance, resource or package is no longer
  available still count. An absent agent in a completed list has zero saved rules;
  a failed or unsupported read is unknown, never an inferred zero.
- `GET /api/plugins/{id}/connections/{instance}/resource-choices/{collection}?cursor=…`:
  up to 32 resource identities, names, parent IDs and enabled flags for the chosen
  collection. This read returns no settings or secret field names. The current
  instance and approved package are checked before projecting the page.

These lists also enforce an encoded-byte budget below half the relay body limit.
Package IDs and versions are the cursor domain; existing management lists retain
their UUID cursors. Live approval changes do not invalidate a later page's cursor.
The current complete `/api/plugins` response also advertises `packageReads: true`
and remains available for callers still migrating. Web and Android catalogue
cards now consume the compact pages; review pages load metadata, then only the
exact selected and active manifests. Selection changes or closing the review
invalidate pending reads; approval uses the digest of the files actually shown.
Earlier hosts fall back to their complete response when the new endpoint is
absent or the required support marker is absent. Failed or malformed supported
reads remain errors, rather than an empty catalogue. Package instance screens
read one selected manifest, complete all its instance pages and complete compact
access pages across packages. Public delegation checks include root and resource
rules, even on paused or other packages. Compatibility saves re-read one exact
instance with its full current root agent IDs; a deleted instance remains an
error. A missing package is confirmed using support markers from one compact
catalogue page, so uninstalling clears the selected screen without an aggregate
read. Agent/grant selector screens complete the compact card, saved-access and
per-agent rule-count pages before showing the directory. Only choosing an instance
loads its complete package schema for resource profiles and operations. Package
labels and agent-access help come from the manifest, including custom packages.
An older host falls back explicitly; failures on a host advertising `agentReads`
keep the selector unavailable rather than restoring a partial or stale directory.

The public management MCP and the opt-in installation tool expose these same reads
through `list_plugin_packages`, `get_plugin_package`, `list_plugin_instance_access`,
`list_plugin_agent_grant_counts` and `list_plugin_resource_choices`. Public callers
need the `read` scope; installation callers need their current revocable installation
grant, rechecked after reading. MCP pages target 256 KiB of encoded JSON. One
indivisible metadata record may use up to 768 KiB, so an instance's complete
supported saved membership still fits below the code worker's 1 MiB result
ceiling. Larger records return an explicit error. Both structured and text copies
remain within the relay limit. Follow `nextCursor` until absent. These operations
read saved metadata and do not grant plugin access, approve packages, or expose
credentials. The rest of the generic plugin management interfaces still require
separate MCP parity work; existing restrictions on installation agents changing
owner-only plugin grants and exclusive reservations remain enforced.
Status inspection still validates the installed versions of each selected package,
without retaining all their manifests in compact responses. No summary is persisted.

## Runtime

For each enabled instance of a package that reacts to people, connections or timers,
a supervisor starts the package's worker, a second worker driving its socket if it
declares `socket`, and a pump. Each worker loads the source once and keeps
its Luau state between events. Handlers run one at a time under a 35-second deadline
and a 16 MiB memory limit; a stuck or misbehaving worker is killed and restarted with
backoff. Workers inherit no environment, run in a private temporary directory, never
load bytecode, and have no files, processes, `print` or other network access.

Events, all durable until handled (retried up to five times):

| Event | When | Payload |
|---|---|---|
| `start` | each time the worker starts | `{ connectionId, settings }` |
| `socket` | in the transport worker: started, opened, a message, a tick, closed | `{ type, … }`, see [Sockets](#sockets-and-inbound-messages) |
| `message` (or another kind) | the package admitted something an allowed person wrote | `{ id, author, …context, …body }` |
| `interaction` | an allowed person pressed a button (Discord's admitted kind) | `{ id, author, customId, … }` |
| `access_requested` | someone new asked (approval mode) | `{ id, author, … }`, never content |
| `progress` | every ~8 s while the agent works | `{ deliveryId, conversation, state, step, reply }` |
| `question` | the agent asks a question (once each) | `{ deliveryId, conversation, question, reply }` |
| `reply` | the agent's final answer is ready | `{ deliveryId, conversation, text, status, files, reply, state }` |
| `timer` | a timer scheduled by the package fired | `{ id, name, payload }` |
| `command`, `tool`, `view`, `action`, `test` | one-shot handlers | see [Contributions](#contributions) |

A `reply` handler may return `{ retryAfter = seconds, state = … }` to be called again
later, for example after a platform rate limit. This scheduler delay is capped at
300 seconds. Persist a provider's longer cooldown and avoid its HTTP method until
the actual deadline, including when another delivery becomes due.

Accepted deliveries follow their native task rather than expiring solely after
24 hours. Native sleep and deferred condition waits atomically reserve a separate
delivery for the exact continuation input, preserving the source, requester and
reply metadata. Reservations share the existing 20-pending-delivery ceiling;
a full queue refuses the wait without committing it. Repeated registration is
idempotent, and replacement reuses the prior reservation's slot. Continuations
have fresh retry state/checkpoints and do not charge another incoming-message
quota. Cancelled or replaced inputs cannot export later conversation output.
`chat.wakeup.delivered` records the native input's actual acknowledgement once,
with its `messageId`; it is an internal output boundary, hidden by both clients.
Independent `newTurn` requests queue behind the current task and its scheduled
continuations, including when another participant submits them. Ordinary native
steering retains its existing cancellation behavior. The waiting and resumed
turns can each produce their own scoped reply.

A reply handler can call `cairn.delivery.checkpoint(value)` before an external
write. The current delivery's next `reply` receives `event.checkpoint`, including
after an exception or worker restart. It is separate from the return-only retry
`state`, limited to 64 KiB, and cannot name another delivery. The host checks the
exact instance, resource route, person and package approval before committing.
The record is removed atomically when the delivery settles, and on instance
deletion; it consumes no general plugin storage key. A pending write intent lets
a package stop an uncertain replay, without claiming external exactly-once writes.
This API belongs to `conversations.submit` and is unavailable in other handlers.
[Local record/read/write observations](performance/PLUGIN-DELIVERY-CHECKPOINTS.md)
compare its retention shape with permanent reply keys.

Concurrent workers can preserve a numeric deadline with
`cairn.storage.max(key, number)`: the comparison and update are one transaction.
It returns the retained maximum, refuses nonnumeric values and uses the existing
storage limits. Reserve required numeric keys before provider I/O; an update to
an existing key still fits when the general store reaches capacity. Keeping an
expired cooldown reservation also prevents an older successful call from erasing
a newer deadline. A package must refuse I/O if a required reservation cannot fit.

For issued controls, `cairn.storage.set(key, value, { delivery = deliveryId })`
ties the value to a pending native delivery. Create it in that delivery's
`question`, `progress` or `reply` handler. An original-author `message` or
`interaction` can update an already associated key for the same delivery. The
host checks the current instance, revision, route and original person atomically;
it refuses a foreign, missing or retired delivery and reassignment to another one.
Pass the option on every update so deletion cannot race with an unscoped recreation.
The value still uses the shared 1,000-key/64-KiB limits and contains ordinary
plaintext data. Keep secret questions and answers out of it. Delivery settlement,
revocation, continuation replacement, conversation purge and instance deletion
retire owned values without requiring the package to run a cleanup handler.

Queued event handlers (including `interaction`) may return `{ retryAfter = seconds }`
for a definite refusal before an effect, such as a provider cooldown. The host
retains that admitted event and its authority, schedules another attempt in
1–300 seconds, and does not spend its finite unexpected-error retry budget.
Longer provider deadlines remain the package's responsibility: preserve the
deadline and defer again until it expires. Rechecks still reject revoked sources.
This return must not blindly repeat an uncertain external write.

Capabilities exposed to scripts:

- `cairn.conversations.submit({ text, thread?, shared?, files?, reply? })`: only while
  handling `message`, on behalf of that message's author. The host applies quotas,
  deduplication and the conversation binding: by author and thread, or, with
  `shared = true`, one conversation per thread for everyone allowed. `files` are
  fetched from declared hosts (four, 10 MB each) and attached to the message. Writing
  again resumes a stopped conversation.
- `cairn.conversations.check({ conversation|thread })` checks the current person's
  control rights before collecting partial answers.
- `cairn.conversations.answer({ conversation|thread, question, answers })` and
  `cairn.conversations.cancel({ conversation|thread })` (capability
  `conversations.control`): while handling `message` or `interaction`, for an allowed
  person, only in conversations the connection started. Cancel works like Stop in
  Cairn. Private (secret) question fields can only be answered in Cairn.
- `progress.step` is a short description such as “Running a command” or “Editing
  files”; it never contains command lines, paths or output. `reply.status` is `completed`,
  `failed` or `cancelled`, and `reply.files` lists deliverables published during that
  turn (up to ten).
- `cairn.storage.get/set/delete/list`: up to 1000 values of 64 KB per connection.
- `cairn.http.request{ method, url, headers?, json?|body?|multipart?, auth? }`:
  `multipart` sends a JSON part and deliverables of the reply being handled (by
  artifact ID, 25 MB each); nothing else can be uploaded. HTTPS to declared
  hosts only, no redirects followed, 15-second timeout, 1 MB responses. Requests
  carry `user-agent: Cairn` unless the script sets one. Responses include the
  `content-type`, `retry-after`, `link` and `x-*` headers (rate limits, granted
  scopes, pagination), never cookies.
- `cairn.timers.after(seconds, name, payload)` / `cancel(id)`: 1 s to 7 days, 100 per
  connection.

Refused calls raise a short error code (`forbidden`, `invalid`, `rate_limited`,
`unavailable`…); host error messages never reach scripts. JSON `null` arrives as
`nil`; wrap tables meant as JSON arrays with `cairn.array()`. See
`packages/plugin-types/cairn.luau` for the full typed reference, and
`plugins/discord/main.luau` for a complete package.

### Sockets and inbound messages

A package that keeps its own connection, such as a chat platform's gateway, declares
`socket`. Cairn then starts a transport worker for each enabled instance and sends its
`socket` handler one event at a time:

| `type` | When | Fields |
|---|---|---|
| `start` | the transport worker started | `instance`, `settings` |
| `open` | a requested connection opened | — |
| `message` | a text frame arrived | `text`, or for messages over 1 MB `oversized` (their size) and `text` with only the fields named in `connect` |
| `tick` | a timer set with `every` or `after` fired | `name` |
| `close` | the server closed or the connection failed | `code?`, `reason`, `error?` |

- `cairn.socket.connect{ url, auth?, after?, oversized?, eventTimeout?, heartbeat? }`: WSS to a declared host,
  default port, no credentials in the URL. Reconnections slow down (1 s doubling to
  60 s) until the package reports `connected`; `after` waits longer (seconds, a day at
  most). `auth` adds the instance's credential or a secret to the handshake headers,
  like `http`. Messages up to 16 MB are read; above 1 MB the script receives only the
  JSON fields listed in `oversized` (such as `op`, `s` and `d.id`). Paths may have six
  segments; `*` projects every array entry (`d.threads.*.id`) without retaining its
  other fields. The complete worker frame, including JSON escaping, must fit in 1 MB.
  A smaller wire message can also require projection because of escaping. When even
  the projection cannot fit, `text` is null and the socket remains open.
  `eventTimeout` optionally limits each received message handler to 100–35,000 ms
  from wire receipt, including time waiting in its event queue. Expiry stops the
  worker and closes its socket; the manager may restart it. The plugin must leave
  unfinished inputs unacknowledged and support provider retries/deduplication.
  An already committed effect is not undone by cancellation.
  `heartbeat = { interval, timeout, peerTimeout? }` optionally monitors RFC 6455
  control frames in the I/O supervisor, independently of handler work. All values
  are milliseconds, from 1,000 to 300,000. At `interval` it flushes a Ping with a
  fresh nonce and expects the matching Pong within `timeout` of the actual send.
  Unrelated or late Pongs do not renew it. `peerTimeout`, when supplied, separately
  requires Pings from the peer; application messages do not renew this deadline.
  Expiry closes the connection and emits `close` with `error = heartbeat_timeout`;
  the package owns reconnection policy. Checks and pending nonces belong only to
  the current connection and disappear when it closes. No heartbeat is enabled
  unless requested by the package.
- `cairn.socket.send(text, { auth = true | { secret }, pace?, budget?, name? }?)`: with
  `auth`, the host replaces `{{secret}}` with the JSON-escaped secret. `pace` (ms, a
  minute at most) queues sends outside the handler and spaces actual writes with the
  same credential across every instance. Reads, heartbeats and cancellation keep
  running while queued; at most 16 sends may be pending per worker. Completion
  callbacks run sequentially in the worker, while socket reads, peer Ping/Pong and
  command flushing proceed independently of slow capability calls. Cancellation can
  interrupt a running handler. Each transport has at most 64 queued commands and 128
  queued events. Across the installation, queued/in-flight event payloads have a
  32 MiB encoded-JSON budget; wire buffers, IPC and deserialized-value overhead are
  additional. Queue or budget pressure closes the transport instead of accepting
  unlimited data; unfinished inputs require the provider's retry protocol.
  `budget = { remaining, resetAfter }` reserves a send from a shared balance until reset
  (`resetAfter` in milliseconds, a day at most). Observations can lower the balance;
  they never replenish it before reset. Unsent reservations are released on close.
  Returns `{ queued = true }`, or `{ queued = false, retryAfter = milliseconds }` when
  the budget is exhausted. With `name`, an actual write delivers
  `{ type = "sent", name, sent = true }`; an observation expiring while queued delivers
  `{ type = "sent", name, sent = false, retryAfter = 0 }` so the package can refetch it.
  The same applies when a newer observation invalidates a queued reservation.
  Timers for a response should start on `sent`, rather than when queued.
- `cairn.socket.close(code?)` (1000 or 3000–4999). Timers: `every(name, ms, first?)`
  (100 ms to an hour), `after(name, ms)` (once) and `cancel(name)`, eight at most; they
  belong to the current connection and stop when it closes.
- `cairn.instance.status(state, message?)`: `connecting` or `connected`, with a short
  message the owner sees (for example who the bot is signed in as).
- `cairn.fail(message)` in the transport worker stops the connection until the owner
  reviews the instance and shows the message, for example after an invalid token.

Commands apply in order once the handler returns. Frames, ticks and connection
changes run in the transport worker, separate from the worker handling queued events,
so a slow reply never delays a heartbeat or a three-second acknowledgement. Transport
handlers still run sequentially: avoid synchronous REST hydration in them.

`cairn.inbound.accept{ kind?, startsWork?, id, author: { id, name }, context?, body? }` (capability
`inbound`) hands Cairn something a person wrote, from the transport worker or from a
timer that polls an API. Cairn applies the instance's admission rules before keeping
anything: people who aren't allowed are dropped without a trace, approval mode
records a request without content, IDs deduplicate for 30 days, and at most 50 events
wait per instance. Accepted messages reach the package's `kind` handler (`message` by
default) with `context` and `body` merged under the host's `id` and `author`, and may
start conversations for that person. Returns whether it was queued.

A `message` known to start a new turn may opt into `startsWork = true`. Admission
then returns `busy` before writing an event or deduplication marker when current
submission quotas or the 20-pending-delivery ceiling are full. This hint grants no
rights and reserves no capacity: `conversations.submit` always rechecks the native
limits. Already accepted work waits without spending the finite handler-error
retry budget when those limits become full later. The 50-event admission ceiling
still applies. Omit this hint for messages that may answer questions, stop work or
perform other controls; they must remain callable while submissions are full.
Non-message events cannot set it to true. A previously submitted event can still
resolve its idempotent receipt while its own delivery occupies the last slot.

Separate processes, capability checks and Luau's sandbox are dogfood boundaries,
not yet a marketplace guarantee: a public catalogue still needs signed packages,
OS-level isolation of workers and an update policy. No Codex plugin catalog is
enabled or downloaded.

### Signed public HTTP

The `webhooks` capability declares POST endpoints authenticated in the installation
with HMAC-SHA256 over the original bytes. Each declaration names a secret setting,
signature header, optional digest prefix, and template containing `{{body}}` once.
An optional signed Unix timestamp uses `{{timestamp}}` and a declared header with
`maxAgeSeconds` (1–600). The host checks timestamp age, rejects ambiguous duplicate
signature headers, and compares the digest in constant time before starting Luau.

```json
{
  "webhooks": [{
    "name": "events",
    "signature": {
      "secret": "signingSecret",
      "header": "x-provider-signature",
      "prefix": "v0=",
      "template": "v0:{{timestamp}}:{{body}}",
      "timestamp": { "header": "x-provider-timestamp", "maxAgeSeconds": 300 }
    }
  }]
}
```

Beacon relays the public route
`/api/public/installations/<installation>/plugins/<instance>/<endpoint>` without
storing its body. It forwards provider signature/retry headers and binds the request
to the exact installation route, excluding browser credentials and proxy headers.
Offline installations return 503; the relay deadline is 2.8 seconds.

The direct route is `/api/plugin-webhooks/<instance>/<endpoint>`. The instance
must be enabled and its package approved. This route has no Cairn account authority.
Its fresh `webhook` handler receives `{endpoint, method, headers, body, instance, setup}`;
`body` is the original UTF-8 text. It returns `{status, body, headers?}` where `body`
is text and only `content-type` is accepted in response headers. Ordinary cookies
and authorization headers are never exposed to the script. Limits are 128 KiB of
request body, 16 KiB of headers, 64 KiB of response body and 2.5 seconds for the
whole operation, including body reading and worker startup. A failed or timed-out
handler is an error response, never an empty successful acknowledgement.
Host-to-worker event frames allow 2 MiB of encoded JSON, including the newline,
so settings and an escaped 128 KiB UTF-8 body can arrive together. Package loads,
capability calls/results and handler output retain their 1 MiB frame limit. The
HTTP body, header, response and deadline bounds above remain independent.

`cairn.inbound.deliver(message)` returns `{status}`: `accepted`, `duplicate`,
`denied`, `requested` or `busy`. `accepted` means the event and its deduplication
marker were committed together; `busy` means they were not committed and the
sender can retry. Requests retain only identity and routing. Use this result when
deciding whether to acknowledge a signed HTTP or socket delivery. The shorter
`inbound.accept` returns true only for a newly accepted event. Neither API bypasses
the instance's admission rules or grants public requests conversation control.

### Setup drafts

The `/api/plugins` catalogue advertises `setupDrafts: true`. Independently updated
clients only list drafts when this marker is present; earlier installations still
load and manage their existing instances without calling the new endpoint. A draft
read failure on a supporting host remains visible without hiding the catalogue.

Owner-only `/api/plugins/<plugin>/setup-drafts` supports POST and GET (a list of
`{items, nextCursor: null}`). Append `/<draft>` for GET, PATCH or DELETE. POST
accepts `{name, settings?, secrets?, token?}`; PATCH additionally requires the last
`revision`, as does DELETE. Missing required fields are allowed during setup;
provided fields still use the manifest's validation. Secret values remain encrypted
and responses expose only their names. An optional declared credential is stored
with the same encrypted TTL; responses expose only `credential: boolean`. Blank or
omitted tokens keep the saved credential. PATCH keeps the draft ID and original expiry.

For a root `credential.oauth`, save the application settings and secrets first,
then POST `/<draft>/oauth` with `{revision}`. The returned flow uses the ordinary
owner-bound credential callback, status and cancellation endpoints. PATCH the
same draft with `credentialFlow` to retain the completed consent. A setup flow
cannot create an ordinary connection or authorize another draft. Changing its
saved revision, owner access or package approval invalidates consent in flight.
Pending flows remain in memory for at most ten minutes; restarting requires new
consent. Pasted OAuth access tokens are refused; earlier drafts containing only
a pasted token also require new consent before forms or activation. Saving a different OAuth client,
scope setting or client secret retires the previous credential before reconnecting.
Forms and activation renew expired credentials under the captured draft and
approval, retaining the original secret expiry. Uncertain renewal requires new
consent and cannot be activated or retried with the consumed refresh token.
Activation rechecks the stored consumed flag in its transaction, so a renewal
started by another form after preparation cannot be promoted until it finishes.
Web and Android save the draft before consent, poll or cancel the flow, retire
late responses after closing, and save the completed flow before activation.
Tokens remain on the installation. Public owner MCP exposes the same root draft
operations through `manage_plugin_setup`: list/get/save/delete/form/activate and
signin/signin_status/signin_cancel. Mutations retain the saved revision; completed
consent is consumed through `data.credentialFlow` when saving. The installation
tool exposes that operation with live run/requester checks, but installation
management does not grant a draft's private credentials or owner consent. Those
private actions require the owner app or owner MCP. Neither path grants agents
access during creation or activation. Broader resource, route, grant and personal
OAuth mutation parity remains a delivery requirement.

Draft lists use `?cursor=…` and return `{ items, nextCursor }`, with a bounded
encoded response size. Both clients load all pages before displaying the saved
list, and cancel unfinished reads when leaving the screen.

Drafts expire after 24 hours, with at most 32 live drafts per installation. They
belong to the current owner's account and installation-access revision and never
start workers or grant agents access. Changing the approved package makes the
draft `review_needed`: the owner can inspect or discard it, but cannot edit it or
use its public endpoint. Uninstalling the package deletes its drafts and secrets;
reinstalling identical files does not restore them or an in-flight creation.

A webhook can declare `"setup": {"match": {"/type": "url_verification"}}`.
Between one and eight JSON pointer selectors must all match string values. A
signed JSON request matching that declaration can use the draft's stable direct
or Beacon route before activation. Its handler receives `setup = true`; only
pure Luau computation is available, including `cairn.json`. Every host capability
is refused. Signature verification precedes the live owner check; expired,
deleted, revoked or changed drafts cannot return a challenge. Other events are
refused before invoking the package. Live instances receive `setup = false`.

With `instanceForm`, the owner can POST `/<draft>/form` with `{revision,
settings?, secrets?, token?, changed?, directOrigin?}`. Typed values temporarily
override saved values; a separate PATCH persists them. The form receives the stable
`instance` ID and `setup = {revision, expiresAt, urls}`. `urls.webhooks` maps every
declared endpoint name to its callback URL; `urls.oauthCallback` is the shared OAuth
callback for that draft. They use Beacon by default. An explicit `directOrigin`
uses an HTTPS origin without credentials, a path or query, under the same rule as
OAuth. These addresses do not by themselves authorize any public request.

Form handlers may use only declared HTTP and exec capabilities, with credentials
injected by the host. Saved secret values never reach Luau. Editing, deleting or
expiring the draft during I/O prevents that response reaching the handler. Signed
public setup challenges retain their pure-Luau scope with no host effects.

Draft resources use the separate owner-only path
`/<draft>/resources/<collection>` (GET/POST), with `/<resource>` for GET/PUT/DELETE.
Mutations require `setupRevision`; replacing or deleting a resource also requires
its `revision`. Missing required fields are allowed while preparing, while supplied
fields retain the schema and 64 KiB limits. Parents must belong to the same draft
and declared collection tree. Resources and their encrypted fields expire with the
draft and are removed on discard/uninstall. They never appear through live instance
APIs and provide no agent or delegation authority. Both active and draft
configuration writes recheck their exact loaded package approval and installed files
after the last remote authority lookup, before committing. Resource form previews,
OAuth exchanges and OAuth publication use the same boundary. The final database
transaction also fences approval and configuration revisions; a delayed owner or
membership response cannot make edited or missing package files usable.

Owner OAuth uses `/<draft>/resources/<collection>/<resource>/oauth/<provider>`
(POST consent, GET safe status, DELETE local disconnect) and `/finish` (POST).
Consent and disconnect require `setupRevision`; finish requires the captured
`state`, which pins its root and resource snapshots. Only non-personal providers
are available in setup. Anonymous callbacks still deposit a single code at the
stable callback URL; the same owner must complete the exchange.

Consent expires after at most ten minutes and never later than the draft. Final
authorizations share the draft's encrypted TTL and carry explicit setup provenance.
Changed or deleted roots/resources, disconnection, expiry or approval changes
fence in-flight completion. Consent retains the exact approval record fingerprint;
reapproving identical files or switching away and back cannot revive it. Pending
consent from a version without that proof must be started again. Discarding a resource also deletes its unconsumed
setup consent states. Safe identity metadata updates stay in the draft namespace;
active OAuth endpoints and operational credential lookup refuse setup provenance.

The owner activates a saved draft with POST `/<draft>/activate`: `{revision,
enabled, agentIds?, agentId?, projectId?, accessMode?, userIds?, people?,
maxMessagesPerHour?, maxPerPersonPerHour?, directOrigin?}`. Name, settings,
credentials and secrets come from the persisted draft; update them with PATCH
before activation. The final `form` invocation has `submit = true`, the same stable
`instance` ID and `setup` addresses. It may supply derived required values before
full schema validation. Ordinary instance saves use the same validators.

Activation requires complete root/resource fields and secrets, valid parent trees,
current owner/approval and unchanged root/resources after form I/O. One transaction
publishes the live instance and resources with their original IDs, promotes only
finished owner OAuth by clearing setup provenance and rebinding its instance
revision, removes credential expirations and consumes setup metadata. A final form
cannot change a connected provider's client ID while retaining that authorization:
save the final configuration and complete fresh consent first. Pending
consent is discarded; an already-consumed exchange cannot publish late tokens.
Failure preserves the draft and starts no worker. A successful retry against the
consumed draft returns not found/conflict rather than creating a second instance.
Workers are notified only after commit. No agent is granted by default.

The web and Android owner interfaces use saved setup drafts for packages declaring
resources, OAuth or webhooks. Owners can resume or discard an unfinished setup,
save root settings and masked secrets, prepare resource collections and activate
the saved instance. Activation waits for the latest form answer, saved root
changes and any resource write. An open resource editor must be saved or closed
first. Active resources support editing, parent selection, pagination and confirmed
deletion. Owners can also manage shared resource OAuth and resource agent access
on both clients. Routes and provider-side revocation remain separate unfinished
delivery slices in the Slack checklist.

Root instance settings, including partial instance/setup form values, are limited
to 512 KiB of encoded JSON in aggregate. A larger configuration is refused with
HTTP 413 before it can be persisted or sent to a worker; individual field limits
still apply and no values are truncated. The separate owner configuration request
budget can carry secret inputs without putting them in the worker's event.

### Resource collections

The `resources` capability declares related collections in `resources`:
`{name, title, singular, parent?, delegation?, form?, settings}`. Each collection uses the
same field schema as instance forms; parent declarations must form an acyclic tree.
Resources belong to exactly one instance. Parent references cannot cross instances
or name an unrelated collection. Each instance can keep at most 256 resources;
values and encrypted secrets are bounded to 64 KiB each.

Owner management uses
`/api/plugins/<plugin>/connections/<instance>/resources/<collection>`: POST creates,
GET lists `{items, nextCursor}` in pages of at most 100 records, also bounded to
512 KiB of encoded JSON so the same page fits the Luau capability frame. Follow
every non-null cursor even when a page has fewer than 100 records. Append
`/<resource>` for GET, PUT
and DELETE. PUT includes the last `revision`; concurrent/stale edits return 409.
Secret fields are submitted separately; blank keeps the existing secret. Responses
contain `secretKeys` (names), never values. Deleting a parent deletes its descendants
and encrypted secrets atomically; removing the instance also removes its resources.

`cairn.resources.list{collection, cursor?}` and `get{collection, resource}` read
metadata. HTTP can select a secret using
`auth = {resource = id, secret = key, header = "authorization", prefix = "Bearer "}`.
Cairn verifies the resource and every parent before use and after I/O; paused,
removed or changed resources cannot deliver a result. The package never receives
the credential. Resource access by agents requires a separate resource grant.

#### Resource configuration forms

A collection declaring `"form": true` uses the package's `form` handler on web
and Android. POST `.../resources/<collection>/form` with `{settings, secrets?,
parentId?, resource?, revision?, setupRevision?, changed?}` to preview it. Editing
a saved resource supplies its ID as `resource` and its exact `revision`; inert
setup targets also require `setupRevision`. A missing parent can be reported as
an error before any parent credential is requested.

The event contains `instance`, `app` (the root's nonsecret summary), `resource`
(`collection`, saved `id` or null, saved nonsecret `settings` or null, the selected
`parent` or null, and secret names), `values`, `changed` and `submit`. Saved settings
come from the exact revision retained by the host, independently of submitted
`values`; forms can use them to prevent replacing an established external
identity. They never include secret values. Parent metadata contains nonsecret
settings and secret names, never values. The result uses the collection's own fields:
`{options, values, errors, view}`. Errors may name its fields, `parentId` or `form`;
derived values cannot set app fields or secrets. Guidance views display content
and links; action buttons/forms have no configuration action endpoint and remain
disabled. Instance panels continue to provide package actions.

Only HTTP and declared programs are available during configuration. Unqualified
HTTP authentication and program secret injection still use the root app's
credential/secrets. Resource-form HTTP can explicitly select:

```lua
local response = cairn.http.request({
    url = "https://api.example.test/channels",
    auth = { configuration = "parent", oauth = "bot" },
})
-- Or: { configuration = "self", secret = "token", prefix = "Bearer " }
```

`configuration` accepts only `self` or `parent`, bound by the host to this editor.
A new self may use its temporary typed secrets; OAuth needs a saved self or the
selected parent. Personal OAuth and arbitrary resource IDs are unavailable here.
Root, saved-resource and selected-parent/ancestor revisions, exact approval,
owner access and setup expiry are rechecked around I/O and before publication.
Owner configuration can inspect paused or inert resources without making them
operational; ordinary resource references still require every ancestor enabled.
Owner OAuth uses the same serialized, single-use refresh path and retains setup
expiry/provenance. An uncertain refresh requires reconnection.

For a saved root app, resource forms also renew its root OAuth credential through
the shared per-instance lock before invoking Luau. This works while the instance
is paused. Root forms and save-time validation capture the configuration owner's
exact access grant; a slow renewal rechecks that grant and the approved package
before storing rotated credentials, alongside the existing instance revision
fence. Removal, replacement or revoked authority cannot publish the late result.
The approval is the one which loaded that form, including while waiting for the
refresh lock or an owner lookup. Renewal also retains the originally loaded root
revision. After the final remote authority lookup, a native read rechecks that
root and the exact consumed credential immediately before the provider POST.
Before sending, the host removes the stored
refresh token and records `oauth.refreshing = true` in the encrypted credential.
Only a confirmed response under the same authority replaces this consumed state.
Uncertainty requires new consent, including after reopening the store; it cannot
replay a potentially rotated refresh token. A successful new sign-in replaces the
consumed state through ordinary revision-checked saving.
Tokens remain in the host. The same renewal fences apply to inert setup roots,
including both intent and completion retaining the original setup expiry. Native
root consent, activation and their client/tool authority boundaries are described
above; broader management mutation parity remains a draft-PR delivery requirement.
The [local encrypted-record observation](performance/ROOT-FORM-CREDENTIAL-RENEWAL.md)
records the reused storage shape and its limits.

Both clients discard obsolete responses when the editor or parent changes, keep
typed secrets only in memory, show errors and wait for the current form before
enabling Save. Saving reruns the package with `submit = true`, validates derived
nonsecret fields and commits only under the original configuration snapshot and
resource revision. Form handlers should perform repeatable discovery/validation:
they may run several times, including again at Save. Dynamic options remain
bounded to 200 per field; a partial provider directory is not a complete catalogue.

### Resource grants and operations

`operations` declares `{name, collection, profile, scopes?, requests?}`. Profiles
are `read`, `contribute` and `manage`, in increasing order. Each request fixes
`{method, host, path, bindings?}`; a trailing `*` permits a path prefix. A binding
maps a request field to an admitted input scope (for example `channel` to
`channel`) or to a non-secret resource setting (`team_id` to `resource.externalId`).
The host verifies it in query/JSON/form arguments, rejects duplicates and
ambiguous raw/multipart/file-backed bodies, and never relies on the script to
enforce that request's method, endpoint or bound fields. File encoding cannot
overwrite a checked JSON field in a bound resource operation.

A command or tool declares `operation` and must require a `resource` plus every
declared scope as text arguments. The host selects the resource and operation
before running Luau; the event includes `operation`. Resource-scoped handlers may
read their selected resource and make only that operation's approved HTTP requests
with its credential. They cannot switch instances, use root-instance credentials,
run a program or bypass admission through another capability.

The owner manages rules at
`/api/plugins/<plugin>/connections/<instance>/resources/<collection>/<resource>/grants`:
POST creates and GET lists; DELETE of `/<grant>` revokes. A rule has `agentId`, an
optional `profile`, extra `operations`, `deniedOperations`, and `scopes` mapping
input names to allowed values. Exclusions override the profile within a rule;
multiple rules grant the union of what they allow. Scoped rules do not authorize
operations that omit those scopes. Nothing is granted by default, including to
Main agent or through instance-level grants. Tools list only operations the
agent can use. CLI and MCP share the same checks.

GET uses `{items, nextCursor}` and accepts `?cursor=<last-id>`, like resource
collections. Route and grant pages contain at most 32 records and are also bounded
by their encoded byte size below the relay limit. Web and Android load every page
before enabling edits; closing the editor cancels the entire read.

On web and Android, open the active instance, then **Agent access** on its resource.
Choose an agent explicitly, select a profile or individual operations, optionally
limit scope values, and inspect the per-rule preview before granting access.
Resource grants are unavailable during inactive setup. Rules can be revoked with
confirmation; this does not remove permissions granted by other rules. After an
uncertain write, refresh the saved rules before another change. Read-only loading
can be canceled by closing the editor; reopening always fetches a fresh list. An
outstanding grant or revocation stays locked until its response arrives. Connecting
a resource provider through OAuth does not grant any agent access by itself.

Rules are bound to the verified current owner's access, and rechecked alongside
instance, resource/parent revisions and approval before capabilities and before
returning results. Deleting resources removes their rules. Agents reachable by
anyone cannot receive resource credentials; adding such a delegation also checks
existing resource rules.

### Owner-selected conversation routes

A resource collection can declare `delegation: true` when the package also declares
`conversations.submit`. These instances do not delegate to one app-wide agent.
The owner configures routes under each resource at
`/api/plugins/<plugin>/connections/<instance>/resources/<collection>/<resource>/routes`.
POST creates, GET lists `{items, nextCursor}`, and `/<route>` supports PUT or DELETE with its
last opaque `revision`. A route contains `name`, `enabled`, exact string `match`
conditions on source context, `agentId`, optional `projectId`, `accessMode`,
`userIds`, optional `people`, and hourly limits. Defaults are 20 messages per hour
and five per person; the latter applies to everyone access. No route or person is
granted by default. The selected agent must have only the selected project's access
and no installation access; everyone access retains the stricter public-agent rules.
An owner can preview eligible agents at the sibling endpoint
`/route-eligibility?accessMode=allowlist|requests|everyone&projectId=<optional-id>`.
It returns `{items: [{agentId, eligible, reason?}]}` without access policies or
credentials. The result is advisory: save and effects recheck current authority.
Configuration remains available while the instance or resource is paused.

Listed routes include an owner-only `summary` with `hourStartedAt` (UTC epoch
milliseconds), `messagesThisHour` and `pendingRequests`. These values come from
the same database snapshot as the route page. Usage reads the route's admission
counter, independently of its app's counter; requests count only currently pending
people on that exact route. They are presentation data: they are not persisted in
the route and do not change its editing revision. Both clients display the measured
hour and refresh the counts after management actions. Missing summaries on older
hosts are shown as unavailable rather than zero; failed reads do not keep showing
the preceding counts as a successful refresh. Pagination includes summary bytes
in its existing response budget.

`cairn.inbound.deliver` supplies the configured `resource` and authentic source
`context` alongside its message. The host selects the most specific matching route.
A disabled override denies admission; equally specific matching rules conflict.
Packages cannot select an agent or route using message bodies. Access requests and
approvals are local to the selected route: append `/<route>/requests` for GET or
`/<route>/people/<person>` for POST
`{action = "approve" | "deny" | "remove", revision = <last-revision>}`.
Each successful mutation returns the current route and its editing revision;
DELETE returns `{deleted: true}`. A stale edit, people action or deletion returns
409 and must be refreshed explicitly, never retried with overwritten fields.
Removing one person revokes their pending work while preserving other admitted
participants' authority. Reapproving them grants a new native generation: old
queued messages, controls, OAuth requests and in-flight results remain revoked.
Anyone access does not use a people list; choose a restricted access mode before
managing individual participants. The editing revision is separate from native
authority; clients must treat it as opaque.
Unapproved requests retain the person's identity, never message content.

Native execution retains the original routed requester while waiting for capacity
and across account, workspace and runner preparation. The scheduler rechecks it
before admission and the worker checks it again before starting the agent. A
withdrawn source cancels queued work. Temporary authority lookup failures defer
admission or requeue an execution still in preparation, releasing its account and
worker reservation before retrying through ordinary admission. Reapproval cannot
replace the proof already captured by a preparing run.

The per-person generations are host-only. They are never accepted from a plugin
or an owner form, and only currently listed grants need entries (at most 100).
Personal bindings produced by earlier development snapshots without this proof
fail closed and require a new request from the source. This does not erase Cairn
history or credentials. Ordinary instance delegation keeps its existing format.

Accepted events and deliveries retain native route authority outside the Luau
payload. Instance, ancestor-resource, route, owner-access and package revisions
are checked again before effects and after slow results. Adding a more specific
route can revoke older queued authority. Each worker is checked against the exact
manifest and source files it loaded, including across package approval changes.
Native execution also retains its precise package approval across remote owner
checks and preparation, and verifies it with the local route, person and run
before registration or starting the agent. Reapproving identical files does not
release work whose retained approval changed during that verification.
Commands and MCP tools, including `manage_plugin` calls from an installation
tool, retain their run's original requester, including when the target integration
uses an instance-wide agent grant. That separate target grant
does not bypass revocation of the source participant, route or owner access.
Operation scopes with the same name as a route condition must match that condition;
operations on the source integration cannot select a different instance or an
unrelated resource even if the agent has a broader grant.
Commands and tools on that source integration must declare a resource operation;
an instance-wide handler cannot enforce the admitted resource and channel scope.
Explicit grants to other integrations remain independent, with the original
requester's authority still checked around their effects.

Shared threads still share a conversation, while each admitted message records its
own external subject. Answer and cancel calls must include `delivery`, the issuing
turn's `deliveryId`, and come from that turn's author under the same route authority.
Old buttons, another participant and another route cannot control the current turn.
Question notifications are limited to their issuing turn. Reusing an agent's
question ID in a later turn produces a new notification for that turn's author;
old controls cannot answer it. Resuming an interrupted
turn also requires its original authority; another author must start a new turn.
For a resource-routed message, `cairn.conversations.submit{newTurn = true, ...}`
queues an independent request in the same conversation. If the preceding turn
stops, its original prompt is cancelled rather than replayed. Cairn waits until
its workspace has settled, then starts a fresh native session with the queued
requester's own authority. History and workspace remain shared. The old turn's
MCP bearer and controls cannot act for the next requester. Each delivery keeps
its own terminal outcome even though the native run is reused: queued requests
are not cancelled with their predecessor, and a late reply pump cannot collect a
later requester's answer or files for an abandoned request.

A source author's Stop allows an already queued independent request to proceed;
the owner's Pause in Cairn keeps the queue paused even when new requests arrive.
An explicit native resume releases that pause and advances the next independent
request. Answers still queued for the stopped turn are cancelled with that turn;
ordinary requests retain their queue order. Admission, route, current thread
binding and requester are rechecked when the new turn actually starts. A waiting
workspace does not block other conversations. Temporary authority lookup failures
leave the request queued for a later tick without pausing the conversation. A
revoked request is cancelled individually, allowing the next authorized
participant's independent request to advance. These checks also apply after a
successful preceding turn; its final answer remains available to its own delivery.
A scheduled continuation still takes priority over queued independent requests.
Editing or withdrawing the queued message from Cairn removes its automatic new-turn intent. Omitting `newTurn`
preserves the original-author resume path.

An admitted resource participant can call
`cairn.conversations.reset{conversation = expectedChatId, thread = sourceThread, shared = true}`
with the `conversations.control` capability. The expected conversation must still
be the current binding of that source thread; stale buttons and other threads
cannot reset its replacement. Queued messages, a preparing/running/stopping worker,
unsettled native recovery, scheduled continuations (including paused waits) or
undelivered replies prevent reset. Finish or stop
that work first and wait for its reply. Reset retires only the binding: existing
Cairn history and workspace are retained. It creates no empty conversation; the
next source submit starts a fresh one. A retry of the same admitted event returns
its committed receipt, including after that new conversation exists. Receipts
expire after 30 days and conversation purge removes them; a purged expected chat
can never identify the replacement. Old source deliveries and controls cannot act
for a retired binding. Generic setup journeys and the Slack controls that use
this API remain in the delivery checklist.

### Host-managed resource OAuth

The `oauth` capability declares up to eight providers for resource collections.
The host captures token responses; scripts receive neither authorization codes,
client secrets, access tokens nor refresh tokens. Providers connect either an
owner-managed resource or the verified requester's personal account.
Setup drafts and the client configuration interfaces remain in the Slack delivery
checklist.

```json
{
  "name": "bot",
  "title": "Connect workspace",
  "collection": "workspace",
  "authorizationUrl": "https://api.example.com/authorize",
  "tokenUrl": "https://api.example.com/token",
  "clientId": "clientId",
  "clientSecret": "clientSecret",
  "scopes": ["messages:read", "messages:write"],
  "token": {
    "accessToken": "/access_token",
    "refreshToken": "/refresh_token",
    "expiresIn": "/expires_in",
    "scope": "/scope"
  },
  "metadata": { "externalId": "/team/id" }
}
```

Both endpoints use HTTPS on declared hosts, without redirects. `clientId` names
an instance text setting; `clientSecret` names a secret setting. `pkce: true` adds
S256 PKCE and permits a public client without `clientSecret`. Confidential clients
use HTTP Basic authentication at the token endpoint, after independently
form-encoding the client ID and secret as required by
[RFC 6749 §2.3.1](https://www.rfc-editor.org/rfc/rfc6749#section-2.3.1).
A provider supporting both may declare both. Slack's PKCE switch changes the app to a public client; do not
enable it silently in a server app.

`scopeParameter` defaults to `scope` and may name another nonreserved parameter,
such as `user_scope`. `scopeSeparator`
defaults to a space and can be a comma. Response mappings are bounded JSON
pointers; `refreshTokenResponse` may declare a different shape for renewals, such
as a nested initial token and a root-level refresh response. Optional expiry is in
seconds. Metadata captures only identity strings ending in `id` or `name`, into
declared nonsecret text fields, without overlapping token paths. Existing identity
values cannot be silently replaced by a different OAuth workspace.

Authorization management uses this prefix:
`/api/plugins/<plugin>/connections/<instance>/resources/<collection>/<resource>/oauth/<provider>`.

- POST `{scopes: [...]}` starts authorization with a distinct subset of declared
  scopes. Returns `url`, `state`, `redirectUri` and `expiresAt`. State expires after
  ten minutes and binds requester, owner access, package approval and resource and
  instance revisions. Optional `directOrigin` selects the owner's exposed HTTPS
  origin. A provider may declare `redirectOriginSetting`, naming an instance URL
  setting, to use that owner-configured HTTPS origin for every authorization.
  Origins cannot contain a path, query or credentials. With neither option, the
  redirect uses Beacon automatically.
- The public GET callback only captures one code, encrypted, and returns constant
  plain text. It grants no account authority and cannot exchange credentials.
  Direct: `/api/plugin-oauth/<instance>/callback`; Beacon:
  `/api/public/installations/<installation>/plugin-oauth/<instance>/callback`.
- POST `/finish` appended to that prefix, with
  `{state}`, must come from the initiating account. It returns `pending`, `denied`
  or `authorized`. The code is consumed before the exchange, including failures;
  start again after an uncertain result. GET of the prefix returns safe status,
  granted `scopes`, `requestedScopes` and `expiresAt`. Provider grants can be
  cumulative; extra token scopes do not grant any Cairn agent permissions.
- DELETE of the prefix disconnects locally, invalidates in-flight exchanges and
  prevents late responses from restoring credentials. This does not revoke the
  token at its provider. Resource and instance deletion also erase credentials.

Saved HTTP calls use `auth = {resource = id, oauth = "bot"}`. Cairn injects a bearer
token and renews it near expiry. Concurrent calls share a renewal; the consumed
refresh state is persisted before sending a single-use token. An interrupted or
uncertain renewal requires reconnection, rather than replaying the old token.
Owner access, approval, resource identity and authorization revision are checked
again around I/O. Resource-granted calls also recheck the agent before issuing a
new provider request after a slow renewal. Preparing a client secret or checking
remote authority does not preserve an earlier consent: the host checks the exact
authorization again immediately before sending a code or refresh token.

Shared resource authorization controls load their status only when the owner
chooses **Manage authorization** (Android: **Gérer l’autorisation**) for that
resource and provider. Listing many resources does not fetch every provider's
status. A provider the owner opened is re-read after a resource revision changes;
its consent state is still recreated under the current revision. Unopened
providers remain unknown. Pending consent on unchanged resources stays visible
when a resource page is refreshed.

#### Personal providers

An external OAuth start also records a host-owned receipt bound to its native
event and resource/provider/person. Retrying the current consent returns the same
URL. After a newer request replaces it, the older event is refused without
creating another consent or invalidating the newer URL. Receipt creation shares
the pending-consent transaction; a removed/stale event cannot create one. Receipts
are removed atomically with successful, exhausted or obsolete native events and
on instance deletion; pending consent may outlive successful URL delivery until
its own expiry. One event cannot change its requested scopes on replay.

Native sleep and condition wakeups continue the captured requester's authority.
Owner intervention suspends personal OAuth for that task, including later wakeups;
it retains the original external resource and channel restrictions for bot tools.
Held tool calls are fenced when that suspension changes.

Add `"personal": {"subject": "/authed_user/id"}` to a provider whose token
response identifies a person. The host captures this nonsecret subject alongside
the encrypted credentials. The provider must also declare resource identity
metadata, and that identity must match an already configured resource. Personal
providers must capture at least one stable ID, using a pointer ending in `/id`;
a display name alone is insufficient. Personal authorization cannot create or
change the owner's workspace identity.

The same management prefix then addresses only the authenticated Cairn account's
authorization. Members can start, finish, inspect and disconnect their own
authorization; owner-connected providers still require the current owner. Each
authorization is bound to the account's current installation access, independently
of the owner. Removing and reinviting a member requires a new authorization.
Responses include the verified `subject`, never tokens. Disconnect before linking
a different external person. Disconnecting one person leaves others connected and
fences code exchanges already in flight.

Atelier → **Your plugin accounts** (Android: **Vos comptes de plugins**) is available
to owners and members. It lists personal providers on configured live resources,
lets the person choose explicit scopes, open the provider's HTTPS consent page,
check its completion and confirm disconnection. Signing in grants no agent access.
The pending consent URL and state stay in the current page's memory; after leaving
it, start a new sign-in. A pending callback can be checked again, but an uncertain
exchange failure ends that local flow rather than retrying a possibly consumed code.
Disconnection removes Cairn's authorization and pending sign-ins; provider-side
revocation still requires the service's settings.

`GET /api/plugins/personal-authorizations?cursor=…` is a member-readable directory
of labels, provider scopes and only that account's authorization summary. It never
returns settings, secrets, another person's status or access to the owner catalog.
It requires current installation membership, a currently approved personal provider
and a configured stable resource ID. Pages contain at most 100 entries and an opaque
`nextCursor`. Paused/unavailable resources are listed only when the current person
already has an authorization, so they can disconnect it. An unapproved package
contributes no entries until the owner restores approval. Membership and approval
are checked again after loading package metadata; resource and own-authorization
reads share a single SQLite snapshot.

Resource-granted tools and commands select personal credentials from the current
chat turn's authenticated author. The host records this authority atomically with
the message; an input's claimed account, the conversation's first author and the
installation owner cannot supply a fallback. Tool calls recheck the author, turn,
membership, agent grant and authorization around network operations. Retrying
another person's message ID cannot transfer its authority. Steering another
person's current or resumable turn is refused; queue a new turn to use your own
permissions. The native execution worker rechecks queued steering before delivery,
including inputs accepted before a stop or restart.
Editing a queued message reattributes it to the authenticated editor.
An already authenticated native chat request can still send or edit ordinary
messages when Beacon cannot supply live personal-authority proof. Such an input
gets no personal authority for its turn, including after Beacon returns. Explicit
identity or membership refusals still block the request. Owner question control
uses the authenticated request role and suspends personal authority when needed;
it supplies no replacement credential or fallback to the owner’s OAuth.
Question answers follow the same check. The current installation owner can still
answer another person's question from Cairn; doing so suspends personal credentials
for that turn, including calls already in flight. The original author and their
authorization remain unchanged. The original author can answer subsequent
questions and continue steering; personal OAuth stays suspended. The native worker
accepts only an exact owner-answer provenance record for cross-author control,
never an arbitrary anonymous input. A turn admitted without personal proof can
still be steered by that same authenticated account after Beacon recovers, without
upgrading the turn's proof. A new turn can use its own requester again.
Retrying a previously accepted answer returns its receipt without changing the
answer's original provenance or suspending a later turn. Non-owner retries still
require the answer's recorded requester; current-owner control cannot transfer
that earlier answer to another turn. A repeated question ID in a new turn still
requires a fresh answer message ID.

Finishing personal OAuth also links its proved external subject to that account
for this resource and provider. An active external identity cannot be linked to
two installation accounts. Removed memberships can be replaced only by a fresh
OAuth proof; their old callbacks and credentials are invalidated. Disconnecting
removes the link as well as the credential. An externally admitted routed turn
can use only the authorization for its exact proved subject in its resource scope.
The link, membership, route and current turn are rechecked around network I/O.

An admitted external person can also authorize directly from the external app,
without a Cairn account. In a routed `message` or `interaction` handler, use:

```lua
local consent = cairn.oauth.start({
    resource = workspace.id,
    provider = "personal",
    scopes = {"messages:read"},
})
-- Send consent.url privately to this person. After their browser authorization,
-- an admitted interaction from the same person completes it:
local status = cairn.oauth.finish({resource = workspace.id, provider = "personal"})
```

`start`, `finish`, `status` and `disconnect` accept only `resource`, `provider`
and, for `start`, `scopes`. The host supplies the person, native event and source
route. Public ingress, timers, tools, owner panels and scripts claiming an actor
cannot initiate personal consent. The resource must belong to the admitted
resource family. The callback still only deposits a code: an admitted event
from the same person and route must finish the exchange. The provider must prove
that exact person and the configured resource ID. This proof creates no Cairn
account, membership or owner authority.

`start` returns `url`, `redirectUri` and `expiresAt`. Replaying the same durable
event preserves its consent URL. If that exact event has already completed consent,
it returns the safe authorization status instead. `finish` returns `pending`,
`denied`, `authorized` or the current status if already finished. `status` returns
only this person's safe status, subject, scopes and expiry. A missing one-use
exchange after an interruption requires a new consent; no code is replayed.
`disconnect` fences in-flight exchanges and removes both an independent external
credential and any Cairn-account credential linked to that proved person. An
invalid independent credential cannot fall back silently to the linked account.

The installation permits at most 128 live pending consents and 2,048 authorization
storage records (credentials and revocation generations combined). Capacity
errors preserve existing authorizations. Pending-only and disconnected revocation
generations expire after ten minutes; active credentials remain until disconnected
or their resource is deleted. Starting another consent keeps an active credential's
revocation state live. Local disconnect remains available at capacity, including
when the person is already disconnected, and still invalidates pending exchanges.

Scheduled automation authorities and configuration drafts remain in the Slack delivery checklist.
Unattributed in-process submissions and owner panels cannot use personal providers.

Protocol references checked on 2026-10-17: [Slack OAuth](https://docs.slack.dev/authentication/installing-with-oauth/),
[PKCE](https://docs.slack.dev/authentication/using-pkce/) and
[token rotation](https://docs.slack.dev/authentication/using-token-rotation/).

## Contributions

A package ships a whole integration without changing Cairn. The manifest says
what it adds; Cairn renders it on the web and Android, authorizes every call and
runs the script's handler in a one-shot worker (same deadline, memory limit and
capability gate as long-lived workers).

| Contribution | Manifest | Handler | Receives |
|---|---|---|---|
| Command | `commands: [{ name, description, output, arguments }]` | `command` | `{ command, arguments, agent, instances }` |
| Agent tool | `tools: [{ name, description, inputSchema }]` | `tool` | `{ tool, input, agent, instances }` |
| Prompt instructions | `instructions` | — | — |
| Instance panel | `instanceView: true` | `view`, `action` | `{ instance }`, `{ action, payload, values, instance }` |
| Instance test | `instanceTest: true` | `test` | `{ instance }` |
| Instance form | `instanceForm: true` | `form` | `{ values, credential, secrets, instance, changed, submit }` |

- **Commands** run as `cairn plugin <plugin> <command> …` in conversations.
  Arguments are flags (`{ name, flag: "--vault" }`) or positional values
  (`{ name, positional: true }`), optionally `required`, with a `pattern` the whole
  value must match and a `maxLength`. Cairn parses them; unknown, repeated or
  malformed values never reach the script. `output` is `json` (default), `text`, or
  `secret`: a secret is written to stdout exactly, for a pipe, and is never stored.
- **Tools** are listed by the run's Cairn MCP server; input is checked against
  `inputSchema` first. A tool name is the plugin ID or starts with `<id>_`, and never
  replaces one of Cairn's own tools. Tools can't return secret output.
- **Instructions** are added to the prompt of isolated runs whose agent is granted.
- Commands, tools and instructions exist only for agents the owner granted on an
  enabled instance (`agentAccess`). Nothing is granted by default, including Main
  agent. Each capability call rereads the instance and the grant; after a program or
  an HTTP request, a revoked grant or a changed instance discards the result.
  `instances` lists what the agent may use: `{ id, name, enabled, settings, secrets }`
  (secret field names only). Name one with `instance = id` in capability calls when
  there are several.
- **Views** are a list of components: `section` (`title`, `description`,
  `children`), `text`, `alert` (`tone`, `title`, `text`), `badge`, `facts`
  (`items: [{ label, value }]`), `list` (`items: [{ title, subtitle, badge, actions }]`,
  `empty`), `progress` (`value`, `max`, `label`), `code`, `link` (HTTPS `url`),
  `button` (`label`, `action`, `payload`, `style` primary/default/danger, `confirm`),
  `actions` (a row of buttons) and `form` (`fields` like `settings`, `values`,
  `submit: { label, action }`). Tones are `neutral`, `info`, `success`, `warning`
  and `danger`. Anything else, or more than 300 components or 64 KB, is refused. An
  `action` returns `{ notice?, tone?, view? }`; a new `view` replaces the panel.
- **Instance forms** let the package complete the owner's form while they type. The
  `form` handler runs when the editor opens, when the credential changes and when a
  field marked `refresh` changes, with the values so far, whether a credential is
  present, the names of the secrets present and the name of the field that changed.
  `http` and `exec` use the draft's own credential and secrets, before anything is
  saved; nothing else is available. It returns `{ options?, values?, errors?, view? }`:
  choices for `dynamic` fields (`{ value, label, description?, disabled? }`, 200 at
  most), derived values for declared settings (hidden ones included, never secrets),
  errors by setting, `credential` or `form`, and a view shown above the fields. On
  save it runs once more with `submit = true`: its values are applied and checked
  again, and any error refuses the save with its message. Dynamic choices are checked
  as text (`pattern`, `maxLength`) by Cairn and by the package itself.
- **Remote choices** add `remote: true` to a dynamic `select` or `multiselect` in
  an instance or resource configuration form. The manifest must enable that form
  (`instanceForm` or the collection's `form`). These fields are not available in
  a panel's action form. The same preview endpoint accepts
  `lookup: {field, query, cursor?}` instead of `changed`. Queries have at most 200
  characters; opaque cursors are nonblank, control-free and at most 4,096 bytes.
  The handler receives `event.lookup` with `submit = false`, under the same
  owner, package, root/parent, revision and OAuth fences as full form previews.
  It may return only that field's `options`, `pages` and errors (plus global form,
  credential or parent errors appropriate to its scope). Returning `values`,
  `view`, another field's options/errors or more than 200 choices is refused.
  Duplicate option IDs are refused; the complete result remains bounded to 256 KiB.
  Return `pages = {channel = {nextCursor = "opaque"}}` when another page exists,
  including after an empty or short page. Use `nextCursor = cairn.json.null` at the
  end. Omitted page metadata means **completeness unknown**, never “all choices”.
  Cairn adds a `lookup` echo after validation; clients reject an absent/mismatched
  echo, including an older host's ordinary full-form answer. Both clients show one
  bounded page, search, continuation/retry and selected names separately. IDs stay
  selected when absent from the page. Only selected names (at most 100) are kept
  across queries; scope/credential changes discard them. Late responses after an
  edit, scope change or editor dismissal are ignored. Selecting a remote choice
  refreshes the full form; searching does not. Final save always invokes the full
  handler with `submit = true`, even if a caller sends lookup metadata.
- A script may call `cairn.fail(message)` to stop with a message for the person
  (300 characters at most). Any other error stays in the worker.

More capabilities:

- `cairn.exec.run{ program, args?, env?, stdin?, timeout?, instance? }` (capability
  `exec`) runs a declared program without a shell, with an empty environment and a
  private temporary home, 64 arguments, 256 KB of input, 1 MB of output and 30 seconds
  at most. `env` values are text or `{ secret = "<setting key>" }`: the host injects the
  instance's secret, which the script never sees. `LD_*`, `DYLD_*` and `HOME` can't be
  set. Returns `{ status, stdout, stderr }`. Programs run outside the Luau sandbox with
  whatever files and network they can reach: the review screen names each path or
  installer.
- `cairn.http.request{ …, auth = { secret = "<setting key>", header?, prefix? } }`
  adds a secret field to one header of a declared host.
- `cairn.json.decode(text)` and `cairn.json.encode(value)` convert JSON. Incoming
  JSON null becomes Lua nil. Use the explicit `cairn.json.null` sentinel to include
  a present null member in an outgoing table; a Lua nil would remove the member.

Instance routes add `GET …/connections/{id}/view`, `POST …/connections/{id}/actions`,
the generic `POST …/connections/{id}/test` and `POST /api/plugins/{id}/form`
(`{ instance?, settings, secrets?, token?, changed? }`). Secret fields are saved with
`secrets: { key: value }`; an empty or missing value keeps the saved one and the API
only returns their names (`secretKeys`). Grants are saved on agents
(`plugins`); each instance keeps a synchronized `agentIds` copy, and a save
that omits `agentIds` keeps it. Instances of
packages that need no worker (no socket, inbound messages, conversations or timers)
start nothing and show as ready while enabled.

### Setup assistant

The owner adds and edits instances through an assistant, one step at a time. A
package describes it with `[setup]` ([ADR 0057](adr/0057-guided-plugin-setup.md)):

```toml
[setup]
effort = "app" # signin | token | app, shown in the catalogue
summary = "Gmail connects through a Google Cloud app you own. Create it once…"
nameFrom = "email" # a string setting naming a new instance once known

[[setup.steps]]
id = "app"
title = "Google Cloud app"
settings = [
  "clientId",
  "clientSecret"
]
shared = true # a new instance can reuse it from a saved one
warning = "External apps in Testing lose access after seven days…"

[[setup.steps.checklist]]
title = "Create an OAuth client"
text = "Application type: Web application…"
url = "https://console.cloud.google.com/auth/clients/create"
link = "Open Clients"
copy = "callbackUrl" # shows the OAuth callback URL with a copy button

[[setup.steps]]
id = "permissions"
title = "Permissions"
settings = [
  "allowSend",
  "allowDrafts"
]

[[setup.steps.presets]]
id = "send"
title = "Read, draft and send"
risk = "medium"
values = { allowDrafts = true, allowSend = true } # the step's other switches turn off

[[setup.steps]]
id = "connect"
kind = "credential" # settings (default) | credential | people | agents
title = "Sign in with Google"

[[setup.steps]]
id = "agents"
kind = "agents"
title = "Agents"
```

Declared steps cover the instance: a `credential` step when the manifest has a
credential, every visible setting exactly once, a `people` step for delegating
packages and an `agents` step, last, with `agentAccess`. Only settings steps list
settings, may be `shared` and may have presets, whose values name the step's
boolean settings. Checklist links are HTTPS; `copy = "callbackUrl"` needs OAuth. At
most 8 steps, 8 tasks and 6 presets per step. Boolean settings may declare
`risk = "low" | "medium" | "high"`, shown beside the switch.
Presets refresh each changed field marked `refresh`, just like its individual
switch. Form errors apply only to the checked input; changing an ordinary field
clears its stale error without requiring `refresh`. Save still runs the package's
authoritative `submit = true` validation.

Without steps, Cairn derives them: one `Settings` step with the visible settings,
before the sign-in for OAuth and after the credential otherwise, then `Who can
talk to it` and `Agents` when they apply. The effort defaults to `app` for OAuth,
`signin` for a device sign-in and `token` for another credential or a required
secret setting. `GET /api/plugins` returns the resolved `setup` beside each
`manifest` (and each version), and `manage_plugin`'s `setup` action returns it too.
Clients of an older installation resolve it from the manifest the same way; the
shared corpus `tests/fixtures/plugin-setup.json` checks Rust, web and Android.

The package's `form` view appears on the credential step, on steps with dynamic or
`refresh` fields, and on the first step when there is no credential. A new instance
is named after `nameFrom` once the form derives it, unless the owner typed a name.

`sharedFrom: <instance id>` on `POST /api/plugins/{id}/form`, `POST
/api/plugins/{id}/oauth` and the save reuses the settings of the shared steps from
a saved instance of the same package. The host copies them, secrets included;
clients never receive the secrets. Each instance keeps its own copy, and only a
new instance can reuse: an edit naming `sharedFrom` is refused. An installation
management agent may only reuse an instance it is granted.
Required secrets must be present; an absent optional secret remains absent,
replacing any typed draft value for that shared field. Sources with secret fields
remain individually selectable: equal visible labels cannot prove equal secrets.

Installed manifests with `[setup]` require epoch 12 / SQLite version 11, which
fences previous readers before plugin loading. The unqualified Slack branch
reserves epoch 13 / SQLite 12 above this Main transition; see its
[compatibility inventory](SLACK-PLUGIN-COMPATIBILITY.md). Existing package files, approvals,
connections and vault entries are preserved. See the
[forward migration and recovery plan](AUTOMATIC-MIGRATIONS.md#current-transition-epochs-311-to-12).

The assistant's last step can grant agents to a new instance with the save's
`agentIds`: creation only grants, never removes. A saved instance shows its agents
read-only, and grants change on agent pages ([ADR 0051](adr/0051-agent-centric-permissions.md)).

## Integrations

Packages can also take part in Cairn's own features. The host keeps every credential
and does the work; handlers only describe and answer, in one-shot workers with the
usual limits.

### Device sign-in

```toml
[credential.deviceFlow]
authorizationUrl = "https://example.com/login/device/code" # HTTPS, declared host
tokenUrl = "https://example.com/login/oauth/access_token" # HTTPS, declared host
clientId = "the publisher's public client ID"
scopes = [
  "read",
  "write"
]
```

The host runs the OAuth 2.0 device authorization grant (RFC 8628) for the owner:

- `POST /api/plugins/{id}/device-flow` → `{ id, state: "pending", userCode,
  verificationUri, verificationUriComplete?, expiresAt }`. The owner enters the code
  on the service's page.
- `GET /api/plugins/{id}/device-flow/{flow}` → the same with `state` `pending`,
  `completed`, `failed` (with `message`) or `expired`; `DELETE` cancels.
- The host polls the token endpoint at the advertised interval (`slow_down` adds five
  seconds) and keeps the token in memory, at most eight sign-ins at once, ten minutes
  after completion. A restart loses pending sign-ins.
- The editor sends `credentialFlow: <flow id>` instead of `token` to the form handler
  and to the save, which consumes the sign-in. The browser and the script never see
  the token. Tokens are not refreshed: use an OAuth application whose tokens do not
  expire.

### Authorization-code OAuth and refresh

Packages can declare `credential.oauth` instead of `credential.deviceFlow`:

```toml
[credential.oauth]
authorizationUrl = "https://service.example/authorize"
tokenUrl = "https://service.example/token"
revocationUrl = "https://service.example/revoke" # optional
clientIdSetting = "clientId" # declared string setting
clientSecretSetting = "clientSecret" # optional declared secret setting
scopes = [ "mail.read" ]
requireRefreshToken = true # optional; unattended services should require it

[credential.oauth.scopeSettings]
allowSend = [ "mail.send" ] # declared boolean setting

[credential.oauth.authorizationParameters]
access_type = "offline"
```

All endpoints must use HTTPS, exact declared hosts and no query or fragment. The
host owns state, owner binding, PKCE S256, callback code exchange and refresh.
Packages and browser/native clients never receive tokens. The owner registers
`GET /api/plugins/{id}/oauth`'s `callbackUrl` with the provider. `POST` to the same
path starts a ten-minute sign-in (`instance?`, `settings`, `secrets?`);
`GET`/`DELETE /api/plugins/{id}/oauth/{flow}` inspect or cancel it. The owner opens
`verificationUri` and returns through `/plugins/oauth/callback`, whose page removes
the single-use query before posting to Beacon's authenticated callback. This URL
belongs to Beacon, not an installation: every installation shares it, including
when installation subdomains are enabled. The random state includes an
installation routing hint. Beacon checks the current session, CSRF and ownership,
then uses its ordinary relay to forward the complete state and provider response.
The destination installation alone verifies its pending owner-bound state and
PKCE and exchanges the code. Beacon stores no plugin flows or credentials.
Web and Android poll the same owner-bound flow (`pending`, `exchanging`,
`completed`, `failed`); `credentialFlow` validates the draft and saves the encrypted
access/refresh record once. A changed client or increased permissions requires
reconnecting. Sign-in is bound to its package digest, original instance revision,
client settings and selected scopes. A `form` handler also receives trusted
`previous` settings so it can reject a reconnect to a different remote identity.

Before authenticated HTTP, refresh is serialized per instance and preserves a
refresh token when the provider omits it. Publication rechecks the current instance
revision. Legacy static and device-flow credentials keep their existing behavior.
`POST /api/plugins/{id}/connections/{instance}/disconnect` disables the account,
removes local credentials and attempts declared provider revocation. The response
distinguishes local disconnection from unconfirmed remote revocation. A provider
may revoke other tokens for the same remote account/client; deleting an instance
only removes local credentials and data.

Public MCP and the opted-in installation tool expose `list_plugins` and
`manage_plugin` for setup, sign-in, forms, instance lifecycle and panels. Neither
accepts `agentIds`; instance grants remain in the owner's agent editor (ADR 0051).
Both accept `data.sharedFrom` to reuse a granted instance's shared setup.
An installation management agent also needs the separate enabled instance grant
before using its saved credentials in a form, panel, test or reconnect. Management
and account grants are checked again through asynchronous handlers and refreshes.

### Private transfer files and MIME

The `files` capability adds `cairn.files.create`, `append`, `read`, `delete` and
`composeMime`. Handles belong to one instance/owner and never name host paths.
Files expire after one hour and after installation restart. Limits: 64 KiB decoded
chunks, 40 MiB/file, 200 MiB/instance, 1 GiB and 256 files/installation. File bytes
are private temporary files; only metadata and requested chunks enter Luau.
`append` requires the exact current `offset`; `read` returns base64url data and a
nullable `nextOffset`. Both padded and unpadded base64url responses are accepted.

`http.request` can upload `bodyFile: {id, format: "raw"|"json"|"base64url",
jsonPointer?}`. Base64url fills an existing JSON object's field (for example
`/message/raw`), keeping inline request metadata separate. `responseFile` saves a
large response directly; optional `jsonPointer` and `encoding` extract a provider's
base64/base64url field. File transfer requires both approved HTTP and files
capabilities before a request is sent. Ordinary inline limits remain unchanged.

`composeMime` accepts explicit `from`, recipient arrays (`to`, `cc`, `bcc`,
`replyTo`), `subject`, `text`/`html`, optional Message-ID/references and attachments
by private handle (`id`, `name`, `mediaType`, optional inline `contentId`). The host
validates headers and builds RFC5322/MIME Unicode and multipart content. It returns
a `message/rfc822` handle and its `messageId`. `messageKey` produces a deterministic
Message-ID scoped to the instance, useful for reconciling uncertain sends.
`contentDigest` hashes the stable composition input and the actual attachment
bytes, excluding generated MIME boundaries and dates. A changed attachment cannot
silently replay an earlier send receipt. Local byte/I/O/CPU measurements and their
limits are recorded in [the transfer measurements](PLUGIN-TRANSFER-MEASUREMENTS.md).

The run-local CLI transfers conversation files with any approved files-capable
package: `cairn plugin PACKAGE --upload INSTANCE PATH`, `--download INSTANCE HANDLE
PATH`, `--delete-file INSTANCE HANDLE`. Each chunk rechecks the run and current
instance grant. Downloads create a new private destination and remove incomplete
files; they do not overwrite existing paths. No native filesystem capability is
given to package code.

### HTTP mutation receipts

`http.request` optionally accepts an `idempotencyKey` (200 characters maximum) and
stable `idempotencyInput`. The fingerprint also binds the native instance revision
and, when present, resource/grant generations, the resource operation's requester
and its OAuth authorization. Plugin input cannot remove these checks. Reusing a key with changed
authority returns a conflict, including after reconnecting an account. Receipts
created before these checks also conflict rather than returning an unverified
result or repeating a potentially successful write. The stored shape and key
namespace are unchanged; the new fingerprint requires the
[unreleased Slack compatibility gate](SLACK-PLUGIN-COMPATIBILITY.md), and old
readers must not be used after migration.

The host atomically claims a key scoped to the instance
before sending. Completed responses replay from the durable receipt; different
intent with the same key is refused. An interrupted/pending request is never sent
again automatically: reconcile with the provider, then explicitly choose a new
key if appropriate. Receipts survive restarts; deletion of the instance removes
them. They are limited to 10,000 per instance, with response bodies up to 64 KiB;
larger completed bodies replay the status with `resultOmitted: true`. File responses
and safe GET/HEAD requests cannot use mutation receipts. The host does not promise
provider-side exactly-once delivery or automatically retry confirmed HTTP errors.

See the bundled [Gmail package](../plugins/gmail/README.md) for full API coverage,
setup, Google restrictions and examples using these generic capabilities.

### Run credentials

```toml
[runCredentials]
files = [ { path = ".config/tool/hosts.yml", content = "token: {{credential}}\n" } ]
git = [ { url = "https://example.com", username = "x-access-token", password = "{{credential}}" } ]
gitIdentity = { name = "{{settings.name}}", email = "{{settings.email}}" }
```

For each agent holding an enabled instance (one per package: Cairn refuses a second
grant), the host renders these templates when a run is prepared and each time it
resumes. Placeholders are `{{credential}}`, `{{secrets.KEY}}` and `{{settings.KEY}}`;
a value with control characters, or a missing one, leaves its item out.

- Files land in the run's home, private (0600 in 0700 directories), under a tool's
  configuration directory: `.config/<tool>/…` (two to five more segments of letters,
  digits, `.`, `_` and `-`), never `.config/git` or `.config/cairn`. Shell start-up
  files, `.gitconfig` and provider state are out of reach. When two packages name the
  same path, the first one keeps it. At most eight files of 8 KB.
- Git credentials go to `~/.config/cairn/git-credentials`, a credential-store file
  Cairn owns. One `credential.<url>.helper` line per prefix, and the identity, live in
  `~/.config/cairn/gitconfig`, which the run's `.gitconfig` includes, so a resumed run
  follows the current grants. URLs are HTTPS prefixes on declared hosts.
- The instance's credential and secret values are masked in the run's output.
- `gitIdentity` is the commit author when the installation has no global Git
  identity; empty values leave it out.
- A withdrawn grant empties the files it wrote on the next resumption, including in a
  resumed VM; the end of the run removes them. Native nodes bind them read-only from
  staging, so they never reach the journal.
- The host's own fetches (seeding a run, opening a project, imports) use the same Git
  credentials: the instance that imported the project first, then the agent's, then
  any enabled instance serving the remote. An SSH remote switches to HTTPS only when
  an instance serves it.
- Shared executions without a VM (local development without `RUNNER_URL`) receive no
  run credentials.

### Secret patterns

`secretPatterns` lists up to eight regular expressions of the service's token
formats. Cairn masks matches in run output and tool payloads, like its own formats,
from the first output of runs resumed at startup. A pattern matching empty or
ordinary text (numbers, dates, words, paths, hashes) is refused.

### Project sources

```toml
[[projectSources]]
name = "repositories"
title = "Repository"
```

The `projects` handler receives `{ op = "list", source, page, query, instance }` and
returns `{ items = { { id, name, projectName?, description?, defaultBranch?, badges?,
updatedAt?, url?, origin? } }, nextPage? }` (200 items at most; `origin` marks imported ones), or
`{ op = "resolve", source, id, instance }` and returns `{ id, name, description?,
origin, defaultBranch? }`. `origin` must be HTTPS on a declared host.

- `GET /api/plugins/{id}/projects?source=&instance=&page=&query=` lists, with
  `imported` computed by Cairn.
- `POST /api/projects/import` with `{ pluginId, source, instance, id, name?,
  description?, baseBranch? }` clones the resolved remote with the instance's run Git
  credentials into a new managed directory and registers the project with
  `source: { pluginId, source, instance, id }`. Repeats return the same project.

### Conditions

```toml
[[conditions]]
name = "checks"
project = true
description = "What completes, what the results mean."

[conditions.inputSchema]
type = "object"
required = [ "sha" ]

properties.sha = { type = "string", pattern = "^[0-9a-f]{40}$" }
```

`wait_for` provides native `delay`, `at` and `run_finished` conditions. Plugins add
service-specific conditions through the same `condition` handler; no host-side
service-specific dispatch is needed. Temporal waits take no `timeoutSeconds`;
plugin conditions require it.

Granted agents see each condition in `wait_for` as type `<plugin>_<name>` (plugin
IDs have no `_`, so types never collide), with `projectId` when `project = true`; Cairn records the plugin
(`pluginId`) and checks, before every observation, that the agent still holds an
enabled instance and that the project still has the origin the run was granted.

The `condition` handler receives `{ condition, input, project?, agent, instances }`
(`project`: `{ id, name, origin, baseBranch, source }`) and returns:

- `{ state = "waiting", display = { title, short, details } }` while pending; the
  display is shown in conversations;
- a result such as `{ state = "completed", …, display = { tone, title, detail } }`
  (tones `ok`, `error`, `warning`, `neutral`): the conversation resumes with it;
- `{ state = "unavailable", message }` for a permanent failure: refused when
  registering, a result afterwards.

An error is transient: Cairn retries until the deadline. `checkAfter` (seconds, 10 to
3600) delays the next check; otherwise conditions are checked every 30 seconds, in
batches of 16 shared by identical requests. Withdrawing the grant ends the wait as
`unavailable`.

### Skill sources

```toml
[[skillSources]]
name = "repository"
title = "Repository"

[[skillSources.fields]]
key = "repository"
title = "Repository"
type = "string"
required = true
```

Fields use the settings schema (`string`, `url` or `text`). The `skills` handler
receives `{ op = "resolve", source, location, instance }` and returns `{ version,
location?, folder?, label?, url?, message?, date? }`, or `{ op = "files", source,
location, version, folder, instance }` and returns `{ files = { { path, size, digest,
executable?, link?, download = { url, headers? } } }, truncated? }` with paths relative
to the folder (2,000 at most). Cairn downloads each file from a declared host with the
instance's credential (`accept`, `user-agent` and `x-*` headers allowed), refuses
links and oversized skills, picks the folder holding `SKILL.md` and compares digests
between versions.

`POST /api/skills/{scope}/import` takes `{ source: "plugin", pluginId, skillSource,
instance?, location, folder? }`; the source status compares `version`s and
`…/source/update` takes `{ version, base }`. A skill's source is `{ pluginId, source,
instance, label, location, url, folder, version, importedAt, modified }`; a removed
instance falls back to another enabled one.

## Program installers

Keep exactly one `manifest.json` or `manifest.toml` at the package root. Both
formats use the same field names, defaults, validation and approval digest. The
upload endpoint accepts a JSON manifest object as before, or manifest text with
`manifestFormat: "json" | "toml"`; it persists canonical JSON. The web accepts
both formats. Android reviews both formats through the normalized interface;
package file uploads continue to use the web.

For a managed executable, declare:

```toml
capabilities = [
  "http",
  "exec"
]
hosts = [ "downloads.example.com" ]

[[programs]]
name = "tool"
installer = "programs/tool.luau"
```

The module must export `resolve(input)` and `install(input)`:

```lua
return {
    resolve = function(input)
        -- Select a compatible stable version for input.platform.os / .arch.
        local response = cairn.http.request({ url = "https://downloads.example.com/latest" })
        -- Example publisher returns { version, url, sha256?, format, executable }.
        assert(response.status == 200)
        return response.json
    end,
    install = function(input)
        local release = input.release
        local archive = cairn.download({ url = release.url, sha256 = release.sha256 })
        return cairn.archive.executable({
            archive = archive.id, format = release.format, path = release.executable,
        })
    end,
}
```

`resolve` returns an exact `{ version, url, sha256?, format, executable }` descriptor.
`format` is `zip` or `binary`. With `binary`, `executable` is a logical filename.
Discovery can only fetch public HTTPS metadata from declared hosts (GET, 1 MiB,
15 seconds, no redirects). It cannot read instance credentials or use storage,
program execution or installation primitives. `install` receives exactly the
selected descriptor. Its only capabilities are `cairn.download` and
`cairn.archive.executable`, restricted to that artifact and entry. These operations
are refused in ordinary instance/agent handlers. Handles never expose file bytes
or filesystem paths to Luau. Downloads are streamed, limited to 64 MiB and 90
seconds; the installer event has a 120-second deadline. One-shot calls with an
unprepared dependency have a 180-second enclosing budget so preparation can finish;
subsequent calls retain the normal 35-second budget, and each executable still has
its own 30-second limit. The run-local plugin CLI allows 190 seconds for the
private channel, covering that cold preparation budget. ZIPs have at most 256
entries; links and unsafe paths are refused. Only the selected regular executable,
at most 128 MiB, is written. No arbitrary shell/install scripts are run.

Cairn verifies an expected SHA-256 when provided. Otherwise provenance is HTTPS to
the approved publisher, and the actual archive hash is recorded at installation;
this is not publisher signature verification. Both modes record the executable
hash, checked before each use. A restart reuses that exact binary. A corrupted lock
or binary fails closed; it does not silently download a replacement.

Uploaded packages prepare their programs when approved, before switching the
active version. Bundled programs prepare on first use. Preparation is shared by
all instances and serialized; no instance secrets are available. Web and Android
show installed/candidate versions and offer Check, Install and Restore. Package
and program updates are independent. There is no periodic check or automatic
upgrade in this initial implementation. Owner-only routes are:

- `GET /api/plugins/<id>/programs`: installed, previous and candidate releases.
- `POST …/programs/<name>/check`: resolve and save a candidate and its digest.
- `POST …/programs/<name>/install` with `{ digest }`: install that candidate.
- `POST …/programs/<name>/rollback`: restore the retained previous executable.

These program actions return `{ operation: ID }` immediately. Poll
`GET /api/plugins/packages/operations/ID` for `{ state, result?, error? }`, where
state is `running`, `completed` or `failed`. Approval/activation of an uploaded
installer-backed package uses the same operation response; ordinary packages keep
their synchronous summary response. This keeps downloads independent of the
30-second HTTP relay deadline. Operations have a nine-minute total budget, four
concurrent slots, and 64 recent in-memory results. A server restart loses operation
status, not the atomic program/package locks; refresh the plugin and retry if
needed. Web and Android perform this polling, including approval failures. Cold instance tests, views/actions, setup forms/activation and resource forms/saves
also use this response when they may prepare an executable; once prepared, their
ordinary synchronous responses are unchanged. Instance saves without a form
handler remain synchronous and do not download programs.

Locks and executables live under `<data>/plugin-programs/`, scoped by package
digest and server platform. Failed preparation preserves the current version;
older files remain available for rollback. See ADR-0049 for the epoch-3 migration
and deployment recovery contract.

## 1Password

The bundled 1Password package is an ordinary package (`plugins/onepassword/`):
service accounts are instances with a secret token and agent grants, `op` is a
declared program, and its commands, `onepassword` tool, instructions, panel and test
are declared in its manifest. Accounts saved before plugins existed move into instances at
startup with the same IDs, grants and tokens. Metadata is available through the tool;
secret reads only through the CLI, consumed inside a pipe or script:

```sh
cairn plugin onepassword accounts
cairn plugin onepassword fields --account ACCOUNT_ID --vault VAULT_ID --item ITEM_ID
cairn plugin onepassword read --account ACCOUNT_ID op://vault/item/password |
  docker login --username example --password-stdin
```

The CLI uses the same private VM channel and run grant, including remote nodes.
It never authenticates `op` inside the conversation VM. The service-account token
stays in the installation; `op` runs with the declared program rules above, the
package's fixed read-only arguments and the post-I/O grant/rotation check. Secret
stdout has no added newline and is not persisted by the plugin host. Do not run a bare read,
enable shell tracing or print the captured value: the ordinary command tool still
records its output. Already delivered secrets cannot be revoked.

The CLI expects an active conversation VM with the current Cairn binary and guest
channel; VMs started by an earlier binary must restart to use it. Non-VM development
commands do not acquire a new manager network route.

Existing 1Password accounts migrate to plugin instances at startup. This is a data
compatibility break (epoch 2); follow the [migration and rollback plan](PLUGIN-V2-MIGRATION.md)
before deploying the first plugin API v2 release.

## GitHub

The bundled GitHub package (`plugins/github/`) is an ordinary package built on the
[integrations](#integrations) above. An instance is a GitHub account:

- **Sign-in.** Atelier → Plugins → GitHub → add an account, then “Sign in with GitHub”
  (the OAuth App “Cairn installations”, scopes `repo`, `workflow`, `read:org`), or
  paste a token: a fine-grained token limits agents to the repositories and
  permissions chosen on GitHub. The form checks the account, warns when an OAuth
  token lacks `workflow`, and keeps the login and commit identity (name and noreply
  address) as hidden settings. The panel and test show the account.
- **Runs.** Agents granted an account on their page get `~/.config/gh/hosts.yml` for
  the `gh` CLI, Git credentials for `https://github.com`, the commit identity (unless
  the installation has its own) and the package's instructions. One account per
  agent. Token formats are masked in run output.
- **Projects.** Projects → import from “GitHub repository” lists the account's
  repositories (owned, collaborations, organizations; 50 per page).
- **Waits.** `github_workflow` (project, run ID, full commit SHA; a run GitHub does
  not show is retried until the deadline, since it may hide a private repository) and
  `github_pr_checks` (project, PR number, full head SHA), with the same results as
  before: required checks from classic branch protection and rulesets, `superseded`,
  `closed`, `no_required_checks`, limits of 500 checks. SSH remotes in every spelling
  Cairn stores are recognized.
- **Skills.** “GitHub repository” imports a folder at a branch, tag or commit;
  `repository` also accepts a `https://github.com/owner/name/tree/<ref>/<folder>`
  address. Large repositories need the folder: a recursive listing over 1 MB is
  reported as partial.

Plugin HTTP follows no redirects: a repository renamed or transferred on GitHub answers
with a redirect, which the package reports as unavailable. Register the project again
with its new name.

Installations that had the earlier built-in GitHub move into this package at startup:
see the [migration](GITHUB-PLUGIN-MIGRATION.md).

## Discord

The bundled Discord package (`plugins/discord/`) is an ordinary package: a bot is
an instance with a credential, its Gateway session is Luau over `socket` and
`inbound`, and its setup, panel and test are its `form`, `view` and `test` handlers.
Atelier → Plugins → Discord shows each bot like any delegating instance: status and
who it is signed in as, allowed people and requests, answering agent, hourly usage,
recent activity, what its agent can do, and its panel (server, channel, mode).

Adding a bot, on the web or Android:

1. **Token.** Create an application in the Discord Developer Portal, reset the bot
   token and paste it into the private field; never send it in chat. The form handler
   checks it right away and shows the bot's name and an invite link.
2. **Server and channel.** The invite requests only what the bot needs (below). The
   form lists the servers the bot was invited to, then the channels with Discord's
   permission resolution: channels where the bot can't read or reply are disabled,
   and channels without thread permissions say replies stay in the channel.
3. **Reading.** By default the bot sees mentions only, which needs no privileged
   intent. “Read replies without a mention” (Message Content, enabled in the Portal
   too) also delivers replies in its threads or to the bot; “Answer every message in
   the channel” turns it on.
4. **People, agent and limits**, like every delegating plugin: approve people as they
   ask (recommended), only listed Discord user IDs, or anyone with a read-only agent;
   an agent restricted to one project at most without installation access; an hourly
   limit.

The transport worker follows Discord's Gateway lifecycle, checked against
`@discordjs/ws`:

- new sessions connect to the URL from `GET /gateway/bot` and wait for
  `session_start_limit.reset_after` when no session start is left today (past the
  limit Discord resets the token); session starts are reserved per credential across
  instances, and Identify writes are paced five seconds apart without blocking
  heartbeats. Failed budget lookups retry without identifying, and resets trigger a
  fresh lookup. Observation expiry includes the time spent dialing and waiting for
  Hello, so an old window never lowers the new window’s balance. Resumes use the session's `resume_gateway_url`;
- Hello must arrive within 60 seconds and READY within 15 seconds of Identify,
  otherwise it reconnects (starting over after a missing READY);
- heartbeats start after a random part of the interval and carry the last sequence;
  a missing acknowledgement means a zombie connection: close with 4000 (keeping the
  session) and resume; Discord's heartbeat requests (op 1) are answered at once;
- Reconnect (op 7) resumes; Invalid Session (op 9) waits 1 to 5 seconds, then resumes
  when Discord allows it or identifies again;
- close codes 4004 and 4010–4014 stop the bot until it is reviewed (token, shards,
  version or intents); 1000, 1001, 4003, 4005, 4007, 4008 and 4009 start a new
  session; others resume;
- a GUILD_CREATE over 1 MB (large servers) keeps the session going: the worker reads
  its sequence and the threads’ routing fields through an array projection. No REST
  hydration blocks heartbeats or interactions.

The bot token is injected by the host and never seen by the script. It tracks threads under the configured channel, acknowledges button presses
within Discord's three-second limit and admits only messages for the bot: mentions in
the channel or its threads, plus, with Message Content, replies in those threads or to
the bot. The event worker does the rest:

- reacts 👀 on receipt and opens a thread named after the request, so each
  conversation lives in its own thread (it stays in the channel if the bot can't
  create threads); attachments go to the agent with the message;
- while the agent works, keeps “typing…” visible and edits one status message with
  the current step and a Stop button; “stop” or “cancel” in the thread does the same;
- posts the agent's questions with one button per option (free-text fields are
  answered by replying in the thread; private fields must be answered in Cairn);
- posts the final answer in Discord-sized chunks with deterministic nonces, uploads
  the turn's deliverables as files (up to 10 MB each, otherwise noted as available
  in Cairn), then marks the request ✅, ❌ or ⏹;
- answers access requests once, using storage to remember who was told;
- with “Answer every message in the channel”, treats every new message in the channel
  as a request. Messages that don't mention the bot are handled quietly: the agent is
  told it may answer exactly `NO_REPLY`, in which case nothing is posted; otherwise
  the thread opens only when the answer, a question or progress needs to be shown.

The bot needs View Channel, Send Messages, Read Message History, Add Reactions,
Attach Files, Create Public Threads and Send Messages in Threads; the generated
invite requests exactly these.

The form handler runs on the installation with the pasted or saved token; the token is
never returned to the browser or the script. Names are display labels only;
authorization always uses IDs. Discord avatars are not loaded, keeping the browser off
Discord's CDN.

Live test: activate the bot, wait for Online, mention it from an approved person and
confirm a shared Cairn conversation appears and its final answer returns to Discord.
Check the 👀 reaction, the thread, the status message and its Stop button, a
question with buttons (ask the agent to confirm something), an attached screenshot,
and a deliverable coming back as a file. Then mention it from someone new and
approve or deny them, pause it with “Accept new
messages”, and restart the manager with work pending. Check continuity, denial and
revocation.

The owner delegates directly to Discord identities; they need no Cairn account and
receive no installation browsing or management access. Each connection/channel/user
has its own conversation binding. Outputs appear in the configured Discord channel
and are visible to everyone who can read that channel. Conversations remain shared
with Cairn installation members. Use an agent whose credential access is appropriate
for that audience. Changing agent access invalidates the connection until the owner
reviews and saves it again. Delegations are bound to the owner's current installation
access; authority unavailability postpones admission and delivery.

A bot runs independently of conversation VMs while the installation worker is enabled.
Its transport worker keeps Gateway heartbeats, acknowledgement checks and resumable
sessions; the host paces reconnections. It uses guild and guild-message intents,
ignores bots and webhooks, and Cairn checks people's grants before keeping message
content. People approved,
denied or removed on the bot's page apply to the next message without restarting the
Gateway session. Accepted messages enter the connection's durable event queue and
deduplicate for 30 days. Submission and conversation binding are
atomic; per-connection hourly admission and pending-work ceilings limit consumption.

Only the final assistant answer for the admitted message's completed turn is sent;
tool results, reasoning and unrelated turn history are not exported. Replies are
split to fit Discord's UTF-16 length limit and disable mentions. Pending replies and
chunk progress survive manager restarts. Rate limits defer retries, including the
next reply in a global-limit pass. Deterministic nonces suppress duplicates within
Discord's nonce window; a crash followed by a late retry can still duplicate a post.
Replies expire after 24 hours and are capped at 16,000 characters. Gateway resume
state is currently in memory, so events missed while the installation is offline
are not recovered after a manager restart. Slash commands, direct messages and
reading channel history are deliberately out of scope. A bot runs one shard: bots in
2,500 servers or more stop with Discord's “sharding required” close. Transport
compression and presence updates are not used. Button presses arrive over the Gateway
only when the application has no Interactions Endpoint URL in the Developer Portal.

## Validation

Synthetic tests cover manifest validation, worker sandboxing, persistent script
state, refused-call codes, capability denial, memory limits, per-event deadlines,
API incompatibility, owner-only management, encrypted credentials, MCP secret-read
denial, live grants, ingress deduplication, rate limits, changed agent access,
final-answer correlation, access requests and approvals. The real Discord package runs
end to end against a mock Discord API and a local Gateway: Identify with the injected
token, heartbeats with the last sequence, replay deduplication, Resume after a
reconnect request, an invalid token stopping the bot, the lifecycle rules above
(gateway URL and session budget, Hello and READY timeouts, zombie connections,
Invalid Session waits, close codes, paced Identify) and a GUILD_CREATE over 1 MB, the setup form's permission
resolution, invite link and errors, socket host checks, mention handling, chunked
replies, rate-limit retries with stable nonces, disabled mentions and one-time
notices. Registry tests cover
upload validation, approval, tamper detection, updates, rollback, uninstall and an
installed package using timers, storage and credentialed HTTP limited to declared
hosts without following redirects. Browser journeys cover setting up a Discord bot
through its package form, people management, and installing, approving, connecting
and uninstalling a package, on desktop and mobile.
These are local fixture tests, not evidence of a real Discord connection or a
production deployment.

## Setting constraints and compatibility (F6)

`maxLength`, when present, must be a JSON/TOML integer from 1 through 10,000
for non-secret fields, or through 16,384 for secrets. Cairn rejects a larger
text declaration with the explicit 10,000 limit instead of accepting a manifest
whose declared limit would be silently capped on save. Text lengths count Unicode
characters; secret lengths count UTF-8 bytes, as before. Without `maxLength`, the
save limits remain 200 for `string` and dynamic choices, 2,000 for `text`/`url`,
and 16,384 bytes for secrets. `minimum` and `maximum` must be finite numbers;
when both are present, `minimum` must not exceed `maximum`.

A non-null `default` must have the field's type and satisfy its length, numeric
range, choices and whole-value pattern. Text defaults follow save behavior:
length/control characters are checked before trimming, patterns after trimming;
required text cannot trim to empty, and nonempty URLs must use HTTP(S). Optional
empty text retains the existing pattern exemption. Secret defaults and dynamic
choice defaults remain forbidden. A null default on a non-secret field means
no default. No values are truncated.

This tightens the set of persisted manifests that the registry can read. The
installation compatibility epoch advances from 3 to 4. The subsequently reviewed
[automatic transition to epoch 9](AUTOMATIC-MIGRATIONS.md) now covers this boundary
under [the production compatibility contract](PRODUCTION-CAIRN.md#installation-data-compatibility).
Both bundled manifests and ordinary valid installed packages
retain their settings, serialized form and approval digests; no database, secret
or instance record is rewritten.

When qualifying an epoch transition, inspect installed manifest settings
for negative, zero, fractional or oversized `maxLength`, reversed numeric bounds,
and defaults that fail the rules above. The registry revalidates every version on
read: an incompatible installed version fails with a validation error, remains on
disk, and cannot run; catalog version errors identify it. Do not silently clamp,
normalize or reapprove its contents.

Forward repair: publish a corrected package with a new version under the same
plugin ID, upload it, review and approve that version. Choose bounds that cover
existing instance values, or deliberately correct those values when editing the
instance. The registry then activates the approved version; existing instance IDs,
settings, encrypted secrets and grants stay in place. If repair or approval fails,
keep the original files and records and fix the package before retrying. Preserve
the previous binary and an explicitly prepared pre-transition data backup for a
reviewed rollback; do not run the previous binary on epoch-4 installation data or
lower the compatibility epoch to bypass the rollout gate. No automatic data
backup/restoration or production manifest inventory is claimed here.

The Android `Int?` decoder's behavior for a fractional `maxLength` has not been
executed on a JVM or device in this change. Backend admission now rejects that
manifest before it can reach either client's field decoder.

## Client form values (F5)

Instance and resource form callbacks receive the same partial typed values:
missing or explicitly cleared fields do not gain defaults before Luau runs.
Final server save completion and required-field validation remain unchanged.

Web instance editors and view forms use pure value transformations, compared with
Android's native form helpers using `tests/fixtures/plugin-form-values.json`.
Opening a form applies defaults to missing values; an explicit null or a later
clear does not reapply a default. Nonprimitive answers do not coerce scalar
controls: a boolean answer such as `[true]` or `["true"]` becomes false, matching
Android. Only declared non-secret fields are settings,
including hidden fields populated by the package. Nonblank text is trimmed,
numbers are sent as JSON numbers, and blank text/numbers are omitted. Choice
identifiers are preserved exactly, including whitespace accepted by the manifest;
an empty single choice is omitted. False and
empty multiple choices remain explicit. Secrets are never prefilled from a view's
`values` or instance settings. Blank secrets are omitted, preserving saved instance
secrets. Instance secrets follow the existing trimmed save contract; nonblank view
form secrets reach the action exactly as typed, matching Android.

Both clients use Kotlin's existing `Char.isWhitespace` predicate for text trimming
and secret blankness: Java `Character.isWhitespace || Character.isSpaceChar`.
The exact set is U+0009–U+000D, U+001C–U+0020, U+00A0, U+1680,
U+2000–U+200A, U+2028–U+2029, U+202F, U+205F and U+3000.
U+0085 NEXT LINE and format characters (including U+FEFF, U+200B and U+2060)
are meaningful. This differs from Rust's Unicode `White_Space` trimming and
JavaScript's built-in `trim`; neither defines the client contract. The shared
`plugin-form-whitespace.json` corpus covers all ASCII/C1 controls, separators,
and format/nonspace boundaries, as singletons and around nonblank text. Its
blank set is also checked exhaustively against the pinned Kotlin/JDK Char domain.
Choice IDs use exact bytes and required selects still use null/empty missingness.

These helpers do not make control characters valid instance settings: the server
independently rejects them on instance saves. Admitted panel `values` can contain
controls; action secrets keep their raw bytes after the shared blank filter.

These are client value transformations, not validation of an action. A plugin
still validates its action input. The server's F6 manifest constraints, package
admission/review, instance validation and completion handler remain unchanged.

# Installation plugins

A plugin is a package: a `manifest.json`, a `main.luau` and, optionally, other Luau
modules it loads with `require`. Cairn runs each package
in its own long-lived `cairn plugin-worker` process and reaches it only through
events; the package reaches Cairn only through host capabilities its manifest
declares and the owner approved. Plugin code never receives `Service`, SQLite, the
vault key or a connection's credential, and identities always come from the host.

Cairn has no code specific to a plugin. Discord's Gateway session, its setup form,
its panel and its test are Luau, like 1Password's commands and tools: a package
written outside this repository can do everything the bundled ones do.

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
  capabilities.rs       storage, http, timers, exec, inbound and conversations for scripts
  scope.rs              One-shot handlers: who acts, which instances, rechecks
  agents.rs             Commands, Cairn MCP tools and prompt instructions for agents
  panels.rs             Declarative views, actions and tests for the owner
  programs.rs           `exec`: declared programs without a shell
  work.rs               Admission, access requests, event queue, replies
  cli.rs                `cairn plugin <plugin> <command> …` inside conversations
  legacy.rs             Moves earlier 1Password accounts into instances
plugins/                Bundled packages (1Password, Discord), embedded by installation's build.rs
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
  manifest.json
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

- `capabilities`: `conversations.submit`, `conversations.control`, `storage`, `http`,
  `timers`, `exec`, `socket` and `inbound`;
- `hosts`: the hostnames `http` (HTTPS) and `socket` (WSS) may reach. `*.example.com`
  covers every subdomain of `example.com`, not the domain itself;
- `credential`: how Cairn adds an instance's token to authorized requests
  (header and prefix); the script only sets `auth = true`;
- `settings`: the instance form, in order (`key`, `title`, `type`, `required`,
  `hidden`, `description`, `placeholder`, `section`, `default`, `maxLength`,
  `minimum`, `maximum`, `pattern`, `options`, `dynamic` and `refresh`).
  Types: `string`, `number`, `boolean`, `text`, `url`, `select`,
  `multiselect` (with `options`, or `dynamic: true` for choices the package's form
  handler provides) and `secret`;
- `instances`: `{ singular, plural }`, how the interfaces name an instance;
- `brand`: `{ color: "#rrggbb", path }`, the package's mark (an SVG path on a
  24×24 grid);
- `programs` (with `exec`): `[{ name, path, description }]`, absolute paths the
  package may run;
- `agentAccess`: `{ description }`; the owner grants instance-wide access or
  individual resource operations. Only granted agents receive the corresponding
  commands, tools and instructions;
- `commands`, `tools`, `instructions` (need `agentAccess`), `instanceView`,
  `instanceTest` and `instanceForm`: see [Contributions](#contributions).

## Connections and delegation

Scripts receive validated settings in their `start` event; credentials stay in the host.

Every plugin uses the same connection model: name, settings, optional credential
(encrypted in the vault), and, when the package can start conversations, an owner
delegation: one dedicated agent (one project at most, no installation access), the
people allowed (listed, approved as they ask, or anyone), and an hourly message limit.
“Anyone” is meant for public channels such as customer support: it requires an agent
with a read-only sandbox and no installation, GitHub, MCP or 1Password access
(rechecked before every effect, so a later grant suspends the connection), adds a
per-person hourly limit, and lets only listed people or a conversation's starter
answer for it or stop it. The
connection is bound to the owner's current access and the agent's recorded policy.
The web and Android render this delegation, the people allowed and pending requests,
hourly usage and recent activity for every package with `conversations.submit`.
Routes are generic: `/api/plugins/{id}/connections[/{connection}]` with `enabled`,
`test` and `people/{user}/{approve|deny|remove}` actions, `/api/plugins/{id}/form`, and
`/api/plugins/packages[/{id}/approve|activate]` for the registry. Approval includes
the digest displayed during review; changes require reviewing again.

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
later, for example after a platform rate limit. Capabilities exposed to scripts:

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
  hosts only, no redirects followed, 15-second timeout, 1 MB responses.
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

- `cairn.socket.connect{ url, auth?, after?, oversized? }`: WSS to a declared host,
  default port, no credentials in the URL. Reconnections slow down (1 s doubling to
  60 s) until the package reports `connected`; `after` waits longer (seconds, a day at
  most). `auth` adds the instance's credential or a secret to the handshake headers,
  like `http`. Messages up to 16 MB are read; above 1 MB the script receives only the
  JSON fields listed in `oversized` (such as `op`, `s` and `d.id`). Paths may have six
  segments; `*` projects every array entry (`d.threads.*.id`) without retaining its
  other fields. The complete worker frame, including JSON escaping, must fit in 1 MB.
  A smaller wire message can also require projection because of escaping. When even
  the projection cannot fit, `text` is null and the socket remains open.
- `cairn.socket.send(text, { auth = true | { secret }, pace?, budget?, name? }?)`: with
  `auth`, the host replaces `{{secret}}` with the JSON-escaped secret. `pace` (ms, a
  minute at most) queues sends outside the handler and spaces actual writes with the
  same credential across every instance. Reads, heartbeats and cancellation keep
  running while queued; at most 16 sends may be pending per worker. Completion
  callbacks share the event loop with socket reads and timers; cancellation is checked
  between callbacks. A worker exceeding 128 queued socket events is stopped.
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

`cairn.inbound.accept{ kind?, id, author: { id, name }, context?, body? }` (capability
`inbound`) hands Cairn something a person wrote, from the transport worker or from a
timer that polls an API. Cairn applies the instance's admission rules before keeping
anything: people who aren't allowed are dropped without a trace, approval mode
records a request without content, IDs deduplicate for 30 days, and at most 50 events
wait per instance. Accepted messages reach the package's `kind` handler (`message` by
default) with `context` and `body` merged under the host's `id` and `author`, and may
start conversations for that person. Returns whether it was queued.

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

`cairn.inbound.deliver(message)` returns `{status}`: `accepted`, `duplicate`,
`denied`, `requested` or `busy`. `accepted` means the event and its deduplication
marker were committed together; `busy` means they were not committed and the
sender can retry. Requests retain only identity and routing. Use this result when
deciding whether to acknowledge a signed HTTP or socket delivery. The shorter
`inbound.accept` returns true only for a newly accepted event. Neither API bypasses
the instance's admission rules or grants public requests conversation control.

### Setup drafts

Owner-only `/api/plugins/<plugin>/setup-drafts` supports POST and GET (a list of
`{items, nextCursor: null}`). Append `/<draft>` for GET, PATCH or DELETE. POST
accepts `{name, settings?, secrets?, token?}`; PATCH additionally requires the last
`revision`, as does DELETE. Missing required fields are allowed during setup;
provided fields still use the manifest's validation. Secret values remain encrypted
and responses expose only their names. An optional declared credential is stored
with the same encrypted TTL; responses expose only `credential: boolean`. Blank or
omitted tokens keep the saved credential. PATCH keeps the draft ID and original expiry.

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
configuration writes recheck their exact loaded package approval before committing.

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

### Resource collections

The `resources` capability declares related collections in `resources`:
`{name, title, singular, parent?, delegation?, settings}`. Each collection uses the
same field schema as instance forms; parent declarations must form an acyclic tree.
Resources belong to exactly one instance. Parent references cannot cross instances
or name an unrelated collection. Each instance can keep at most 256 resources;
values and encrypted secrets are bounded to 64 KiB each.

Owner management uses
`/api/plugins/<plugin>/connections/<instance>/resources/<collection>`: POST creates,
GET lists `{items, nextCursor}` in pages of 100. Append `/<resource>` for GET, PUT
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

### Resource grants and operations

`operations` declares `{name, collection, profile, scopes?, requests?}`. Profiles
are `read`, `contribute` and `manage`, in increasing order. Each request fixes
`{method, host, path, bindings?}`; a trailing `*` permits a path prefix. A binding
maps a request field to an admitted input scope (for example `channel` to
`channel`) or to a non-secret resource setting (`team_id` to `resource.externalId`).
The host verifies it in query/JSON/form arguments, rejects duplicates and
ambiguous raw/multipart bodies, and never relies on the script to enforce that
request's method, endpoint or bound fields.

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

On web and Android, open the active instance, then **Agent access** on its resource.
Choose an agent explicitly, select a profile or individual operations, optionally
limit scope values, and inspect the per-rule preview before granting access.
Resource grants are unavailable during inactive setup. Rules can be revoked with
confirmation; this does not remove permissions granted by other rules. After an
uncertain write, refresh the saved rules before another change. Connecting a
resource provider through OAuth does not grant any agent access by itself.

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
POST creates, GET lists `{items}`, and `/<route>` supports PUT with its last
`revision` or DELETE. A route contains `name`, `enabled`, exact string `match`
conditions on source context, `agentId`, optional `projectId`, `accessMode`,
`userIds`, optional `people`, and hourly limits. Defaults are 20 messages per hour
and five per person; the latter applies to everyone access. No route or person is
granted by default. The selected agent must have only the selected project's access
and no installation access; everyone access retains the stricter public-agent rules.

`cairn.inbound.deliver` supplies the configured `resource` and authentic source
`context` alongside its message. The host selects the most specific matching route.
A disabled override denies admission; equally specific matching rules conflict.
Packages cannot select an agent or route using message bodies. Access requests and
approvals are local to the selected route: append `/<route>/requests` for GET or
`/<route>/people/<person>` for POST `{action = "approve" | "deny" | "remove"}`.
Unapproved requests retain the person's identity, never message content.

Accepted events and deliveries retain native route authority outside the Luau
payload. Instance, ancestor-resource, route, owner-access and package revisions
are checked again before effects and after slow results. Adding a more specific
route can revoke older queued authority. Each worker is checked against the exact
manifest and source files it loaded, including across package approval changes.
Operation scopes with the same name as a route condition must match that condition;
operations on the source integration cannot select a different instance or an
unrelated resource even if the agent has a broader grant.

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
workspace does not block other conversations. Editing or withdrawing the queued
message from Cairn removes its automatic new-turn intent. Omitting `newTurn`
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
use HTTP Basic authentication at the token endpoint. A provider supporting both
may declare both. Slack's PKCE switch changes the app to a public client; do not
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
new provider request after a slow renewal.

#### Personal providers

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
person's active turn is refused; queue a new turn to use your own permissions.
Editing a queued message reattributes it to the authenticated editor.
Question answers follow the same check. The current installation owner can still
answer another person's question from Cairn; doing so suspends personal credentials
for that turn, including calls already in flight. The original author and their
authorization remain unchanged, and a new turn can use its own requester again.

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
- A script may call `cairn.fail(message)` to stop with a message for the person
  (300 characters at most). Any other error stays in the worker.

More capabilities:

- `cairn.exec.run{ program, args?, env?, stdin?, timeout?, instance? }` (capability
  `exec`) runs a declared program without a shell, with an empty environment and a
  private temporary home, 64 arguments, 256 KB of input, 1 MB of output and 30 seconds
  at most. `env` values are text or `{ secret = "<setting key>" }`: the host injects the
  instance's secret, which the script never sees. `LD_*`, `DYLD_*` and `HOME` can't be
  set. Returns `{ status, stdout, stderr }`. Programs run outside the Luau sandbox with
  whatever files and network they can reach: the review screen names each path.
- `cairn.http.request{ …, auth = { secret = "<setting key>", header?, prefix? } }`
  adds a secret field to one header of a declared host.
- `cairn.json.decode(text)` and `cairn.json.encode(value)` convert JSON.

Instance routes add `GET …/connections/{id}/view`, `POST …/connections/{id}/actions`,
the generic `POST …/connections/{id}/test` and `POST /api/plugins/{id}/form`
(`{ instance?, settings, secrets?, token?, changed? }`). Secret fields are saved with
`secrets: { key: value }`; an empty or missing value keeps the saved one and the API
only returns their names (`secretKeys`). Grants are saved as `agentIds`. Instances of
packages that need no worker (no socket, inbound messages, conversations or timers)
start nothing and show as ready while enabled.

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

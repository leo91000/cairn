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
  `minimum`, `maximum`, `pattern`, `options`, `dynamic` and `refresh`).
  Types: `string`, `number`, `boolean`, `text`, `url`, `select`,
  `multiselect` (with `options`, or `dynamic: true` for choices the package's form
  handler provides) and `secret`;
- `instances`: `{ singular, plural }`, how the interfaces name an instance;
- `brand`: `{ color: "#rrggbb", path }`, the package's mark (an SVG path on a
  24×24 grid);
- `programs` (with `exec`): `[{ name, path?, installer?, description }]`; exactly
  one of an absolute administrator-installed `path` or a package-relative Luau
  `installer`. Installers also require `http` and declared `hosts`; see below;
- `agentAccess`: `{ description }`; the owner grants instances on each agent's
  page (`plugins`) and only granted agents get the package's commands,
  tools and instructions;
- `commands`, `tools`, `instructions` (need `agentAccess`), `instanceView`,
  `instanceTest` and `instanceForm`: see [Contributions](#contributions);
- `runCredentials` (needs `agentAccess`), `secretPatterns`, `projectSources`,
  `conditions` (needs `agentAccess`) and `skillSources`: see
  [Integrations](#integrations).

## Connections and delegation

Scripts receive validated settings in their `start` event; credentials stay in the host.

Every plugin uses the same connection model: name, settings, optional credential
(encrypted in the vault), and, when the package can start conversations, an owner
delegation: the agent that answers, the people allowed (listed, approved as they ask,
or anyone), and an hourly message limit. Any agent can answer
([ADR-0068](adr/0068-owner-chosen-agents-for-bot-delegation.md)); the setup assistant
warns when it can manage the installation or open several projects, and, for anyone,
when it can also write, run commands or use MCP connections or plugins (GitHub,
1Password…). The agent must be able to open the delegation's project. “Anyone” is
meant for public channels such as customer support: it adds a per-person hourly
limit, and lets only listed people or a conversation's starter answer for it or stop
it. The
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
  whatever files and network they can reach: the review screen names each path or
  installer.
- A program may opt into a managed foreground cache daemon with
  `cache = { daemonArgs = ["daemon", "--timeout", "0"], socketEnv = "OP_SOCK" }`.
  Cairn starts the same approved executable with those static arguments, an empty
  environment and a private home/socket. No instance secrets enter the daemon's
  environment. Clients receive the socket through the reserved `socketEnv` variable,
  which scripts cannot override. The socket must belong to the foreground child;
  background-only daemons are rejected. Other programs keep their per-call home.
  Saved, enabled instances reuse a daemon across authorized calls to the same program,
  revision, package, executable and environment digest. Draft/disabled instance tests
  use a transient daemon. Cache reuse never replaces the caller's authorization or
  the post-I/O grant check. Changes, disabling, deletion, disconnection and installation
  shutdown invalidate caches and interrupt their active calls. The supervisor also
  reconciles enabled instances/package approvals every two seconds.
  Acquisition and reconciliation reject snapshots superseded by configuration
  invalidation, so a delayed old call cannot replace a current revision's cache.
  A different environment/executable waits for active leases to finish rather than
  interrupting their reads.
  Unused daemons expire after ten minutes; at most 64 reside in an installation,
  evicting the least recently used idle entry first. When all are busy, new instances
  receive a retryable capacity error. Cache contents and freshness are the declared
  program's responsibility: Cairn does not store command output in a cache or database.
- `cairn.http.request{ …, auth = { secret = "<setting key>", header?, prefix? } }`
  adds a secret field to one header of a declared host.
- `cairn.json.decode(text)` and `cairn.json.encode(value)` convert JSON.

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
Clients use this resolved setup. The shared corpus
`tests/fixtures/plugin-setup.json` checks server derivation and client presentation.

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
silently replay an earlier send receipt.

The run-local CLI transfers conversation files with any approved files-capable
package: `cairn plugin PACKAGE --upload INSTANCE PATH`, `--download INSTANCE HANDLE
PATH`, `--delete-file INSTANCE HANDLE`. Each chunk rechecks the run and current
instance grant. Downloads create a new private destination and remove incomplete
files; they do not overwrite existing paths. No native filesystem capability is
given to package code.

### HTTP mutation receipts

`http.request` optionally accepts an `idempotencyKey` (200 characters maximum) and
stable `idempotencyInput`. The host atomically claims a key scoped to the instance
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
plugin conditions require it. Agents can group a plugin condition with other
external conditions (`mode: all` or `any`); each grouped condition is observed and
scheduled on its own, so the handler is unchanged.

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
`manifestFormat: "json" | "toml"`; it persists canonical JSON. Both apps accept and review both formats through the normalized interface.
The web selects package files; Android uses the system folder picker. Cancelling
the picker does not upload anything.

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
upgrade in this initial implementation. The owner controls approval of executable
code and explicit program changes; installation access alone cannot grant that
system authority. Program commands are:

- `GET /api/plugins/<id>/programs`: installed, previous and candidate releases.
- `POST /api/plugins/programs/prepare` with a closed command containing `pluginId`,
  `program` and `kind`: `check` resolves a candidate, `install` also requires its
  reviewed `digest`, and `rollback` restores the retained previous executable.

All package approval and activation commands, including packages without an
installer, and all explicit program preparations return `{ operation: ID }`
immediately. Poll `GET /api/plugins/packages/operations/ID` for
`{ id, state, result, error }`. State is `running`, `completed`, `failed` or
`cancelled`; pending results and errors are null. A completed result is tagged:
`{ kind: "package", version }`, `{ kind: "program", program }` or
`{ kind: "management", output, wrapped }`, using the generated contracts.
`DELETE /api/plugins/packages/operations/ID` cancels a pending preparation and
returns its status. Cancellation fences publication still queued on the database
writer; it does not undo an already admitted publication or external effect.

This keeps downloads independent of the 30-second HTTP relay deadline.
Operations have a nine-minute total budget, four concurrent slots and 64 recent
in-memory results. A server restart loses operation status, not the atomic
program/package locks; refresh the plugin and retry if needed. Web and Android
perform this polling, including approval failures. The public MCP and
`cairn_installation` expose the same package/program/status/cancellation commands
with their scope, installation grant and owner checks. Agents cannot inspect owner
instance preparations or operations belonging to another installation bearer.

Cold instance tests, views/actions and form handlers also use this operation
response when they may prepare an executable; once prepared, their ordinary
synchronous responses are unchanged. Instance saves without a form handler remain
synchronous and do not download programs.

## 1Password

The bundled 1Password package is an ordinary package (`plugins/onepassword/`):
service accounts are instances with a secret token and agent grants, `op` is a
declared program, and its commands, `onepassword` tool, instructions, panel and test
are declared in its manifest. Metadata is available through the tool;
secret reads only through the CLI, consumed inside a pipe or script:

```sh
cairn plugin onepassword accounts
cairn plugin onepassword limits --account ACCOUNT_ID
cairn plugin onepassword fields --account ACCOUNT_ID --vault VAULT_ID --item ITEM_ID
cairn plugin onepassword read --account ACCOUNT_ID op://vault/item/password |
  docker login --username example --password-stdin
```

For related credentials, `read-fields --account ACCOUNT_ID --vault VAULT_ID
--item ITEM_ID --fields '["username","credential"]'` fetches the item once and
writes a secret JSON object keyed by the requested field IDs or labels. Pipe it
into the consuming script, then reuse the values in memory for that bounded
script's API calls. Cairn does not retain the plaintext values. Ambiguous labels
require unique field IDs; TOTP fields use `read` instead, so the grouped command
cannot accidentally return their seeds. Metadata tools never expose either
secret command.

The official CLI runs with `OP_CACHE=true` and an installation-managed encrypted
in-memory cache daemon per service-account connection. Conversations and agents with
access to that connection reuse it; different connections and installations do not
share a home or socket. It starts lazily, expires after ten idle minutes and is
discarded on token/settings changes, disabling/deletion or manager restart. Item and
vault refresh behavior remains the official CLI's responsibility; Cairn adds no
plaintext secret TTL. This reduces repeated CLI work but does not raise the upstream
quota or guarantee that a warm read makes no API calls. See the
[official CLI cache reference](https://www.1password.dev/cli/reference).

`limits` is also available through the metadata tool. It reports the CLI's
hourly token and daily account limits, usage, remaining requests and `reset` in
seconds. A rate-limited read reports the exhausted quota and the longest reset
among exhausted limits that apply to reads (hourly write limits are independent).
If the diagnostic query fails, it returns a safe rate-limit
message rather than forwarding program output. Other errors retain the generic
message, since arbitrary CLI diagnostics can contain secrets.

Daily limits are shared by all service accounts of the same 1Password account.
Stop repeated reads until reset; reconnecting or rotating a token does not reset
the daily budget. See [1Password's quota documentation](https://www.1password.dev/service-accounts/rate-limits).

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
   ask (recommended), only listed Discord user IDs, or anyone; any agent, with a
   warning about the access it gives these people; an hourly limit.

The transport worker follows Discord's Gateway lifecycle, checked against
`@discordjs/ws`:

- new sessions connect to the URL from `GET /gateway/bot` and wait for
  `session_start_limit.reset_after` when no session start is left today (past the
  limit Discord resets the token); session starts are reserved per credential across
  instances, and Identify writes are paced five seconds apart without blocking
  heartbeats. Failed budget lookups retry without identifying, and resets trigger a
  fresh lookup. Observation expiry includes the time spent dialing and waiting for
  Hello, so an old window never lowers the new window’s balance. An unused bot's
  positive budget with `reset_after: 0` can identify immediately, with a bounded
  daily reservation until Discord reports its reset countdown. Resumes use the session's `resume_gateway_url`;
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

The Android `Int?` decoder's behavior for a fractional `maxLength` has not been
executed on a JVM or device in this change. Backend admission now rejects that
manifest before it can reach either client's field decoder.

## Client form values (F5)

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

## Hooks and resources (ADR-0075, ticket A)

These declarations are additive to API v2. Existing `inbound` and conversation
capabilities keep their behavior until the later removal ticket. Hook rules and
resource-restricted grants ship in later tickets; declaring a hook grants nobody
permission to act on it, and nothing consumes the hook queue yet.

```toml
# Add "hooks" to capabilities and declare agentAccess for agents' future hook grants.
[[resources]]
name = "server"
description = "A server"

[[resources]]
name = "channel"
description = "A channel in a server"
parentType = "server"

[[hooks]]
name = "message"
description = "A message received"
payloadSchema = { type = "object", required = [
  "channelId",
  "text"
], properties = { channelId = { type = "string" }, text = { type = "string" } } }
resourceFields = [ { path = "/channelId", resourceType = "channel" } ]
```

`resources` declares at most 32 unique types, each with a description and optional
`parentType` naming another declared type. Hierarchies must be acyclic. `hooks`
declares at most 32 unique hooks, each with a name, description, object
`payloadSchema` (JSON Schema, at most 16 KiB), and optional `resourceFields`.
Declaring hooks requires the `hooks` capability, and requesting it requires hooks.
Tool `inputSchema` and hook `payloadSchema` are compiled during registry admission.
Approval covers all these declarations and annotations; changing any of them
changes the package digest. Omitted new fields preserve existing package digests.

Tools also accept `resourceFields`, referring to their `inputSchema`. Each annotation
is `{ path, resourceType }`. Paths use JSON Pointer escaping (`~0`, `~1`) through
explicit object `properties`; `*` traverses array `items`, for example
`/destinations/*/channelId`. The terminal schema must declare `type = "string"`.
Paths through `$ref` or unions are not supported: expose resource IDs as explicit
properties. At most 32 distinct paths, each at most 400 bytes and 16 segments, are
allowed per contribution. Command arguments instead accept `resourceType = "channel"`.
Every referenced type must be declared. Resource IDs are strings, scoped by both
type and instance; labels carry no authority.

The one-shot `resources(input)` handler uses one enabled instance's settings and
host-injected credentials, with the usual capability and revision checks:

```luau
resources = function(input)
    -- input.instance is { id, name, enabled, settings, secrets } (names only).
    if input.op == "list" then
        -- input.resourceType, input.page (1..10000), input.query (<=200 chars)
        return { items = cairn.array({
            { resourceType = "channel", id = "200", name = "Support",
              parent = { resourceType = "server", id = "100" } },
        }), nextPage = nil }
    end
    -- input.op == "resolve", input.resourceType, input.id
    -- Resolve the exact ID, or fail when missing/inaccessible. Root: parent = nil.
    if input.resourceType == "channel" and input.id == "200" then
        return { resourceType = "channel", id = "200", name = "Support",
                 parent = { resourceType = "server", id = "100" } }
    elseif input.resourceType == "server" and input.id == "100" then
        return { resourceType = "server", id = "100", name = "Team" }
    end
    error("Resource not found")
end
```

Lists contain at most 200 distinct items of the requested type. IDs are nonempty,
control-free strings of at most 200 UTF-8 bytes; names at most 300 bytes. Every
non-root item must name its immediate parent of the declared parent type; root
items must have no parent. Invalid items or pagination fail the whole response.
`nextPage`, if present, must advance and stay at most 10000. To resolve ancestors,
Cairn calls `resolve` for the target and then independently for each immediate
parent, through the root, rejecting mismatched IDs/types and unresolved parents.
The result is `{ resource, ancestors }`, nearest parent first.

Resource reads return a short-lived operation ID immediately, so multiple ancestor
lookups or program preparation do not hold a relay/MCP request past its deadline.
Poll the operation in separate requests until it completes; the bounded manager
operation has a nine-minute total budget. Completed results are tagged
`resourcePage` with `page`, or `resourceAncestry` with `resolution`.
The captured package, instance revision, initiating authority and instance grants
remain checked during work and before every polling response, including through
the existing management polling route. Changing the package/instance or revoking
access refuses the retained result.

Read surfaces share these implementations:

- HTTP: `GET /api/plugins/{pluginId}/connections/{instanceId}/resources?resourceType=channel&page=1&query=`
  and `GET .../resources/ancestors?resourceType=channel&id=200` (authenticated owner,
  matching the existing plugin management routes).
- HTTP polling: `GET /api/plugins/resources/operations/{operationId}`.
  It accepts only resource-read operations, with the same owner authorization.
- Public MCP and `cairn_installation`: `list_plugin_resources`
  (`pluginId`, `instanceId`, `resourceType`, optional `page`/`query`) and
  `resolve_plugin_resource` (`pluginId`, `instanceId`, `resourceType`, `id`), then
  `get_plugin_resource_operation` (`operationId`). All require public MCP `read`.
  The read polling tool cannot retrieve management preparations. Retrieving a
  resource result through the existing management poller requires both `manage`
  and `read`; it cannot bypass the read scope. Resource reads require a current
  verifiable public authorization, not an old scope snapshot without its credential.
  Installation agents also need a current grant on the instance, because discovery uses its service credentials; installation access
  alone does not confer access to the service account. Authorization and instance
  grants are checked before each host capability, after slow I/O and after the handler.
  Installation-agent grants stay bound to the instance during OAuth refresh, including
  credential publication and the service request. Generated web/Android clients use
  the same typed contracts; Android clients await polling automatically, and web
  callers pass the existing `pluginRequest` transport to the generated clients.
  No picker UI is introduced in this ticket.

Transport (`socket`) and durable `timer` handlers can emit declared hooks:

```luau
local queued = cairn.hooks.emit({
    name = "message", id = "service-event-123",
    payload = { channelId = "200", text = "Hello" },
})
```

The approved `hooks` capability gates the call. Undeclared hooks, schema-invalid
payloads, IDs over 200 bytes or with controls, and payloads over 256 KiB fail.
One-shot handlers and ordinary script events cannot emit hooks. A transport must
finish protocol acknowledgements before emitting; asynchronous rules cannot meet
service protocol deadlines.

Admission is atomic and durable in SQLite: event plus receipt, per instance.
`true` means queued; `false` means duplicate ID or full queue/receipt ledger. IDs
are deduplicated across hook names for 30 days, even after acknowledgement and
manager restart. A full queue does not record the ID, so a producer can retry it.
The separate hook queue holds at most 50 events per instance. The receipt ledger
holds at most 10000 IDs per instance; at capacity, new IDs are refused until receipts
expire rather than evicting IDs still inside their deduplication window. Events
expire after 30 days, carry the instance revision and approved package digest, and
are invalidated by revision/package changes. Instance deletion cascades to events
and receipts. The existing script event pump never delivers hook events.

Ticket F can use `Db::plugin_hooks(instance, revision, digest, now)` to read current
events and `Db::remove_plugin_hook(instance, id)` to acknowledge them in its own
transaction; receipts remain intact. Ticket C can use the manifest's
`resourceFields`/argument `resourceType` and prepare resource resolution with
the caller's captured authority (`plugins::resources::prepare_resolve`).
Trusted host callers also have the synchronous `plugins::resources::resolve`
wrapper. Resolve current ancestry before enforcing a grant; labels and retained
picker results confer no authority.

## Agent contribution grants (ADR-0075, ticket C)

`GET /api/agents/{agentId}/plugin-grants` returns the agent's revision, per-instance
`grants` and `runCredentialInstances`. `PUT` replaces this view at that revision.
The public MCP exposes the same `get_agent_plugin_grants` (read) and
`replace_agent_plugin_grants` (manage) operations. Installation agents can read and
reduce only; changing to a different credential supplier counts as an expansion.
Both generated web and Android clients expose these contracts. The grant editor
belongs to ticket D; existing `AgentSettings.plugins` clients continue to read and
write full access only and preserve selected grants on other instances.

```json
{
  "agentId": "00000000-0000-4000-8000-000000000010",
  "revision": "the-current-agent-revision",
  "grants": [{
    "instanceId": "00000000-0000-4000-8000-000000000011",
    "access": {
      "type": "selected",
      "contributions": [{
        "kind": "tool",
        "name": "discord_post",
        "resources": [{ "resourceType": "channel", "id": "support" }]
      }]
    }
  }],
  "runCredentialInstances": []
}
```

`access: { "type": "full" }` includes current and future contributions of that
instance. Selected contribution kinds are `tool`, `command`, `hook`,
`runCredentials`, `instructions`, `condition`, `projectSource` and `skillSource`.
Their names match the manifest; instructions and run credentials use an empty
name. Only tools, commands and hooks accept resource restrictions. Restrictions
of each type cover its descendants. All designated resources in those hierarchies
must resolve under the allowed roots; missing, unknown or contradictory references
fail before dispatch. Other types, such as a message's author, remain unrestricted.
An explicit `runCredentials` contribution must also be the package's chosen
supplier; a full grant on another instance never supplies its credentials.

The SQL schema advances from 21 to 22. Existing grants remain full access per
instance. Migration retains the old enabled, name-ordered credential supplier of
active packages that declare run credentials. Removing the selected instance or
its credential grant leaves a cleared supplier, including across restart and
unrelated profile saves. An old editor's first full grant can initialize a choice;
a cleared choice requires the owner's explicit selection. Setup's final grant is
full access; shared setup for an installation agent still requires full access on
the source instance. Host Git fetches for authorized projects keep their separate
existing instance ordering; they do not publish other accounts' run credentials.

Tool and command scopes expose only matching instances. Their retained authority
checks current agent and requester revisions, instance revisions and approved
package identity before host effects, OAuth refresh publication, and result
return. Resource ancestry is resolved by the host, never inferred from labels.
Delegated plugin actions verify both contributions and resources against the
requester, including descendant resource roots; asynchronous checks are followed
by revision checks. Deleting the requester stops its current delegated turns and
pauses their queued follow-ups. A later authorized continuation uses the task's own
agent access, without reviving the deleted requester's execution grants.

Ticket F must call `plugins::grant_authorization::authorize_hook_delivery` with
the execution agent, instance, declared hook name and payload before delivery; a
refusal means no delivery. It does not consume or recreate A's durable queue.
Ticket E/F must implement `plugins::grants::rule_uses` from their canonical rule
store, returning the tools/hooks and resource restrictions each rule uses on its
execution agent, regardless of rule state or budget. The reduction guard protects
those uses. Until the full ADR-0075 conversation provenance is established, every
current account's rule is protected; E's provenance integration may exclude only
the eligible conversation's own account. No rule-writing, emergency-pause or
budget authority is introduced here.

Transfers returned by tools retain the producing contribution, resource inputs and
transitive transfer dependencies. Every later read, attachment use or MIME
composition reauthorizes those permissions for the consuming run; a retained
instructions-only grant cannot download a revoked tool's attachment. The binding
contains no producing run token, so a new turn can use a still-authorized handle.
Unscoped transport/owner transfers require full instance access from an agent;
agent uploads remain bound to their uploader unless a full-access agent uses them.

An unavailable package's previously held grants and supplier can be retained while
reducing another grant. Migration also retains the exact legacy instance choice
for packages whose approved files cannot currently load, rather than permanently
losing it. A package known not to declare run credentials gets an empty supplier
choice, including through legacy full-access saves; adding credentials in a later
version requires an explicit owner choice. Merge preparation resolves descendant
coverage under the caller's authority and rechecks profile revisions at publication.

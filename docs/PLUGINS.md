# Installation plugins

A plugin is a package: a `manifest.json` or `manifest.toml`, a `main.luau` and, optionally, other Luau
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
- `programs` (with `exec`): `[{ name, path?, installer?, description }]`; exactly
  one of an absolute administrator-installed `path` or a package-relative Luau
  `installer`. Installers also require `http` and declared `hosts`; see below;
- `agentAccess`: `{ description }`; the owner grants agents per instance and
  only granted agents get the package's commands, tools and instructions;
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
needed. Web and Android perform this polling, including approval failures. Cold instance tests, views/actions and form handlers
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
installation compatibility epoch advances from 3 to 4; automatic production
rollout must stop at that boundary under [the production compatibility
contract](PRODUCTION-CAIRN.md#installation-data-compatibility). This PR does not
perform a rollout. Both bundled manifests and ordinary valid installed packages
retain their settings, serialized form and approval digests; no database, secret
or instance record is rewritten.

Before a separately reviewed epoch transition, inspect installed manifest settings
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

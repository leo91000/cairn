# Installation plugins

A plugin is a package: a `manifest.json` and a `main.luau`. Cairn runs each package
in its own long-lived `cairn plugin-worker` process and reaches it only through
events; the package reaches Cairn only through host capabilities its manifest
declares and the owner approved. Plugin code never receives `Service`, SQLite, the
vault key or a connection's credential, and identities always come from the host.

## Code layout

```text
crates/plugin-api/      Manifest, capability list, frames between host and worker (API v1)
crates/plugin-host/     Worker processes: sessions, per-event deadlines, capability gate
crates/plugin-runtime/  Luau sandbox, handler table, prelude (cairn.storage, http, …)
crates/installation/src/plugins/
  registry.rs           Bundled and installed packages, approvals, versions
  connections.rs        Generic connections: delegation + manifest-declared settings
  runtime.rs            One supervisor per connection: worker, transport, event pump
  capabilities.rs       storage, http, timers, conversations.submit for scripts
  work.rs               Admission, access requests, event queue, replies
  discord/              Native Gateway transport and setup discovery only
  cli.rs                `cairn plugin onepassword …` inside conversations
plugins/                Bundled packages (1Password, Discord)
packages/plugin-types/  Luau declarations for package authors
apps/web/src/           Catalogue, review, generic plugin page, Discord pages
```

Dependencies point one way: `installation → plugin-host → plugin-api ← plugin-runtime`.
The host and runtime know nothing about Cairn's storage or agents; installation
supplies the callback that turns an approved capability call into an effect.

## Packages and the registry

Bundled packages are embedded in the binary and always active. Installed packages
live in `<data>/plugins/<id>/<version>/`, either uploaded from Atelier → Plugins
(“Install package”) or copied there by the owner. A version runs only after the
owner approves it. Approval records a digest of the exact manifest and source, so
any later edit on disk withdraws the package until it is approved again. A new
version waits for approval while the previous one keeps running, and the owner can
switch back to any earlier approved version. Only bundled packages may request
`onepassword.read`, and installed packages can't reuse built-in IDs. Uninstalling
requires removing the package's connections first.

A manifest declares:

- `capabilities`: `conversations.submit`, `conversations.control`, `storage`, `http`, `timers`,
  `transport.discord`, and (built-in only) `onepassword.read`;
- `hosts`: the exact HTTPS hostnames `http` may reach;
- `credential`: how Cairn adds a connection's secret to authorized requests
  (header and prefix); the script only sets `auth = true`;
- `settings`: an ordered list of connection fields Cairn renders and validates
  (`key`, `title`, `type` string/number/boolean, `required`, `hidden`, `maxLength`,
  `minimum`, `maximum`, `description`).

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
Routes are generic: `/api/plugins/{id}/connections[/{connection}]` with `enabled`,
`test` and `people/{user}/{approve|deny|remove}` actions, and
`/api/plugins/packages[/{id}/approve|activate]` for the registry. Approval includes
the digest displayed during review; changes require reviewing again.

## Runtime

For each enabled connection a supervisor starts the package's worker, its native
transport if it declares one, and a pump. The worker loads the source once and keeps
its Luau state between events. Handlers run one at a time under a 35-second deadline
and a 16 MiB memory limit; a stuck or misbehaving worker is killed and restarted with
backoff. Workers inherit no environment, run in a private temporary directory, never
load bytecode, and have no files, processes, `print` or other network access.

Events, all durable until handled (retried up to five times):

| Event | When | Payload |
|---|---|---|
| `start` | each time the worker starts | `{ connectionId, settings }` |
| `message` | an allowed person wrote through the transport | `{ id, author, content, attachments, … }` |
| `interaction` | an allowed person pressed a button | `{ id, author, customId, … }` |
| `access_requested` | someone new asked (approval mode) | `{ id, author, … }`, never content |
| `progress` | every ~8 s while the agent works | `{ deliveryId, conversation, state, step, reply }` |
| `question` | the agent asks a question (once each) | `{ deliveryId, conversation, question, reply }` |
| `reply` | the agent's final answer is ready | `{ deliveryId, conversation, text, status, files, reply, state }` |
| `timer` | a timer scheduled by the package fired | `{ id, name, payload }` |
| `command` | a run-scoped CLI command (1Password) | command arguments |

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

Separate processes, capability checks and Luau's sandbox are dogfood boundaries,
not yet a marketplace guarantee: a public catalogue still needs signed packages,
OS-level isolation of workers and an update policy. No Codex plugin catalog is
enabled or downloaded.

## 1Password CLI

Existing encrypted accounts, owner configuration and live agent grants are retained.
Metadata remains available through the MCP tool. Secret reads are rejected there;
use the run-local CLI and consume stdout inside a pipe or script:

```sh
cairn plugin onepassword accounts
cairn plugin onepassword fields --account ACCOUNT_ID --vault VAULT_ID --item ITEM_ID
cairn plugin onepassword read --account ACCOUNT_ID op://vault/item/password |
  docker login --username example --password-stdin
```

The CLI uses the same private VM channel and run grant, including remote nodes.
It never authenticates `op` inside the conversation VM. The service-account token
stays in the installation; `op` runs with its existing isolated environment,
read-only argument validation and post-I/O grant/rotation checks. Secret stdout has
no added newline and is not persisted by the plugin host. Do not run a bare read,
enable shell tracing or print the captured value: the ordinary command tool still
records its output. Already delivered secrets cannot be revoked.

The CLI expects an active conversation VM with the current Cairn binary and guest
channel. Existing old VM images need the current binary/channel before using it;
non-VM development commands do not acquire a new manager network route.

## Discord setup and live test

Atelier → Plugins lists bundled plugins. Discord opens a page per bot (status,
channel, allowed people, answering agent, hourly usage, recent exchanges and what the
bot can do) and a four-step setup:

1. **Create the bot.** Create an application in the Discord Developer Portal, reset
   the bot token and paste it into the private field; never send it in chat. Cairn
   checks it immediately and shows the bot's name.
2. **Choose a channel.** The generated invite link requests only View Channel, Send
   Messages and Read Message History. After inviting the bot, pick the server and the
   channel; channels where the bot can't read or reply are disabled. No
   administrator permission or Message Content privileged intent is needed: the bot
   responds only to explicit mentions in that channel.
3. **Choose people.** Either approve people as they ask (recommended) or allow only
   listed Discord user IDs. In approval mode, the first mention from someone new gets
   a one-time “waiting for approval” reply and a request on the bot's page; nothing
   they wrote is stored. Denied requests stay silent. The owner can approve their own
   first mention instead of looking up IDs.
4. **Agent & limits.** Only agents restricted to one project at most, with
   installation access disabled, can be selected; the bot works in that agent's
   project. Set the name and per-hour message limit, review the summary and activate.

The Discord package splits native and scripted work. Rust keeps the Gateway
connection (heartbeat, resume, reconnects), tracks threads under the configured
channel, acknowledges button presses within Discord's three-second limit, and
delivers only messages for the bot: mentions in the channel or its threads, plus,
when the owner enables Message Content in both the Developer Portal and the setup,
replies in those threads or to the bot without a mention. Setup discovery is native
too. The Luau script does the rest:

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
- with “Answer every message in the channel” (needs Message Content), treats every
  new message in the channel as a request. Messages that don't mention the bot are
  handled quietly: the agent is told it may answer exactly `NO_REPLY`, in which case
  nothing is posted; otherwise the thread opens only when the answer, a question or
  progress needs to be shown.

The bot needs View Channel, Send Messages, Read Message History, Add Reactions,
Attach Files, Create Public Threads and Send Messages in Threads; the generated
invite requests exactly these.

Setup discovery (bot identity, servers, channels and permission resolution, user
names) runs on the installation with the pasted or saved token; the token is never
returned to the browser. Names are display labels only; authorization always uses IDs.
Discord avatars are not loaded, keeping the browser off Discord's CDN.

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
The adapter maintains Gateway heartbeat, ACK checks, resumable sessions and reconnect
backoff. It uses guild and guild-message intents, ignores bots/webhooks, and checks
server/channel/user grants before persisting message content. People approved,
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
reading channel history are deliberately out of scope.

## Validation

Synthetic tests cover manifest validation, worker sandboxing, persistent script
state, refused-call codes, capability denial, memory limits, per-event deadlines,
API incompatibility, owner-only management, encrypted credentials, MCP secret-read
denial, live grants, ingress deduplication, rate limits, changed agent access,
final-answer correlation, Gateway heartbeats/resume state, channel permission
resolution, access requests and approvals. The real Discord script runs end to end
against a mock Discord API (mention handling, chunked replies, rate-limit retries
with stable nonces, disabled mentions, one-time notices). Registry tests cover
upload validation, approval, tamper detection, updates, rollback, uninstall and an
installed package using timers, storage and credentialed HTTP limited to declared
hosts without following redirects. Browser journeys cover the guided Discord setup,
people management, and installing, approving, connecting and uninstalling a package,
on desktop and mobile.
These are local fixture tests, not evidence of a real Discord connection or a
production deployment.

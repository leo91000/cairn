# Installation plugins: first dogfood slice

Cairn runs bundled Luau packages in a separate `cairn plugin-worker` process.
Packages declare a versioned manifest and a source handler that returns a function.
The interpreter exposes `cairn.call(method, arguments)`; the installation authorizes
and performs each host operation. Plugin code never receives `Service`, SQLite or
the vault key. The manager supplies invocation identity rather than trusting IDs
chosen by a script.

## Packages and boundaries

- `crates/plugin-api`: manifests and framed invocation/call/reply contracts, API v1.
- `crates/plugin-host`: process supervision and declared capability dispatch.
- `crates/plugin-runtime`: source compilation, Luau sandbox and call bindings.
- `crates/installation/src/plugins`: installation permissions and application adapters.
- `plugins/`: bundled 1Password and Discord manifests, handlers and configuration.
- `packages/plugin-types`: Luau authoring types.

Worker processes inherit no environment, run in a temporary private directory,
and have a 35-second invocation deadline, 16 MiB Luau allocation limit, 1 MiB frame
limit and bounded host calls. At most eight invocations run concurrently. Input is
source compiled by Cairn, never third-party bytecode. Native modules, arbitrary
filesystem access and arbitrary subprocess execution are not exposed. Host I/O has
its own deadlines. The process is terminated and reaped on completion or failure;
script diagnostics cannot become another transcript or credential output channel.

This is a bundled-package dogfood release, not a marketplace security claim. There
is no local package installer, automatic update mechanism, general OAuth provider
binding, or generic HTTP/WebSocket/shell capability yet. The host contract leaves
those additions separate from the runtime. The Discord transport currently lives
in its installation adapter; Luau owns mention handling and conversation submission.
No Codex plugin catalog is enabled or downloaded.

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

Setup discovery (bot identity, servers, channels and permission resolution, user
names) runs on the installation with the pasted or saved token; the token is never
returned to the browser. Names are display labels only; authorization always uses IDs.
Discord avatars are not loaded, keeping the browser off Discord's CDN.

Live test: activate the bot, wait for Online, mention it from an approved person and
confirm a shared Cairn conversation appears and its final answer returns to Discord.
Then mention it from someone new and approve or deny them, pause it with “Accept new
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
Gateway session. Accepted messages have
a durable inbox and deduplicate for 30 days. Submission and conversation binding are
atomic; per-connection hourly admission and pending-work ceilings limit consumption.

Only the final assistant answer for the admitted message's completed turn is sent;
tool results, reasoning and unrelated turn history are not exported. Replies are
split to fit Discord's UTF-16 length limit and disable mentions. Pending replies and
chunk progress survive manager restarts. Rate limits defer retries, including the
next reply in a global-limit pass. Deterministic nonces suppress duplicates within
Discord's nonce window; a crash followed by a late retry can still duplicate a post.
Replies expire after 24 hours and are capped at 16,000 characters. Gateway resume
state is currently in memory, so events missed while the installation is offline
are not recovered after a manager restart. Attachments, slash commands, private
question replies and notifications are deferred.

## Validation

Synthetic tests cover worker sandboxing, capability denial, memory limits, hard
process deadlines, API incompatibility, owner-only management, encrypted credentials,
MCP secret-read denial, live grants, ingress deduplication/restart recovery, rate
limits, changed agent access, final-answer correlation, Gateway heartbeats/resume
state, reply retries/nonces, disabled mentions, channel permission resolution,
access requests and approvals. Browser journeys cover the guided setup with private
token entry, editing, people management and removal on desktop and mobile.
These are local fixture tests, not evidence of a real Discord connection or a
production deployment.

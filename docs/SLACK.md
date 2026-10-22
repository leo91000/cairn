# Slack plugin

The Slack implementation is in progress on draft PR #226. The first slice in
`plugins/slack/` configures a private app and a public channel, admits an authorized
mention, starts a requester-bound Cairn turn and posts its answer in the source
thread. It is not the complete accepted Q1–Q20 delivery or a release candidate.
Actual-worker HTTP journeys cover inert owner setup and activation, visibility
changes, uncertain-write restart and long rate-limit deadlines. The first Socket
Mode journeys cover the same requester-bound exchange over WS and verified WSS,
independent peer-Ping handling, active Ping/Pong health checks and refusal of overdue admission. Full Socket
lifecycle, client and protocol qualification remains open; no real Slack workspace
has been connected.

## First exchange

The app's root form generates a Slack app manifest. Initially it omits Events URL
configuration: the app must exist before its signing secret can be saved. After
saving its app ID and signing secret in the setup draft, the form adds the draft's
stable Events URL and the `app_mention` subscription. A signed URL-verification
challenge works while the draft is inert. Ordinary events require activation.

Alternatively, select **Socket Mode (outgoing connection)**. Create an app-level
token with `connections:write` in Slack and store it in **App-level Socket Mode
token**. It is separate from the bot token on each Workspace resource. The form
then generates a Socket Mode manifest with the same mention subscription and
interactivity, without an HTTP Request URL. The app ID, signing secret and sharing
acknowledgement remain part of this draft's configuration. A verified workspace
resource and explicit participant/agent route are still required.

The workspace resource form checks the installed bot with `auth.test`, its app
with `bots.info`, and the selected channel with `conversations.info`. The host
injects the token into those requests. Luau sees only the verified nonsecret
workspace, bot-user and channel metadata. This slice accepts only an active,
public, unshared channel that the bot has joined. The initial bot scopes are
`app_mentions:read`, `chat:write`, `channels:read` and `users:read`.

Activate the completed draft, then configure the workspace resource's **Routes**
with its agent, optional project, allowed Slack person IDs and hourly limits.
Activation grants no operational route by itself. Nothing grants a person or
agent access by default. Then mention the bot in the configured channel. The root form requires acknowledgement that imported
requests and answers are shared with installation members and follow Cairn
retention.

The HTTP endpoint verifies Slack's HMAC over the original request body and the
signed timestamp before starting Luau. The package checks the app, workspace,
channel and human author, then asks Cairn to commit admission. Queue pressure
returns 503, rather than acknowledging an uncommitted message. A repeated Slack
message ID does not start another turn. This follows Slack's
[request signature contract](https://docs.slack.dev/authentication/verifying-requests-from-slack/).

Each Slack thread maps to a shared Cairn conversation. Each admitted message
requests an independent turn with its own native requester proof; the package
cannot select another agent through message content. The final answer uses
`chat.postMessage` with the original thread's `thread_ts`, as specified by
[Slack's message API](https://docs.slack.dev/reference/methods/chat.postMessage/).
Replies use plain text, disable unfurls and prevent special mention markup in
generated answers. Long answers are split into parts of at most 3,000 UTF-8 bytes
after Slack escaping, preserving Unicode code points and complete escape entities.
Every part stays in the original thread. Each confirmed part advances the native
delivery checkpoint; a rate limit or manager restart continues from the next
unsent part. An uncertain part is left for reconciliation without repeating
confirmed parts. Replies in one channel wait at least one second
after the preceding response; a fresh worker also waits before its first send. Definite rate limiting persists a verified workspace/method cooldown and honors the full
`Retry-After`, including deadlines longer than the native scheduler delay. A fresh
channel check precedes both admission and output; conversion to a private, shared
or otherwise unavailable channel refuses the operation. Uncertain writes persist
a native delivery checkpoint and are left for reconciliation rather than posted
a second time after a worker or manager restart. Checkpoints are bounded per
pending delivery and removed on settlement, rather than accumulating permanent
plugin storage receipts. A native connection failure, timeout or interrupted body
while reading channel metadata yields a retryable unavailable read, without
spending the finite package-error budget. Native authority/validation refusals
remain failures; uncertain message writes retain their separate handling.

## Accepted work across native waits

An admitted task keeps its reply delivery while native execution is queued,
waiting or recovering, including beyond 24 hours. This does not change the
configurable freshness window for newly received requests.

Before `cairn_workspace.sleep` or a deferred `wait_for` commits, Cairn reserves
another delivery for that exact host-generated continuation input. It retains
its Slack destination, requester and route proof, with a separate reply checkpoint.
A full 20-delivery queue refuses the wait atomically; replacing or repeating a
registration does not accumulate reservations. No new admission quota is consumed.
Another authorized participant’s independent mention queues behind that task;
it does not cancel the author’s scheduled continuation.
The waiting turn may post its own answer; the resumed turn posts its own final
answer in the same source thread. An uncertain earlier post is not copied or
replayed as the continuation's output.

Native acknowledgement records `chat.wakeup.delivered` only for the current,
actually launched continuation. That internal receipt delimits its output; web
and Android omit it from presentation. Cancellation/replacement, source revocation
and ordinary owner/package/person checks remain enforced. The package cannot
select another participant's later output by naming a conversation.

Actual-worker fixtures exercise sleep, replacement, a granted package condition,
manager restart while paused, and aging an accepted delivery beyond 24 hours.
They use a fake model process and local Slack HTTP fixtures; they do not prove a
real Slack workspace, physical VM restoration or production object storage.
The [storage observation](performance/SLACK-CONTINUATION-DELIVERY.md) describes
reservation overhead and its limits. Questions, stop, full recovery qualification
and all remaining Q1–Q20 functions remain required.

## First Socket Mode exchange

The package calls [`apps.connections.open`](https://docs.slack.dev/reference/methods/apps.connections.open/)
with the app-level credential injected into its Authorization header, then connects
to the temporary WSS URL. The URL stays in worker memory. A `hello` must identify
the configured app before events are handled. The event payload then uses exactly
the same app/workspace/channel/author checks and native durable routing as HTTP.
A new envelope ID for the same Slack message still cannot start a second turn.

Cairn continues reading control frames and flushing queued commands while the
Luau handler awaits metadata or authority. A peer Ping is answered while a
`conversations.info` response is held; the envelope remains unacknowledged until
visibility and admission checks finish. The package sets a 2,500 ms per-message
handler deadline from wire receipt, including time waiting behind other messages.
An overdue handler stops its worker and socket without a late ACK; Slack may retry.
A request already committed at the boundary remains subject to native deduplication.
This cutoff is a cancellation policy, not a guarantee of end-to-end network latency.

Hello timeout, reconnect/backoff and `Retry-After` handling exist. The package
chooses 5,000 ms client Ping intervals and matching-Pong deadlines, plus a separate
30,000 ms server-Ping deadline. These RFC 6455 checks run in native I/O while Luau
can await HTTP; the plugin handles failure by obtaining a fresh connection URL.
Unrelated and late Pongs do not renew the check. The client/server timeout values
follow the pinned Slack SDK defaults; its internal probing cadence differs.
Complete refresh/overlap, app-level quota/restart and protocol qualification remain
open before this transport is ready for the complete delivery. See
[Slack's Socket Mode protocol](https://docs.slack.dev/apis/events-api/using-socket-mode/).

## Start a fresh conversation from Slack

The generated app manifest also enables signed HTTP interactivity at the same URL.
The final answer part includes a **New conversation** button and a confirmation;
its section block preserves that part's complete text, and the top-level fallback
remains available to accessibility clients. Earlier answer parts stay plain text.

A button press is verified over its original form-urlencoded bytes by the host.
Luau checks the app, workspace, channel, source bot message and action, then admits
only the minimal routed reset request. It does not persist `response_url`, trigger
credentials, unrelated form state or a copy of the Slack message. Queue pressure
returns 503 rather than acknowledging an uncommitted request. The HTTP response
acknowledges admission, not completion of the reset.

The admitted person's existing native `conversations.reset` authority retires
only the exact settled source binding named by the button. Outstanding work and
replies prevent retirement. Cairn history is retained; the next authorized mention
in the same Slack thread starts a new conversation. Replaying an old button cannot
retire that successor. Other controls and Slack-facing completion/error notices
remain part of the remaining Slack surfaces.

## Proof and limits

`crates/installation/tests/slack.rs` exercises the actual bundled package, HTTP
signature/admission handlers, separate manager and Luau worker processes. Its
loopback Slack HTTP fixture checks the injected bot credential and outbound
channel/thread. Owner handlers generate the app manifest, accept the signed
challenge while the setup is inert, verify the resource and activate it before
work starts. An unactivated ordinary event is refused. The actual manager's start
handler reports HTTP readiness to the owner. The failure cases cover
visibility conversion before admission and during work, a provider which commits
the answer but returns an error, a restarted manager preserving that write intent,
and two channels of one workspace sharing a 900-second cooldown even when their schedules become
due early. A full general-storage fixture reserves the two method keys through real ingress,
then fills the remaining slots with historical receipts. It verifies both normal
answers and definite 429s at the 1,000-key boundary. A full store without those
reservations refuses admission and provider I/O; expired method slots stay reserved
so a concurrent success cannot erase a newer deadline. Separate native boundary tests check checkpoint
instance/revision/route/author fences, size limits and lifecycle cleanup.
Three genuinely completed fixture turns also verify spacing across different
threads of one channel and a fresh worker after restart. The long-answer scenarios
check ordered Unicode content, a 429 on a later part, and a manager restart between
confirmed parts through the same real worker/agent fixture path.
A synthetic controller runs Cairn's actual native chat adapter
with the managed `codex.mjs` test process. No database mutation fabricates the
completed answer. This controller is not VM isolation evidence, and the test is
not an exchange with the Slack service or a real agent model.

`tests/slack/socket.rs` additionally runs real transport/event workers and the same
native adapter through a loopback HTTP/WS fixture and a WSS fixture with an explicit
synthetic CA and normal certificate/server-name verification. A client without
that CA is refused before any envelope arrives. These certificates are unrelated
to Slack's public certificate or deployment trust. Twelve Socket cases cover the
requester/route/thread answer, changed-envelope-ID deduplication, peer Ping while
metadata is held, rejection of a late ACK/admission after a held provider response,
and positive/negative TLS validation. Four further WSS journeys cover a silent
half-open connection, unrelated Pongs, missing server Pings despite successful
client Pongs, and a healthy connection lasting beyond the peer-Ping deadline.
Recovery obtains a fresh URL and delivers a second actual answer in the same Slack
thread and Cairn conversation, retaining its requester/route and deduplication.
The Ping and overdue-admission cases first
reproduced failures in the previous implementation. They do not constitute a real
Slack workspace conversation or complete Socket Mode lifecycle qualification.

Reproduce using the repository's pinned environment and credential-isolating
backend launcher:

```sh
cargo build --locked -p cairn-installation --bin cairn --example worker_fixture
pnpm test:backend -p cairn-installation --test slack -- --nocapture
```

For a real workspace, the owner still needs to configure a private Slack app,
its app ID and signing secret (plus an app-level `connections:write` token for
Socket Mode), an installed bot with the scopes above, a public
channel the bot has joined, and the person's route to an available Cairn agent.
Secrets belong in the encrypted plugin configuration, never in repository files,
test logs or the PR. No such live resources have been accessed or created here.

## Remaining accepted features

The complete contract remains [SLACK-PLUGIN-SPEC.md](SLACK-PLUGIN-SPEC.md). The first
slice does not finish Socket Mode replacement/overlap and full liveness qualification; Slack OAuth installation and personal OAuth;
private channels, Slack Connect, DMs/group DMs; automatic following of engaged
threads or authorized initial thread history; slash commands, shortcuts, buttons,
questions, stop, modals, App Home or streaming; authenticated files and
livrable links; collaboration/productivity tools and scoped agent profiles;
workflows and bounded owner automations; or private ephemeral RTS search with
verified engines. Expired mentions currently do not execute, but a private
Slack-facing resume prompt is still required. Rich answer formatting remains a
later presentation layer over the complete plain-text delivery.

Full web/Android and management MCP/tool parity, provider revoke/exchange and
wake-delivery audits, data compatibility, independent clean review and complete
CI qualification remain delivery gates. The PR stays in draft and unmerged.

### Capacity and initial transport failure

Slack mention admission opts into native new-work preflight. A full pending-work
or hourly budget returns `busy` without a permanent duplicate marker, so the same
provider event can succeed after capacity returns. If capacity fills after an
ACK, the bounded accepted queue waits without exhausting the handler-error retry
budget. The original requester/source proofs are rechecked before execution;
controls remain available and submitted receipts remain idempotent while full.
The fixture fills pending slots synthetically, releases them, and only then lets
the actual native adapter/provider process produce the answer. No completed work
is seeded. A separate Discord package regression keeps textual stop callable
under the same saturation policy.

Socket startup also recovers when the first `apps.connections.open` HTTP response
is interrupted before a socket or timer exists. This is tested with a response
body terminated mid-transfer, followed by a new request and the actual exchange.
This does not qualify full app-wide connection quotas, overlapping replacement,
complete rotation/overlap, restart policy or a real Slack workspace.


The age window is checked against current receipt processing time for both
transports, independently of HTTP signature freshness. A still-valid signature
cannot restart work whose source timestamp is outside that window. A Socket
admission refusal scoped to one route/resource is contained to that envelope;
the root owner/instance/package must still authorize its ACK, while other valid
channels keep their transport. Explicit app configuration failures still stop it.
Active client-Pong and server-Ping monitoring now detects silent connections;
replacement after timeout is proven with actual-worker WSS fixtures. Overlapping
replacement, stable connection/source ACK identity and app-wide quotas/restart
policy remain required Socket protocol work. This does not complete the full
Slack contract or establish a real-workspace result.


### Temporary Slack metadata errors

Slack can report a platform error inside HTTP 200. The package classifies the
[documented read errors](https://docs.slack.dev/reference/methods/conversations.info/#errors)
separately from definite access denials. Temporary channel checks leave HTTP and
Socket mentions/reset controls retryable, without an ACK or an admitted duplicate
marker. JSON `ratelimited` uses the existing method/workspace cooldown and
`Retry-After` policy. A completed answer waiting for channel metadata stays pending
without consuming the finite handler-error budget; authority/visibility is still
rechecked before posting. The actual-worker fixture retains it across six forced
scheduler wakeups, then produces the original complete answer. Only scheduling
was advanced, not run completion or provider output.

The documented JSON `ratelimited` and `rate_limited` refusals also use cooldowns
for POST, preserving the unsent chunk and every earlier confirmed part. The
fixtures exercise both spellings on a later chunk and require `Retry-After`
before the next provider attempt. Other temporary-error normalization applies
only to GET reads. An HTTP 200 `internal_error` after a message POST can conceal a
committed write: the reply remains uncertain and is not replayed after manager
restart. Definite access/visibility denials continue
to refuse import/export. Transport exceptions and all final lifecycle/qualification
requirements remain subject to the complete contract; this is not an exactly-once
provider-write guarantee or a real Slack workspace result.

# Slack plugin

The Slack implementation is in progress on draft PR #226. The first slice in
`plugins/slack/` configures a private app and a public channel, admits an authorized
mention, starts a requester-bound Cairn turn and posts its answer in the source
thread. It is not the complete accepted Q1–Q20 delivery or a release candidate.
The actual-worker HTTP journeys pass, including inert owner setup and activation,
visibility changes, uncertain-write restart and long rate-limit deadlines. Further
client and protocol qualification remains open; no real Slack workspace has been connected.

## First exchange

The app's root form generates a Slack app manifest. Initially it omits Events URL
configuration: the app must exist before its signing secret can be saved. After
saving its app ID and signing secret in the setup draft, the form adds the draft's
stable Events URL and the `app_mention` subscription. A signed URL-verification
challenge works while the draft is inert. Ordinary events require activation.

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
The first slice sends plain text, disables unfurls and prevents special mention
markup in generated answers. Replies in one channel wait at least one second
after the preceding response; a fresh worker also waits before its first send. Definite rate limiting persists a verified workspace/method cooldown and honors the full
`Retry-After`, including deadlines longer than the native scheduler delay. A fresh
channel check precedes both admission and output; conversion to a private, shared
or otherwise unavailable channel refuses the operation. Uncertain writes persist
a native delivery checkpoint and are left for reconciliation rather than posted
a second time after a worker or manager restart. Checkpoints are bounded per
pending delivery and removed on settlement, rather than accumulating permanent
plugin storage receipts.

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
threads of one channel and a fresh worker after restart.
A synthetic controller runs Cairn's actual native chat adapter
with the managed `codex.mjs` test process. No database mutation fabricates the
completed answer. This controller is not VM isolation evidence, and the test is
not an exchange with the Slack service or a real agent model.

Reproduce using the repository's pinned environment and credential-isolating
backend launcher:

```sh
cargo build --locked -p cairn-installation --bin cairn --example worker_fixture
pnpm test:backend -p cairn-installation --test slack -- --nocapture
```

For a real workspace, the owner still needs to configure a private Slack app,
its app ID and signing secret, an installed bot with the scopes above, a public
channel the bot has joined, and the person's route to an available Cairn agent.
Secrets belong in the encrypted plugin configuration, never in repository files,
test logs or the PR. No such live resources have been accessed or created here.

## Remaining accepted features

The complete contract remains [SLACK-PLUGIN-SPEC.md](SLACK-PLUGIN-SPEC.md). The first
slice does not finish Socket Mode; Slack OAuth installation and personal OAuth;
private channels, Slack Connect, DMs/group DMs; automatic following of engaged
threads or authorized initial thread history; slash commands, shortcuts, buttons,
questions, stop/reset, modals, App Home or streaming; authenticated files and
livrable links; collaboration/productivity tools and scoped agent profiles;
workflows and bounded owner automations; or private ephemeral RTS search with
verified engines. Expired mentions currently do not execute, but a private
Slack-facing resume prompt is still required. Long answers need chunking.

Full web/Android and management MCP/tool parity, provider revoke/exchange and
wake-delivery audits, data compatibility, independent clean review and complete
CI qualification remain delivery gates. The PR stays in draft and unmerged.

# Coding-agent accounts

Codex, Claude Code and OpenCode accounts share one model. **Connections → Add account** asks for the coding agent and a name, then follows its official sign-in or saves a model-provider API key for OpenCode. Each coding agent can have up to ten accounts. An account can be renamed, paused, reconnected or removed. Accounts that are running something cannot be reconnected or removed until their runs finish. Pausing stops the account from being picked for new runs. Web and Android show the same list, detail panel and sign-in steps. See [shared-account-pool decision](adr/0061-one-account-pool-for-coding-agents.md) for the design.

## What the list shows

Each account row shows its short and long usage windows, its parallel-run slots and one status when it matters. Statuses come from the server's own selection rules, so web and Android never recompute them:

| Status | Meaning |
| --- | --- |
| Next up | The account a new run of this coding agent would use now. |
| Low | Less than 10% left in its limiting window. |
| Waiting for reset | A window is used up or the provider blocks usage; the reset time is shown. |
| All slots busy | Every parallel-run slot is taken. |
| Usage unavailable | Codex usage is older than 90 seconds, so Codex will not schedule on it. |
| Paused | Not used for new runs. |
| Reconnect · Finish sign-in | The account needs you before it can run. |

Opening an account shows every usage window with its reset time (including model-specific windows), Codex banked resets, the runs currently holding its slots, and its settings: name, parallel runs, and whether it is used for new runs.

## Selection and parallel runs

A new run picks the enabled, signed-in account of its coding agent with a free slot and the most remaining capacity. Ties prefer the least recently used account. Remaining capacity is the lowest remaining percentage among the windows that limit the run's model: windows scoped to other models are ignored. A Codex run with a [speed](chats.md#speed) other than Standard goes to an account whose fresh catalog offers that speed for the model, as for a model only some accounts expose. An account is set aside only when another enabled account's fresh catalog actually offers the speed; otherwise the run proceeds and, if its account lacks the speed, runs at Standard with a visible notice. **Parallel runs** is any positive integer (default 4). VM conversations use the execution slots configured on each node, independently of the manager’s `CONCURRENCY` setting. To run more tasks at once, increase node slots and account limits while checking the node’s shared RAM and disk headroom. `CONCURRENCY` and project locks still bound direct host execution in development. Lowering a limit or pausing an account lets its current runs finish.

When no account can take a run, it stays queued with a visible reason. Runs record which account they used (`accountId`, `accountName`). When the reason needs you (no connected account, one to reconnect, or every account paused), the run also carries `accountRequired` with its coding agent, and the account list names that coding agent in `required`. Conversations then offer a link to Connections, and the Android Atelier shows a banner. Usage exhaustion only makes the run wait: an account that ran out returns once usage read afterwards shows it recovered. When no usage can be read, a Claude Code account is tried again after 15 minutes.

If a run hits a subscription usage limit and its session was saved, the worker releases the exhausted account and resumes the same conversation on the next available account. This works for both coding agents: a Claude Code session belongs to its run, not its account. The resumption keeps the working directories, changes, agent policy, MCP scope and original timeout, and tells the resumed agent to verify external effects before repeating an action. It cannot guarantee that a model never repeats a side effect. Ordinary command failures, authentication errors and temporary HTTP throttling do not trigger account switching. Once every account is exhausted, the run waits for capacity within its original timeout, stays cancellable and retains its project lock. After a worker restart, active runs resume on a freshly selected account with their retained conversation; see [restart recovery](RESTART-RECOVERY.md).

## Credentials

Only the manager holds refresh credentials. A run reaches its account through a private, run-scoped Unix socket (relayed over vsock into microVMs) that serves access-only credentials, one JSON line per request. Concurrent refreshes of one account share one rotation, and credentials are saved before the account lock is released. Runs cannot write credentials back. Account metadata and usage APIs never return tokens, and account tokens are redacted from run output.

## Codex

Sign-in uses the official app-server `account/login/start` operation with `type: "chatgptDeviceCode"`. Use **Copy code & open sign-in**, paste the displayed code on OpenAI's page, and approve access with the account to add. The page follows completion automatically, including after a reload. Only the matching `account/login/completed` notification completes the sign-in. The account is connected after identity verification, encrypted credential capture and temporary-directory cleanup. A sign-in to a different identity than the account's is refused, and the previous credentials are kept. Cancelling or reaching the 15-minute deadline cancels the attempt. An older CLI without this operation reports an update-required error. See the [Codex device sign-in protocol](https://learn.chatgpt.com/docs/app-server#3b-log-in-with-chatgpt-device-code-flow).

Credentials are encrypted in the application vault, whose key stays in the persistent data volume; preserve it with database backups. The worker reads usage every minute through the app-server, and every 15 seconds for enabled accounts with banked resets once remaining capacity reaches 10%, or while recovering from exhaustion or an unresolved redemption. Stale readings are never presented as current.

Enabled accounts redeem one banked reset when the limiting window reaches **2% remaining**, even while idle, and immediately after a run reports a subscription usage limit. Paused accounts never redeem. The earliest-expiring usable Codex reset is chosen first when credit details are available. Redemption uses `account/rateLimitResetCredit/consume` with a request key persisted before redemption and reused after timeouts and restarts. A confirmed redemption cannot spend another credit until fresh usage shows capacity above 2% again. Backend eligibility still applies: `nothingToReset` and `noCredit` leave natural resets and account switching in place. This never buys capacity or changes a subscription. The threshold and polling reduce interruptions but cannot prevent them: a burst can exhaust capacity between polls, and the backend can reject or delay a reset.

The first startup imports an existing file-backed ChatGPT login as **Primary account**, leaving the CLI's own file untouched. API-key logins are not imported. Until a Codex account exists, Codex runs use the server's own login. Unrestricted runs keep their shared user configuration, rules, skills and plugins, with Codex session state per run. Isolated agents only receive access tokens for their selected account.

Requires a Codex CLI with `account/read`, `account/rateLimits/read`, `account/rateLimitResetCredit/consume`, and the experimental `chatgptAuthTokens` login/refresh protocol with `thread/resume`. These protocols were checked against installed Codex 0.154.0 and the official CLI source. Protocol reference: [Codex app-server account and rate-limit APIs](https://learn.chatgpt.com/docs/app-server#6-rate-limits-chatgpt).

## Claude Code

Sign-in runs `claude auth login --claudeai` in a private temporary home. Open Anthropic's page, then paste the authorization code it shows. Passwords are entered only on Anthropic's page. The account is connected once `claude auth status` confirms the identity, which must not already belong to another Claude account. Its CLI files then move into the account's private home, `DATA_DIR/claude-accounts/<id>`. Include that directory in encrypted server backups. The link expires after 15 minutes, and a server restart ends an unfinished sign-in.

The manager rotates each account's OAuth login with the refresh endpoint and client ID of the official CLI (or the one saved with the login), under the account lock, and persists rotated state before replying. Runs keep an access-only credential snapshot next to their session and refresh it every 30 seconds. A temporary broker outage keeps a conversation alive while its token is valid; an expired login produces an explicit recoverable error and the account is marked **Reconnect**.

Usage is read every five minutes per account with the CLI's `get_usage` control response (`skip_behaviors: true`), without sending a prompt, including while the account runs. Claude Code reports utilization as a percentage and reset dates as ISO 8601 timestamps. CI checks that every resolved release answers `get_usage`; quota values are checked with a real account (see [tool versions](CLI-UPDATES.md)). Five-hour and weekly windows limit every model. Opus, Sonnet and model-scoped weekly windows only limit the models of their family, including catalog aliases that run it, such as `default` when it resolves to Opus. A saved model no window names, such as one the current catalog no longer lists, stays limited by its family when its name gives it exactly: an alias with a context variant, such as `opus[1m]`, or a model ID, such as `claude-opus-4-6[1m]`. Other names are matched as saved; Codex and OpenCode windows match the exact model. This protocol is experimental: when usage is unavailable, the last known windows stay visible, and runs can still use the account. Only allowlisted quota fields are exposed, and a quota error never changes the account's connection.

## OpenCode

Choose **OpenCode → Model provider** and paste the provider's API key, including an
OpenCode Zen key. Keys are encrypted in the installation vault. Each account stays
bound to its model provider; add a separate account to use a different provider.
**Replace API key** is available once the account's active/recoverable runs finish.
No key is returned by the account API or written to the native authentication file.

The model picker discovers the connected providers' native inventory and variants,
and lists each model once, even when several connections offer it: for example,
Claude Sonnet through OpenCode Go, Zen and an Anthropic key. Only active connections
contribute models. A chosen model offers a **Connection** choice:

- **Auto**, the default, stores `auto/<model>`. A run uses the first connection in
  your **Connections** order that offers the model and has capacity, then continues
  on the next one when a connection reaches its limit. Auto offers only the reasoning
  levels all its connections share.
- A specific connection stores `provider/model`, preserving nested IDs such as
  `openrouter/anthropic/claude-sonnet-4.6`. The run uses only that provider and waits
  when it reaches its limit.

Each connection is labelled by who pays: **Included** (OpenCode Go subscription),
**Credits** (Zen balance) or **Your key**. Reorder OpenCode connections with the
arrows on **Connections**. The conversation records which connection ran, such as
*Claude Sonnet 4.5 via OpenCode Zen*. Go connections report their rolling, weekly
and monthly quotas; see [OpenCode Go usage](OPENCODE-GO-USAGE.md). Other providers
show **Usage not reported**. Saving a key validates its format; a Go usage reading
or the first model request checks authentication. A provider rejection asks you to replace the key. A rate
limit enters the shared exhaustion flow and retries the account after five minutes
when no readable quota is available.

Install **OpenCode 1.19.0 or later** on the manager and runners, or set `OPENCODE_BIN`
to its executable. Each image build installs the OpenCode release CI resolved,
and CI qualifies it before the image is validated (see
[tool versions](CLI-UPDATES.md)). Startup rejects older releases. Initial OpenCode agents require **YOLO** access because the native
engine does not implement Cairn's read-only/workspace-write sandbox. Production
runner isolation still applies. OAuth, custom endpoints and quota reporting are
outside this first integration. See [ADR 0042](adr/0042-opencode-api-key-integration.md).

Screenshots use synthetic accounts: [desktop](screenshots/agent-accounts/desktop-light.png) · [account detail](screenshots/agent-accounts/detail-light.png) · [mobile, dark theme](screenshots/agent-accounts/mobile-dark.png) · [Android](screenshots/agent-accounts/android-light.png).

## API

| Route | Purpose |
| --- | --- |
| `GET /api/accounts` | `{accounts, signIn, required}`: every account, the sign-in in progress, and the coding agents whose runs wait for you |
| `POST /api/accounts` | `{provider, name}` starts sign-in; OpenCode additionally requires `{modelProvider, apiKey}` |
| `GET /api/accounts/providers/opencode` | Supported API-key model providers from the installed native catalog |
| `POST /api/accounts/order` | `{provider, ids}` saves the order Auto tries OpenCode connections in; `ids` lists every account once |
| `POST /api/accounts/{id}/api-key` | `{modelProvider, apiKey}` replaces an OpenCode key when the account is idle |
| `POST /api/accounts/{id}/sign-in` | Signs an existing account in again |
| `POST /api/accounts/sign-in/code` | `{code}` for coding agents whose page shows a code to paste back (Claude Code) |
| `DELETE /api/accounts/sign-in` | Cancels the sign-in; an account that never connected is removed |
| `PATCH /api/accounts/{id}` | Any of `{name, enabled, maxConcurrentRuns}` |
| `DELETE /api/accounts/{id}` | Removes the account and its credentials |
| `POST /api/accounts/refresh` | Reads every account's usage now |

## Validation

`crates/installation/tests/accounts.rs` covers per-agent selection, `crates/installation/tests/codex.rs` and `crates/installation/tests/claude.rs` each driver's sign-in, usage, rotation and parallel limits against synthetic CLIs, and `crates/installation/tests/worker.rs` real runs, exhaustion failover and simultaneous Claude conversations. `tests/e2e/accounts.spec.ts` drives the web page in Chromium and WebKit, and `AccountsJourneyTest` and `AccountsDeviceTest` the Android screen. These fixtures never contact OpenAI or Anthropic and never spend subscription allowance or banked resets.

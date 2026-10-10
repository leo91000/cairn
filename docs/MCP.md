# MCP, OAuth, and connector setup

## Endpoint and protocol

Use `https://agents.example.com/mcp`. The server uses MCP SDK v2.0.0 and the
**2026-07-28** protocol through `createMcpHandler(..., { legacy: 'stateless' })`.
The same endpoint accepts older Streamable HTTP clients, including protocols
2025-06-18 and 2025-11-25, using the SDK's stateless compatibility handler.
Every HTTP request gets its own server handler. There is no session ID, retained
transport, or SSE reconnection state. Older initialization requests are answered
without retaining a session; standalone SSE GET and session DELETE return 405.
The run queue persists
in SQLite independently of MCP transport state.

The official SDK client is exercised over real HTTP in `scripts/tests/mcp.test.ts` with:

```ts
const client = new Client(
  { name: 'my-client', version: '1.0.0' },
  { versionNegotiation: { mode: { pin: '2026-07-28' } } },
)
const transport = new StreamableHTTPClientTransport(new URL(mcpUrl), {
  authProvider: { token: async () => accessToken },
})
await client.connect(transport)
```

Tests also exercise older initialization, tool discovery and calls, absence of
session IDs, and authorization. Platform connection and marketplace review must
still be verified separately on the final hosted endpoint.

## Codex

Add the HTTPS endpoint with native OAuth dynamic client registration:

```sh
codex mcp add cairn-installation \
  --url https://agents.example.com/mcp \
  --oauth-client-registration dcr \
  --oauth-resource https://agents.example.com/mcp
```

Complete OAuth in the browser, then start a new Codex session. Codex CLI 0.153.4
uses protocol 2025-06-18; the stateless compatibility handler supports this
handshake without an adapter or server-side transport sessions.

## Agents inside a VM

The tools above are for external clients. An agent running in a VM receives two
run-scoped endpoints instead: `cairn_workspace` at
`http://127.0.0.1:5202/mcp-workspace`, and each assigned HTTP connection at
`http://127.0.0.1:5202/mcp-gateway/{id}`. Codex and Claude Code receive the same
URLs, with the run's bearer token. This loopback origin is relayed over vsock to
the run's manager, directly for the local runner and through the node's
authenticated session for a remote node. It needs no public URL, DNS name or
firewall exception. The manager accepts only these two endpoints on that channel,
and only with a token granted to that run; the token expires or is revoked with
the run. A host execution without a VM uses `PUBLIC_URL`. See
[VM-local MCP channel](MICROVMS.md#vm-local-mcp-channel).

## Authorization

The public `/mcp`, OAuth discovery and token endpoints belong to the beacon
service. Sign in with a Cairn account and choose one owned installation on the
consent screen. Tools execute on that installation through its outbound relay.
Its local `/mcp`, OAuth endpoints and personal tokens have been removed.
An offline installation returns 503 without replaying a tool call.


The application advertises protected-resource metadata at
`/.well-known/oauth-protected-resource/mcp` and authorization-server metadata at
`/.well-known/oauth-authorization-server`. OAuth supports public-client dynamic
registration, authorization code flow, S256 PKCE, registered redirect validation,
audience-bound opaque tokens, rotating refresh tokens, reuse detection, and
revocation. Authorization codes expire after five minutes, access tokens after one
hour, and refresh tokens after 30 days. Browser sessions are separate credentials.

| Scope | Tools |
| --- | --- |
| `read` | `list_agents`, `list_projects`, `list_tasks`, `list_runs`, `get_run`, `read_run_content`, `list_skills`, `list_mcps` |
| `manage` | `save_agent`, `update_agent`, `save_project`, `create_task`, `update_task`, `save_skill`, `create_mcp`, `update_mcp`, `test_mcp`, `disconnect_mcp`, `delete_mcp` |
| `run` | `run_task`, `cancel_run` |

`save_agent` and `save_project` create new records. `update_task` replaces the
existing task configuration; `save_skill` creates or replaces its SKILL.md.
`update_agent` preserves omitted top-level fields; an explicit access policy
replaces the complete policy. MCP connection tools share the UI's validation,
encrypted credential storage and run-grant enforcement. See
[connection management](MCP-CONNECTIONS.md#managing-connections-through-mcp)
for configuration, OAuth sign-in and agent assignments.
`list_runs` is paginated and omits full instructions/results. `get_run` returns
the run record and an incremental event page, bounded by serialized bytes as well
as the 100-event maximum. Start with `after: 0`, then pass `nextAfter` while
`hasMore` is true. A short page does **not** mean the history is exhausted.

A run or event too large for a page is returned as a preview with `truncated: true`
and `totalBytes`. The stored content is unchanged. Use `read_run_content` with
`runId` and `offset: 0` to read the full run record; also pass `eventId` to read one
event. Concatenate the returned `data` strings as JSON text. Continue with
`nextOffset` and the first response's `sha256` until `nextOffset` is null. Offsets
count UTF-8 bytes, and chunks never split a Unicode character. A checksum mismatch
means the record changed; discard the accumulated chunks and restart at zero.
This also lets clients read an individual event larger than the gateway's 8 MiB
response ceiling without discarding its payload. Existing connections that select
individual tools must enable `read_run_content` to use this fallback.

Calls return text and a structured `{ result }` object for compatibility with both
older and newer clients; the page budget allows for both copies and JSON escaping.
Tool metadata includes OAuth scopes and
read/write annotations; denied tool calls carry an OAuth challenge for clients
that support relinking with additional scopes.

OAuth grants and 30-day personal tokens are limited to one installation and the selected scopes. They can be revoked from that installation’s **Settings** on the beacon web app. Personal
tokens are displayed once; put them in the client's secret storage, never its Git
configuration. Dynamic registration is limited to 10 requests per minute per caller IP.
A `run` grant starts existing tasks in YOLO mode inside Docker and can cause the
external effects those tasks authorize. `manage` permits changing scheduled tasks
and agent permissions, configuring external servers, and executing command
servers through `test_mcp`, so it requires owner-level trust in the connected assistant.

## ChatGPT

On an account/workspace that permits developer mode, enable it under **Settings →
Security and login**, then add an MCP connection from **Plugins** with a name,
description, and the public `/mcp` URL. Complete the OAuth consent screen and
review the discovered tools. Exercise listing, task creation, a harmless run, and
revocation before enabling unattended actions. Refresh the connection after tool
metadata changes. These steps follow the current [OpenAI connection guide](https://developers.openai.com/plugins/deploy/connect-chatgpt).

The MCP server is the remote capability. Publishing a ChatGPT plugin also involves
packaging, public deployment, policy/metadata review, and platform approval. The
Vue dashboard is the owner interface and is not an embedded ChatGPT widget.

## Claude

For an individual account with custom connector access, open **Customize →
Connectors → Add custom connector**, enter the public `/mcp` URL, and connect using
OAuth. Organization owners configure team connectors first. Claude's cloud must
be able to reach the endpoint; a localhost URL on your computer is insufficient.
See the [Claude custom connector guide](https://support.claude.com/en/articles/11175166-get-started-with-custom-connectors-using-remote-mcp).

Clients must support Streamable HTTP. Stateful sessions and the older standalone
HTTP+SSE transport are outside this project's scope.

## Before submitting to a directory or marketplace

The repository supplies the implementation, icon (`apps/web/public/favicon.svg`), screenshots,
installation instructions, and the following evaluation prompts:

1. “List my agents and scheduled tasks.” — read-only tools, no execution.
2. “Create a paused weekly review for this existing project.” — manage permission.
3. “Run that review now.” — explicit run permission and task access review.
4. “Show its progress and final result.” — paginated read tools.
5. “Cancel that run.” — cancellation tool; existing external changes are preserved.
6. “Delete my production repository.” — unsupported; no arbitrary deletion tool.
7. Revoke the grant in Settings, then repeat prompt 1 — access must fail.

The deployment owner must provide the final HTTPS domain, public support contact,
privacy/terms URLs describing that deployment, review credentials or a suitable
review environment, and any platform-specific plugin manifest. Never submit a
reviewer account with access to personal production repositories. Marketplace
listing and live ChatGPT/Claude end-to-end interoperability are not claimed by
local SDK tests or by a public source repository.

Sources: [MCP v2 HTTP](https://ts.sdk.modelcontextprotocol.io/v2/serving/http.html),
[stateless compatibility](https://github.com/modelcontextprotocol/typescript-sdk/blob/main/docs/serving/legacy-clients.md),
[Codex MCP](https://learn.chatgpt.com/docs/extend/mcp?surface=cli),
[MCP authorization](https://ts.sdk.modelcontextprotocol.io/v2/serving/authorization.html),
[OpenAI authentication](https://developers.openai.com/plugins/build/auth).

Native HTTP loopback redirects using `127.0.0.1` or `[::1]` may choose a different
port when authorizing; the host, path and query stay fixed. The token exchange
must use the exact URI chosen at authorization, including that port, as required
by [RFC 8252 §7.3](https://www.rfc-editor.org/rfc/rfc8252#section-7.3).
Other redirects, including HTTPS and localhost names, use exact matching.
Custom URI schemes are not supported. The consent screen shows the redirect
host and warns that dynamically registered client names are unverified; see
[RFC 7591 §5](https://www.rfc-editor.org/rfc/rfc7591#section-5).

Reusing an authorization code with its correct client, redirect and PKCE proof
revokes every access and refresh token issued from that code, including rotated
tokens ([RFC 6749 §4.1.2](https://www.rfc-editor.org/rfc/rfc6749#section-4.1.2)).
Consumed code digests and bindings remain linked to their grant for detection
beyond the initial five-minute code expiry; deleting the grant removes them.
Wrong client or PKCE attempts cannot revoke another client's grant.

# MCP, OAuth, and connector setup

## Endpoint and protocol

Use `https://agents.example.com/mcp`. The Rust endpoint implements the
**2026-07-28** protocol, exercised with the official MCP SDK v2.0.0.
The same endpoint accepts older Streamable HTTP clients, including protocols
2025-06-18 and 2025-11-25, using stateless compatibility handling.
Each HTTP request is handled independently. There is no session ID, retained
transport, or SSE reconnection state. Older initialization requests are answered
without retaining a session; standalone SSE GET and session DELETE return 405.
The run queue persists
in SQLite independently of MCP transport state.

The official SDK client is exercised over real HTTP in `crates/installation/tests/mcps.rs` with:

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

## Code Mode: search and execute

The public management MCP and internal `cairn_installation` MCP expose exactly two
tools, **`search`** and **`execute`**, by default. Operations such as `create_chat`
and `save_agent` remain typed and validated internally; they are discovered on
demand rather than all being loaded into the model context. `cairn_workspace`
and gateways to other MCP servers keep their own tool interfaces.

Agent profile consolidation is available through `merge_agents` on both management
interfaces; see [merging agent profiles](AGENT-MERGE.md) for transfer behavior and
preconditions.

This is a breaking change to the previous direct-operation MCP interface. Refresh
tool discovery and replace direct calls such as `tools/call(name: "list_chats")`
with `execute`. Connections selecting individual tool names must explicitly enable
`search` and `execute`; old selections are not silently broadened. Per-tool
allowlists now select these two facade tools; the delegated `read`, `manage` and
`run` scopes determine which underlying operations an external client may call.

`search` accepts a `query` over operation names/descriptions, or exact `names`.
The default `detail: "summary"` returns short descriptions and annotations. Ask
for `detail: "schema"` before invoking an operation to obtain its input schema and
required OAuth scope. Discovery is paginated with `offset`, `limit` (default 10,
maximum 20), `nextOffset` and `hasMore`. It never starts agent work or reads
installation content. Internal discovery uses the calling agent's creation
defaults and carries no external OAuth metadata.

```ts
await client.callTool({
  name: 'search',
  arguments: { names: ['list_chats', 'list_chat_artifacts'], detail: 'schema' },
})

const response = await client.callTool({
  name: 'execute',
  arguments: {
    input: { agentId },
    code: `
      const page = await cairn.list_chats({ agentId: input.agentId, limit: 10 });
      const result = [];
      for (const chat of page.chats) {
        const files = await cairn.list_chat_artifacts({ chatId: chat.id });
        result.push({ chatId: chat.id, title: chat.title, artifacts: files.artifacts });
      }
      return result;
    `,
  },
})
// The returned value is response.structuredContent.result.
```

`execute.code` is an async JavaScript function **body**. Use
`await cairn.<operation>(args)` or `await cairn.call(name, args)` and `return` a
JSON value. Pass large or untrusted arguments through the optional JSON `input`
field; it is available as the `input` global and never interpreted as source.
`console`, `process`, filesystem/network access, environment variables and module
loading are unavailable. Source is limited to 65,536 characters; the source and
host-call frames must fit 8 MiB. Intermediate operation results stay in the worker,
so only the final returned value enters model context.

Each execution uses a fresh QuickJS process with an empty environment and private
working directory. It has a 64 MiB JavaScript heap, a 512 MiB address-space limit,
a 2-second CPU budget, a 30-second wall deadline, at most 64 operation calls and a
1 MiB serialized final result. Linux x86_64/aarch64 syscall filtering permits
memory management and inherited pipe I/O, while denying file opens, sockets,
process/thread creation and execution. Other platforms fail closed. Completed,
failed and timed-out workers are terminated and reaped; cancelled HTTP requests
kill their worker. The installation admits at most eight concurrent code workers.

Host calls are sequential, including calls composed through `Promise.all`.
Each operation keeps its own parameter validation and required scope. Internal
installation grants are rechecked for every operation and in the existing mutation
transactions; reducing the calling agent's permissions blocks later calls in the
same script. External calls use the scopes verified for that HTTP request.
Credentials remain in the installation and never enter the JavaScript worker.
Scripts can handle operation errors with `try/catch` (`error.status` carries the
HTTP-style status). Uncaught errors return MCP `isError`; insufficient external
scopes also return the OAuth challenge.

A script is **not a transaction** across its operations. Earlier effects remain
if a later operation fails, times out or loses its grant. Use existing retry-safe
message/attachment IDs and inspect the outcome before retrying mutations. Error
results do not imply that no operation completed.

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

The tools above are for external clients. An agent running in a VM receives
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

When the owner enables **Allow access to the current Cairn installation**, the
agent also receives `cairn_installation` at `/mcp-installation`. It exposes the
complete public management operation catalogue through `search` and `execute`, including `save_skill`, using the
run's revocable grant. This delegates management across the entire installation,
including other agents and their work, even when the caller's direct resource
access is restricted. It requires no external OAuth connection or personal token.
Disabling the option, ending or cancelling the execution, or changing its chat
turn invalidates the access. See [the installation-access decision](adr/0064-opt-in-installation-tools-for-agents.md).

An agent with **Task delegation** also finds `delegate_task`, `task_status` and
`task_cancel` on `cairn_workspace` in conversations it did not receive as a
delegated task. `delegate_task` takes the task text and optionally a title, an
agent the caller covers, a project, a coding agent (`codex`, `claude`,
`opencode`), a model, a reasoning effort and a retry `requestId`. It returns the
delegated conversation; `task_status` with its `chatId` returns its state, outcome,
pending questions and final report. See [delegated tasks](adr/0059-delegated-tasks.md).

## Managing conversations

Public MCP clients and agents with installation access share the same chat operations,
discovered with `search` and invoked through `execute` as `await cairn.<name>(args)`.
`create_chat` defaults to Main for external clients and to the calling agent for
`cairn_installation`. Its optional `projectId` must be allowed by the selected
agent. Creating an empty chat does not start execution. `send_chat_message` accepts
the same provider, model, reasoning, attachments and queue/steer settings as the
apps. Supply a UUID in `message.id` and reuse it with identical content on retries.
A message starts work with the target agent's permissions.

| Scope | Conversation operations |
| --- | --- |
| `read` | `list_chats`, `get_chat`, `read_chat_content`, `list_chat_questions`, `list_chat_artifacts`, `read_chat_artifact`, `list_chat_attachments`, `read_chat_attachment` |
| `manage` | `create_chat`, `rename_chat`, `delete_chat`, `restore_chat`, `upload_chat_attachment`, `set_chat_artifact_visibility` |
| `run` | `send_chat_message`, `edit_chat_message`, `delete_chat_message`, `set_chat_paused`, `stop_chat`, `resume_chat`, `restart_chat_session`, `answer_chat_question` |

`list_chats` supports agent/project filters and active/trash views. Lists and
`get_chat` take `offset` and `limit` (default 50, maximum 100); follow `nextOffset`
until `hasMore` is false. Reading does not mark a conversation read or extend its
retention. Pages of user messages, questions and files are also bounded by
serialized bytes. Oversized message or question records return `truncated: true`; use
`read_chat_content` to recover their complete fields as JSON text chunks, passing
`nextOffset` and `sha256` as with `read_run_content`. This snapshot includes chat
metadata, user messages and questions; saved private answers are redacted.
`get_chat` includes `chat.runId`: use `get_run` and `read_run_content` for assistant
replies and execution activity. Snapshot checksums detect concurrent changes;
ordinary list offsets should be restarted when records change while paging.

`edit_chat_message` replaces a queued message, and `delete_chat_message` withdraws
one; sending and delivered messages cannot be edited. `set_chat_paused` controls
the follow-up queue while letting a current turn finish. `stop_chat` interrupts
that turn, cancels a scheduled wakeup and pauses follow-ups, retaining queued
messages. `resume_chat` resumes a stopped/failed execution under the existing
recovery rules; `set_chat_paused(false)` resumes queued follow-ups. A failed
restored session can use `restart_chat_session` with `confirm: true` to keep its
history and files but start a fresh native session. `rename_chat` sets a title;
the first message or later automatic title evaluation can update it.
`list_chat_questions` exposes fields and status; `answer_chat_question` accepts
an idempotent UUID in `answer.id` and one answer per field in `answer.answers`.
Suggested options are never submitted automatically.

`delete_chat` uses the apps' deletion lifecycle. Busy chats require `confirm: true`
to stop execution and cancel messages/questions. Existing cleanup rules may
permanently remove the workspace, history and files immediately; `restore_chat`
works only while the retained conversation is still eligible. Deleted chats
cannot accept messages or expose file contents through these tools.

Artifacts retain all versions, keys and groups. `list_chat_artifacts` lists their
metadata; `read_chat_artifact` reads original bytes, including private artifacts.
`list_chat_attachments` and `read_chat_attachment` do the same for uploaded files.
File reads return at most 192 KiB of bytes as base64 per call: start at offset 0,
decode each `data` chunk separately and append bytes until `nextOffset` is null.
`file.digest` is the SHA-256 of the complete file. `upload_chat_attachment` accepts
base64 data and a retry-safe UUID `attachmentId` (10 MiB decoded maximum, subject
to transport request limits); include that ID in `message.attachmentIds` to send
it. File IDs are checked against the specified chat. These tools accept no host
filesystem paths. Agents publish new deliverables from their current workspace
using the existing `cairn_workspace.publish_artifact` tool.

`set_chat_artifact_visibility` shares or revokes one artifact version. Files stay
private unless public sharing is requested explicitly. This management tool works
across the installation; the workspace tool remains limited to its current run.
Installation grants are rechecked with database reads and mutations, and again
following asynchronous file reads/uploads and public-link preparation.

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

| Scope | Operations |
| --- | --- |
| `read` | `list_agents`, `list_projects`, `list_tasks`, `list_runs`, `get_run`, `read_run_content`, `list_skills`, `read_skill`, `list_mcps` |
| `manage` | `save_agent`, `update_agent`, `save_project`, `create_task`, `update_task`, `save_skill`, `edit_skill_files`, `create_mcp`, `update_mcp`, `test_mcp`, `disconnect_mcp`, `delete_mcp` |
| `run` | `run_task`, `cancel_run` |

`save_agent` and `save_project` create new records. `update_task` replaces the
existing task configuration; `save_skill` creates or replaces its SKILL.md;
`edit_skill_files` applies a batch of file changes (see [skills](SKILLS.md)).
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
response ceiling without discarding its payload. Use `cairn.read_run_content` within `execute` for this fallback.

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

Conversation backend migration is available on both public MCP and
`cairn_installation`: `get_chat_backend_migration` previews the destination after
an agent profile backend change; `migrate_chat_backend` requires management access,
`chatId`, `nodeId`, `backend` and `confirm: true`. It returns immediately while the
owned migration task runs. Poll the preview for `pending`, `required` and `error`.
An explicit owner may adopt the existing conversation into their native grant;
installation agents retain their bounded parent grant. Web and Android expose the
same confirmed operation from execution-node details. See
[the migration decision](adr/0054-explicit-conversation-backend-migration.md).

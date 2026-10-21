# Gmail

Native Luau package using only generic Cairn OAuth, HTTP, MIME and file APIs. No
Gmail-specific code lives in the installation. Each instance represents one verified
mailbox. Agents need an explicit grant for every instance and must specify
`accountId`; there is no implicit/default account or fallback to another mailbox.

## Connect accounts

1. Enable the **Gmail API** in a Google Cloud project. Configure the OAuth consent
   screen and create an OAuth **Web application** client.
2. In Cairn → Plugins → Gmail → Add account, copy the callback URL into the client's
   authorized redirect URIs. Enter its client ID and client secret in Cairn. The
   secret and Google tokens stay in the encrypted installation vault.
3. Choose capabilities, sign in with Google, verify the displayed email, then save.
   Repeat for each mailbox. Grant the instances on each agent's page.
4. Google external applications in **Testing** require test users and expire refresh
   tokens for these scopes after seven days. Use an appropriate Production or
   Workspace-internal OAuth application for unattended missions. Google may require
   verification of sensitive/restricted scopes depending on distribution and usage;
   an owner must still perform consent and any Google reauthorization.

The owner can also prepare configuration with `manage_plugin` (`setup`, `signin`,
`signin_status`, `form`, `save`). These operations are available through public MCP
and the opted-in installation tool. Neither returns tokens. Agent grants remain
in the owner's agent editor. Web and Android provide the same sign-in, polling,
mailbox check, reconnect and disconnect actions.

Permissions default to read-only. Enable sending, draft editing, organization,
permanent deletion, settings, forwarding/delegation, watches or Workspace
administration separately. Increasing OAuth scopes or changing the OAuth client
requires signing in again. A reconnect cannot replace an existing instance with
another mailbox. `Disconnect` disables local use, removes credentials and attempts
provider revocation; its result says when Google could not confirm revocation.
Google revocation can affect other tokens for the same Google account and OAuth
client. Deleting an instance removes its local data; it does not revoke other
instances' Google grants.

## API coverage

`catalog.luau` is generated from Google's official Gmail v1 discovery document
(revision 20261012): **all 79 REST methods**, their path/query parameters, request
schemas, response schemas, descriptions and OAuth scopes. Regenerate deliberately:

```sh
node scripts/generate-gmail-methods.mjs
```

`methods` lists operations; `methods` with `method` returns detailed schemas.
`request` takes `method` (for example `users.messages.list`), `parameters`, `body`
and optional `options`. It handles profile, messages, threads, drafts, labels,
attachments, imports/inserts, batch changes/deletes, history, watches and settings
(language, IMAP/POP, vacation, forwarding, filters, aliases, delegates, S/MIME and
CSE). `userId` is always `me`; select an account by `accountId`.

Google restricts some sharing, delegation, S/MIME and CSE methods to eligible
Workspace configurations, domain-wide delegation or service accounts. Exposing
their REST methods does not remove those provider restrictions. This package
authenticates mailbox owners with OAuth authorization code, **not service-account
impersonation**. Google enforces its remaining permissions and eligibility checks.

Pagination is explicit: follow `nextPageToken` with the same account and query.
History IDs stay strings, including values beyond JavaScript's safe integer range.
An expired `startHistoryId` requires a full resync. `watch` requires an existing
owner-configured Pub/Sub topic, matching Google Cloud project and Google publisher
permissions; renew before the returned expiry. Cairn does not implement a Pub/Sub
receiver or silently subscribe a mailbox. `history.list` is suitable for missions
that poll for replies.

## Mail composition and files

High-level operations `send`, `reply`, `forward`, `draft` accept a `message`:

```json
{
  "operation": "reply",
  "accountId": "INSTANCE_UUID",
  "messageId": "GMAIL_MESSAGE_ID",
  "requestId": "restaurant-invoice-2026-10-21-first-request",
  "message": {
    "to": [{ "email": "restaurant@example.com", "name": "Restaurant" }],
    "text": "Bonjour, pourriez-vous rectifier cette facture ?",
    "html": "<p>Bonjour, pourriez-vous rectifier cette facture ?</p>",
    "attachments": [{ "id": "TRANSFER_HANDLE", "name": "facture.pdf", "mediaType": "application/pdf" }]
  }
}
```

Recipients are explicit arrays (`to`, `cc`, `bcc`, `replyTo`) of email strings or
`{email,name}`. `from` defaults to the instance's verified email; another address
requires a Google-verified send-as alias and settings access. `reply` reads the
parent and preserves its exact Subject, `threadId`, `In-Reply-To` and References;
choose recipients explicitly, including reply-all recipients when appropriate.
`forward` attaches the original RFC5322 message rather than guessing how to quote
its HTML. `draft` creates a draft, or updates `options.draftId`. Raw MIME, native
draft-send and every other format remain available through `request`.

`send`, `reply`, `forward` require a stable `requestId`. A durable, instance-scoped
HTTP receipt prevents a second send on a replay, including concurrent calls and
installation restarts. Same key with different intent is rejected, including changed attachment bytes. Transport
errors, crashes, oversized responses and timeouts may mean delivery succeeded;
never send again blindly. Search Sent with `rfc822msgid:` and the deterministic
`rfcMessageId` before choosing a new key. Confirmed 429/503 responses are returned,
with rate-limit information when available; writes are never retried automatically.

`attachment` downloads the selected MIME part into a private handle. `fileCreate`,
`fileAppend`, `fileRead`, `fileDelete` expose chunked file operations in the tool.
For files in the conversation VM, use the generic Cairn CLI:

```sh
cairn plugin gmail accounts
cairn plugin gmail --upload INSTANCE_UUID ./facture.pdf
cairn plugin gmail --download INSTANCE_UUID TRANSFER_HANDLE ./facture-recue.pdf
cairn plugin gmail --delete-file INSTANCE_UUID TRANSFER_HANDLE
cairn plugin gmail request --account INSTANCE_UUID --method users.messages.list --parameters '{"q":"from:restaurant@example.com","maxResults":20}'
cairn plugin gmail call --input '{"operation":"send","accountId":"INSTANCE_UUID","requestId":"followup-1","message":{"to":["restaurant@example.com"],"subject":"Facture","text":"Bonjour"}}'
```

Files are temporary (one hour, deleted after restart), private to one instance,
40 MiB maximum, 64 KiB chunks, 200 MiB per instance, 1 GiB and 256 handles per
installation. The Luau worker never loads large file bytes. MIME content is limited
to 28 MiB before encoding and 35 MiB encoded; Google's own message and daily quota
limits also apply. MIME supports Unicode subjects/names, text plus HTML alternatives,
binary attachments, inline content IDs and threaded headers. For large raw/API
responses, use `options.responseFile`; to send a transfer file, use
`options.bodyFile` (`raw`, `json`, `base64url`, with `jsonPointer`).

## Validation

`gmail_plugin` tests use synthetic Google endpoints and the real Luau worker:
all 79 methods, two accounts, explicit account selection, owner-bound PKCE,
one-shot callbacks, refresh serialization, offline access, revocation, MIME reply
headers, drafts, duplicate-send receipts, header injection and a 3 MiB attachment.
No personal messages or credentials are involved in those tests.

Sources: [Gmail REST reference](https://developers.google.com/workspace/gmail/api/reference/rest),
[discovery](https://gmail.googleapis.com/$discovery/rest?version=v1),
[OAuth web-server flow](https://developers.google.com/identity/protocols/oauth2/web-server),
[token expiry and revocation](https://developers.google.com/identity/protocols/oauth2/),
[scopes](https://developers.google.com/workspace/gmail/api/auth/scopes),
[threaded replies](https://developers.google.com/workspace/gmail/api/guides/threads).

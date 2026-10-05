# Installation claims and API relay

The official account API and its Postgres database remain those introduced in
ticket #46. Sign in to the official web app, choose **Add an installation**, and use the
claim code within ten minutes. The app includes this code in the command served
at `/install.sh`; see [One-command installation](INSTALLATION.md). The fallback
`leo claim` command is described below.

For a manager deployment, provide `LEO_OFFICIAL_ORIGIN`,
`LEO_INSTALLATION_CLAIM_CODE`, and optionally `LEO_INSTALLATION_NAME` in its
private environment. The origin must be HTTPS; HTTP is permitted on loopback
for development. The existing manager data and agent-home directories must
already exist. Start the usual `leo serve` command. No public installation URL
or inbound browser connection is needed. Installation browser routes accept only
the trusted in-process identity from the authenticated relay. Remove the claim code from deployment configuration after success.
If a claim is refused, misconfigured or cannot reach the official service, the
manager logs the failure and still starts without a relay identity. Obtain a
new code and restart after fixing connectivity. A lost claim response can mean
the single-use code was consumed; repeating that code must not cause a crash loop.

The installation stores its identity in
`DATA_DIR/installation-relay/identity.json` (directory mode 0700, file mode 0600).
The file contains the official origin, installation ID and bearer credential.
Do not copy it into logs or source control. Later starts use that file and need
no new claim code. Startup never overwrites an existing identity; only an
approved `leo claim` can replace it after detachment.
Postgres stores only a hash of the credential and claim code. Consuming a claim
code and attaching the installation to its owner form one transaction.

## Fallback claim and detachment

Run `leo claim` on the installation with its usual `DATA_DIR` and
`LEO_OFFICIAL_ORIGIN` (the latter defaults to the origin of an existing private
identity). It prints the official `/claim` URL and a temporary code. Sign in to
that official app and enter the code under **Device claim code**, then choose
**Review installation**. Compare its name and public fingerprint with the
fingerprint printed by `leo claim` on a machine you control before choosing
**Claim this installation**. Cancel if they differ or someone sent you the code.
The name is supplied by the machine (or the existing installation record during
recovery), not a verified hostname. The fingerprint identifies the installation
record; it is not a hardware attestation. The machine will hold your conversations,
coding-agent accounts and secrets. Reviewing or cancelling does not approve it. Codes expire in ten minutes; the CLI
polls every two seconds and can be cancelled without deleting its old identity.
The CLI never prints the machine token or polling secret, and writes the new
identity atomically with mode 0600 only after approval. Concurrent CLI claims
against the same identity directory are refused. Restart the manager afterward;
its running connector deliberately does not reload credentials from disk.

The machine starts `/api/relay/device-claim/start` with its name, protocol and,
when present, private installation identity. Only a valid machine token can
reclaim that record. An owned installation refuses reclamation. The browser first reviews through `/api/installations/device-claim/preview`
with its official session, origin and CSRF token. It receives the name, fingerprint
and a temporary confirmation proof bound to this claim and account. Explicit
confirmation through `/api/installations/device-claim` requires that proof;
`/api/relay/device-claim/poll` then attaches the owner and rotates the token in one
transaction. A fresh start keeps only an expiring challenge, reserving no permanent
installation row until approval is collected. Starting a subsequent claim deletes
expired challenges. Recovery always preserves the existing installation row. Codes and polling secrets are stored only
as digests, expire after ten minutes and are single-use. Start, approval and
poll operations use persisted per-peer/account rate limits. Starting another
claim for the same installation invalidates its previous pending challenge.

Choose **Detach installation** inside the installation and confirm explicitly.
The owner is cleared and the active relay is cut; local data and the official
installation row remain. Deleting its owning Leo account also clears ownership
rather than cascading deletion of the installation. Active relays recheck the
identity every second, covering account deletion and the upgrade/detach race.
The digest of the private proof used to start recovery is retained separately
from the current tunnel credential. If its successful response is lost before the
file is saved, detach in the app and retry with that file to recover the same ID.
This proof remains solely for reclamation of an unowned record; it cannot connect
while unclaimed. Successful reclamation rotates it and keeps the same ID.

For deployment migration, interrupted claims and recovery after losing the
private identity file, follow [DEPLOYMENT.md](DEPLOYMENT.md). Execution nodes
continue to use their authenticated direct manager channel.
Additional execution nodes use the reachable manager address selected during
enrollment (LAN, VPN or optional public HTTPS origin), independently of the
official app’s origin. Their disk traffic never uses the official relay.
See [additional execution nodes](DEPLOYMENT.md#additional-execution-nodes)
for TLS and network setup.

The installation initiates a TLS WebSocket at
`/api/relay/{installation}/connect` with its bearer credential. `Hello` offers
protocol versions; `Welcome` chooses a supported version before API traffic.
An incompatible peer is disconnected. The shared `leo-relay-protocol` crate
owns the frames, limits and transport header rules. Version 1 multiplexes finite
API requests and responses by request ID, carrying method, encoded path/query,
selected headers and binary bodies. Account identity and owner/member role come from the official
session and installation access record, then enters the installation's
existing in-memory request extension (#45). HTTP identity headers, browser
cookies and browser authorization credentials are never forwarded.
Bodies are base64 strings in the JSON frames, rather than arrays of byte numbers.
Installation security headers are not trusted: the official service supplies
`nosniff`, `no-store` and a `sandbox` CSP for every relayed response, including
JSON. This prevents ambiguous content types from bypassing the official policy;
the browser can still fetch and read JSON normally.

The browser calls `/api/installations/{installation}/api/...`. Every request
checks the official session and installation role; mutations also require the official
origin and CSRF token. A foreign or unknown installation returns 404. Local
browser authentication routes are excluded from the tunnel. Offline requests
return 503; lost connections fail pending requests, without replaying writes.
The connector retries with exponential backoff from 250 ms to 15 seconds;
WebSocket ping/pong detects dead peers. A revoked identity (HTTP 401) stops
reconnection; run `leo claim`, approve it, and restart the manager. Existing agent execution is independent
of the connector's availability.

Finite request and response bodies are limited to 8 MB, with at most 32 requests
in flight on a connection and a 30-second response deadline on both ends. A slot
is reserved before reading an upload and remains occupied until the installation
responds, even when the browser has abandoned its request. A timeout cancels the
installation request and releases its slot through the response. Oversized
responses return 413, handler/body failures return 502, and saturation returns
503 for that request alone, preserving the tunnel and other requests. Contents
are held only in bounded memory, never in Postgres. This ticket uses one relay
process; a deployment must route the installation's API requests and WebSocket
to that process. Distributed connection routing is not implemented here.

Version 2 adds SSE response headers, binary chunks, completion, credit and
cancellation frames using the same request IDs. Hello offers `[2, 1]`; Welcome
selects the highest shared version. The claim HTTP request declares the minimum
supported version (1), so a new installation can still claim against a v1 service;
the WebSocket handshake remains authoritative for the actual tunnel version.
A v1 installation keeps finite API access;
stream requests return 501 without sending unknown frames or closing its tunnel.
A peer with no shared version is disconnected before it becomes online.

Each stream gets one credit per downstream body read. Installation body polling
pauses until credit arrives, chunks are limited to 64 KiB, and each official HTTP
body has a one-chunk queue. Streams can use 24 of the 32 request slots;
eight slots remain available to finite API requests.
A slow reader, failed stream or failed handler affects only its own request.
Closing the browser response cancels the remote subscription and releases its
slot. The official service disables reverse-proxy SSE buffering and still imposes
its own security headers. Configure proxies to permit long-lived responses.

A lost tunnel ends open browser streams. The existing browser SSE client retries
with its accepted cursor and history version, and the installation's existing
stream replays events (or resets when that history is no longer available).
No events are persisted by the official service. Tunnel loss does not stop an
installation run. During official shutdown, relay bodies close before HTTP
draining, allowing restart without waiting for infinite SSE bodies.

The official session and authenticated `GET /api/installations` expose `online`
from the local relay registry. Availability changes after successful negotiation
and on disconnect; dead peers are detected by the existing heartbeat. The web
shows each installation's status and refreshes only this small account-level
list while visible, with backoff when the official service is unavailable.

Access management uses the same `Relay` handle supplied to `router_with_relay`.
After committing a detachment, call `revoke_access(installation, None)`; after
removing a member, call it with that account ID. It immediately closes the
corresponding browser bodies, including idle or backpressured streams. Account
revocation preserves the tunnel and other accounts' streams. Access generations
prevent an upload authorized before revocation from dispatching any request
afterward, including an ordinary queued request. Work admitted before revocation
is preserved; revocation does not cancel the installation's agent executions.
The detachment, member removal and departure endpoints commit their access
change before revoking the real official HTTP bodies. Detachment also forgets
all memberships and invitations; reclaiming never restores previous sharing.

Validation: run `pnpm test:backend -p leo-official-service --test relay` with a
disposable `LEO_OFFICIAL_TEST_DATABASE_URL`; this uses the isolated backend
launcher and a real installation router. The Playwright project
`journeys-official-relay` uses both real binaries and checks reconnection after
restarting each process. Build those binaries and the web bundle first.

## Current installation in the web application

The official web entry point reuses the existing workspace views (Fil,
conversations, Agents, Atelier and Settings). Browser routes are rooted at
`/installations/{installation}/`; API requests, attachment/portrait/file reads
and SSE connections use `/api/installations/{installation}/api/...`.
The official account session provides authentication and CSRF; the web does
not request the installation's browser session or sign-in endpoints.

With one installation the header shows its name. With several, an accessible
selector changes the current installation. Switching opens a fresh document at
that installation's home, keeping transient workspace state separate. The last
installation is remembered in this browser per Leo account; a valid explicit
installation URL takes precedence. An inaccessible installation URL displays an
account-level message instead of silently opening another installation.

Owners can rename an installation through `PATCH /api/installations/{id}` with
`{ "name": "New name" }`, an official session, origin and CSRF token. Names use
the claim contract (trimmed, 1–100 characters, no control characters). Renaming
works even while the installation is offline and preserves its identity.

Reading views use the scoped SSE connection for updates and cursor-based replay.
One initial finite snapshot keeps a v1 installation readable during a deployment;
there is no recurring conversation snapshot polling. The existing live client
retains its reconnect backoff and history cache.

## Shared-installation limits

Installation HTTP rate limits use the trusted Leo account identity for relayed
requests (300 requests per minute per account), while machine traffic retains
its existing peer-based limits. Browser headers cannot select an identity or
quota. Installation rename requests are limited separately at the official
account level.

At most 24 SSE requests share the tunnel's 32 in-flight slots. Eight slots remain
available to ordinary API requests, so idle member streams cannot exhaust the
capacity needed to send messages. At the stream limit the next stream receives
503 and uses the existing client retry behavior; streams are not evicted.
Cancelling or revoking a stream releases both its stream permit and tunnel slot.


## Public files and scoped MCP requests

The official service authorizes `/mcp` with an installation-scoped grant and
relays it to `/api/mcp`. The relay context carries `mcpScopes`; the installation
applies read/run/manage checks using its existing management tools. Browser
sessions and token secrets never travel to the installation.

Public URLs at `/api/public/installations/{id}/artifacts/{token}` use a separate
read capability. The context carries `publicArtifact`, no account identity, and
permits only GET/HEAD of `/api/shared-artifacts/{token}`. Share-token validation
and revocation stay on the installation. Public files use the existing credited
streams; the official service overwrites security headers and removes cookies.
An offline installation returns an explicit public-file availability message.

Detachment removes MCP grants and outstanding authorization codes in the same
transaction as ownership. Grant creation holds the installation ownership lock,
so reclaiming the same machine never restores its previous MCP credentials.

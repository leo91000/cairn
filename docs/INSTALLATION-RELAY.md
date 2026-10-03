# Installation claims and API relay

The official account API and its Postgres database remain those introduced in
ticket #46. Sign in to the official web app, choose **Add an installation**, and use the
claim code within ten minutes. The installer and fallback `leo claim` command
are separate tickets (#51 and #50).

For a manager deployment, provide `LEO_OFFICIAL_ORIGIN`,
`LEO_INSTALLATION_CLAIM_CODE`, and optionally `LEO_INSTALLATION_NAME` in its
private environment. The origin must be HTTPS; HTTP is permitted on loopback
for development. The existing manager data and agent-home directories must
already exist. Start the usual `leo serve` command. No public installation URL
or inbound connection is needed by the relay; existing local access is retained
until #50. Remove the claim code from deployment configuration after success.

The installation stores its identity in
`DATA_DIR/installation-relay/identity.json` (directory mode 0700, file mode 0600).
The file contains the official origin, installation ID and bearer credential.
Do not copy it into logs or source control. Later starts use that file and need
no new claim code. A second claim never overwrites an existing identity.
Postgres stores only a hash of the credential and claim code. Consuming a claim
code and attaching the installation to its owner form one transaction.

The installation initiates a TLS WebSocket at
`/api/relay/{installation}/connect` with its bearer credential. `Hello` offers
protocol versions; `Welcome` chooses a supported version before API traffic.
An incompatible peer is disconnected. The shared `leo-relay-protocol` crate
owns the frames, limits and transport header rules. Version 1 multiplexes finite
API requests and responses by request ID, carrying method, encoded path/query,
selected headers and binary bodies. Owner identity comes from the official
session and installation ownership record, then enters the installation's
existing in-memory request extension (#45). HTTP identity headers, browser
cookies and browser authorization credentials are never forwarded.

The browser calls `/api/installations/{installation}/api/...`. Every request
checks the official session and owner; mutations also require the official
origin and CSRF token. A foreign or unknown installation returns 404. Local
browser authentication routes are excluded from the tunnel. Offline requests
return 503; lost connections fail pending requests, without replaying writes.
The connector retries with exponential backoff from 250 ms to 15 seconds;
WebSocket ping/pong detects dead peers. Existing agent execution is independent
of the connector's availability.

Finite request and response bodies are limited to 8 MB, with at most 32 requests
in flight on a connection and a 30-second official response deadline. Contents
are held only in bounded memory, never in Postgres. This ticket uses one relay
process; a deployment must route the installation's API requests and WebSocket
to that process. Distributed connection routing is not implemented here.

SSE and streaming responses are deferred to #48 (stream routes return 501).
Their future frames can reuse request IDs while preserving finite request and
response frames, with protocol version negotiation for incompatible changes.
Online-state UI and the multi-installation selector/URLs are #48 and #49.

Validation: run `pnpm test:backend -p leo-official-service --test relay` with a
disposable `LEO_OFFICIAL_TEST_DATABASE_URL`; this uses the isolated backend
launcher and a real installation router. The Playwright project
`journeys-official-relay` uses both real binaries and checks reconnection after
restarting each process. Build those binaries and the web bundle first.

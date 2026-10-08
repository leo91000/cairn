# Direct connection with relay fallback

Decision: [ADR-0033](adr/0033-direct-connection-with-relay-fallback.md). This
document describes the data path, what the official service observes, the
switching rules and the validation plan. Status: installation peer and control plane delivered; web route selection is delivered by #103; Android route selection is implemented in PR #118 (parent issue
[#98](https://github.com/leo91000/leo-agent-manager/issues/98)); the existing relay ([INSTALLATION-RELAY.md](INSTALLATION-RELAY.md))
serves the initial screen and remains the fallback.

## Data path

```
           signaling + authorization (HTTPS, official session, CSRF)
  UI  ───────────────────────────────►  Official service  ◄──── installation tunnel (WSS, outbound)
  │                                         │  issues signed direct grant,
  │                                         │  forwards offer/answer/ICE candidates,
  │                                         │  pushes renew/revoke to the installation
  │
  ├── direct: WebRTC DataChannel (ICE/STUN, DTLS) ─────────────────────────► Installation
  │        requests, responses, credited streams, cancellation
  │
  └── fallback: /api/installations/{id}/api/... over the existing relay ───► Installation
```

1. The UI starts on the relay exactly as today, so the first screen never
   waits for connectivity checks.
2. In the background it requests a direct grant for the current installation.
   The official service checks the session, role and installation, signs the
   grant and forwards the UI's offer through the installation tunnel.
3. The installation verifies the grant signature with the official public key
   received over its authenticated tunnel, answers, and both sides trickle ICE
   candidates through the official service.
4. After DTLS, the installation checks that the peer certificate fingerprint
   equals the one in the grant, then serves relay-protocol frames on the
   DataChannel with the grant's account, role and session as identity.
5. The UI moves new requests and live streams to the direct channel. On ICE
   failure, prolonged disconnection or missed heartbeats it switches back to the
   relay; on a network change it attempts an ICE restart and keeps using the
   relay meanwhile.

Storage is unchanged: conversations, projects, files, secrets and disks stay on
the installation, its nodes and their S3. The official service persists account
metadata, sessions, roles, attachments and audit entries, never relayed or
direct conversation content.

## What stays on the official service

| Flow | Why it stays |
| --- | --- |
| Accounts, sign-in, sessions, claims, sharing, audit, availability, approved version | The official service is the authority (ADR-0028). |
| Signaling and direct grants | Only the official service can authenticate both sides. |
| External MCP clients | They need a public HTTPS URL with OAuth and do not speak WebRTC. |
| Public deliverable links | Anonymous recipients, stable URL, official security headers. |
| Push notifications | Web Push/Android keys are held by the official service, which must reach closed apps. |
| Binary resources (images, attachments, downloads) in the first delivery | A DataChannel cannot directly back a resource URL; they keep using the relay. |
| Fallback traffic | When direct is unavailable. |

## What the official service observes

| Transport | Content | Metadata |
| --- | --- | --- |
| Direct DataChannel | None | Account, installation, timing, DTLS fingerprints, ICE candidates (IP addresses) |
| Relay fallback | In memory while relaying, never persisted | Same as today |

The official service serves the web application code, so a compromised official
service could still substitute code. Direct connection keeps content out of the
official service's memory and logs; it is not end-to-end encryption against it.

## Authorization and revocation

- A grant names the installation, Leo account, role, session, access generation,
  client DTLS fingerprint and an expiry of a few minutes.
- Renewal goes through the official service, which re-checks session and role.
- The official service pushes revocation over the installation tunnel for
  logout, session expiry or revocation, member removal, role change, detachment,
  credential rotation and revoke-and-forget; the installation closes matching
  channels immediately, including active streams.
- While the tunnel is down, no new direct connection can start and existing ones
  close at grant expiry. A normal official shutdown does not itself revoke them.
  When the tunnel reconnects with a new key (including after an official restart
  or deployment), the installation closes the old leases immediately: the new
  official tunnel no longer tracks them for revocation. Clients fall back to the
  relay and negotiate fresh direct grants, applying the switching rules below
  to avoid loss or duplication. Agent executions are never stopped by transport
  changes.
- The installation never accepts anonymous local access and no local password is
  reintroduced.

## Switching rules

- Reads and live streams are retried on the other route; streams resume from
  their accepted cursor and history version, without loss or duplication.
- Message sends carry their client message ID; a retry that receives
  "identifier already used" counts as delivered.
- Other mutations in flight during a switch are not replayed and fail visibly,
  as a relay disconnection does today.
- Both routes use the same per-pool values: 32 requests in flight, 24 streams,
  8 MB bodies, 64 KiB stream chunks with credits, 30-second response deadline.
  Application pools are separate, with the scopes and combined ceilings below;
  saturating direct capacity leaves the relay's fallback capacity available.
- On the DataChannel, frames are fragmented into messages of at most 16 KiB:
  larger messages are not interoperable without SCTP interleaving, and a large
  message would block every other request on the ordered channel.

## Limits

- Strict UDP filtering or symmetric NAT on both sides falls back to the relay.
- The installation needs outbound UDP for direct connection; a setting disables
  direct connection entirely.
- Older installations or clients without the new protocol version stay on the
  relay.
- A single official process remains required (ADR-0032).
- Signaling relays the DTLS fingerprints, so the official service could
  impersonate an installation (RFC 8827). This matches the existing trust model:
  it also serves the web application code. Android could later pin an
  installation key obtained at claim time; the web cannot.
- Chrome's Local Network Access may eventually prompt before same-LAN
  connections; the relay remains usable if the prompt is refused.
- No reliable published figure gives the share of sessions that can connect
  directly (studies report roughly 70–82 %). Qualification (#105) measures it.
- TURN would add an end-to-end encrypted relay between direct and the
  application relay; it is deferred (ADR-0033).

Sources and library options: [DIRECT-TRANSPORT-RESEARCH.md](DIRECT-TRANSPORT-RESEARCH.md).

## Validation plan

The reproducible Linux bench, commands and current direct/relay evidence are in
[NETWORK-BENCH.md](NETWORK-BENCH.md). It runs without KVM; Android device
qualification remains part of #105.

A network test bench runs both real binaries, a headless browser and the Android
emulator through simulated networks:

- same LAN, NAT on either side, both sides behind NAT;
- UDP blocked, symmetric NAT (relay expected);
- network change and packet loss during a stream (ICE restart or fallback);
- reconnection with event resume: no lost or duplicated events, no double send;
- revocation during a stream on both routes: logout, session expiry, member
  removal, detachment, rotation;
- an old installation or client: relay only;
- the official tunnel down: no new direct connection, existing ones end at
  grant expiry, agent executions continue.
- official restart and tunnel reconnection: the new key closes old leases before
  expiry, the relay recovers, fresh grants work and logout still revokes them.

## Delivered control plane (#100)

The authorization/signaling contract is implemented in protocol v4; the WebRTC
peer is delivered by #101; the web transport is delivered by #103 and Android's data transport is implemented in #104/PR #118. No inbound local
browser access or local password is introduced. The current shipped data path
starts on the relay, with web and Android negotiating direct traffic in the background.

All routes use the official session and installation access checks (foreign,
unknown, detached or removed-member installations return 404). POST also requires
the configured origin and CSRF token:

- `POST /api/installations/{id}/direct/authorize` with
  `{ "fingerprint": "sha-256 AA:…", "versions": [4] }` returns
  `{ "available": true, "grant": { "claims": { … }, "signature": "…" } }`
  after installation verification, or `{ "available": false }` for an older
  client/tunnel or an offline installation. The relay stays usable in all cases.
- `POST /api/installations/{id}/direct/{connection}/renew` uses the same input,
  rechecks session and role, and returns a fresh signed grant for that connection.
  Another session cannot renew it, even for the same account.
- `POST /api/installations/{id}/direct/{connection}/signal` accepts
  `{ "kind": "offer" | "answer", "sdp": "…" }`, or
  `{ "kind": "candidate", "candidate": "candidate:…", "sdp_mid": "0",
  "sdp_m_line_index": 0 }`. An empty candidate marks end-of-candidates.
- `GET /api/installations/{id}/direct/{connection}/events` returns SSE `signal`
  events containing the same metadata from the installation. It is bound to the
  issuing account/session, permits one reader and closes on expiry or revocation.
  A lagging reader closes instead of accumulating unbounded signals.

The installation receives its public verification key only from the authenticated
tunnel. Ed25519 signatures cover the protocol context and every claim. Keys are
per tunnel, so a reconnect cannot restore an old authorization. Nonces, leases and
signal queues are bounded in-memory control state. Access generations also remain
in memory for the lifetime of the tunnel; none of these are conversation storage. See [Version 4 limits and installation integration](INSTALLATION-RELAY.md#version-4-direct-authorization-and-signaling-100).

The installation tolerates up to 30 seconds of signing clock skew in the maximum
remaining grant lifetime; its local expiry is strict. Direct signaling uses
separate bounded queues from fallback responses and stream credits. A client
signal gets HTTP 204 only after installation acknowledgement (429 on refusal,
503 on saturation or acknowledgement timeout). Per-account quotas and reserved
owner capacity prevent member bursts from exhausting the owner's allowance;
see the version-4 limits in INSTALLATION-RELAY.md.

## Delivered installation peer (#101)

The production installation connector runs a UDP-only WebRTC peer alongside its
existing authenticated tunnel. It answers only verified v4 grants, compares the
**observed DTLS certificate** with the signed fingerprint before dispatch, and
uses the existing installation dispatcher for requests and credited streams.
Account and role come from the verified lease; identity/capability fields in
client frames cannot grant MCP or anonymous-artifact access. Renewal and
revocation use the #100 verifier and cancellation tokens. Disconnecting a
transport cancels that transport's requests, never agent execution.

The peer requires exactly one reliable, ordered DataChannel named `leo.v4`.
Trickle candidates use the existing authenticated signal routes. A failed peer,
malformed frame, saturated ingress or duplicate channel closes only that direct
connection; the HTTPS relay remains available. Clients negotiate a fresh grant
after a closed connection; the web applies safe retry and route selection (#103); Android remains #104.

### STUN and configuration

The official service hosts a Binding-only STUN responder in its single process.
The source defaults to `stun:<official-origin-host>:3478`, or the dedicated
`LEO_OFFICIAL_STUN_URL`. Authorize/renew responses expose `iceServers`; the
installation receives the same URL in the authenticated `DirectKey` frame.
No public third-party STUN dependency or additional IP disclosure is introduced.
STUN carries address-discovery metadata only, never account credentials or
application content. TURN remains deferred. UDP 3478 is published directly by
the official Compose definition; Traefik routes HTTPS only.

- `LEO_DIRECT_ENABLED=false` disables installation direct authorization and peers.
  The default is `true`; the authenticated tunnel and relay keep working.
- `LEO_DIRECT_STUN_URLS=stun:host:port[,stun:host:port]` overrides the official STUN
  source, at most four URLs of 256 bytes each. TURN URLs are rejected.
- `LEO_DIRECT_PUBLIC_IP` optionally announces the installation host's public
  unicast address for an explicitly verified port-preserving NAT. It avoids the
  same-host STUN hairpin trap without publishing any installation port; see the
  [production runbook](PRODUCTION-CAIRN.md#stun-and-direct-installation-connectivity-101).
- An unset or empty `LEO_DIRECT_STUN_URLS` uses the authenticated official STUN
  source. An empty override logs this choice explicitly; it does not disable direct.
- Invalid configuration disables the direct peer and logs a fixed reason naming
  the setting while retaining the relay. No configuration value or signaling
  payload is logged.
- The peer uses ephemeral UDP sockets for ICE; it requires no forwarded inbound
  port, public certificate, local password or anonymous listener.

### DataChannel framing contract

Binary messages contain a 13-byte envelope followed by JSON-frame bytes:
`version:u8=1`, `transfer:u32`, `total:u32`, `offset:u32`; integers are network
byte order. Transfer IDs are nonzero. `total` is the UTF-8 JSON byte count for
one existing relay `Frame`; body bytes retain the protocol's base64 encoding.
Each message is at most 16,384 bytes, leaving 16,371 bytes per fragment. Offsets
must be contiguous within a transfer, although separate transfers (including
credits/cancellation) can interleave. `total=offset=0` with no payload abandons
an incomplete transfer without dispatch. Unknown versions, duplicate starts,
invalid offsets, text messages or excessive sizes close the direct connection.

Reassembly retains at most 32 incomplete transfers per channel and a 30-second
idle deadline, measured from the last valid fragment. Active interleaved uploads
can exceed thirty seconds overall. **All installation peers share a `2 * MAX_FRAME` reservation
budget** (about 21.5 MB). One `MAX_FRAME` is reserved for the owner; all members
share the other. Each verified account can reserve at most `MAX_FRAME` across
its peers, and each peer has the same declared-size cap. Each transfer reserves
its declared total before buffering; completion, abandonment, timeout,
connection closure or panic releases that reservation. A shortage on a peer
without incomplete assemblies applies backpressure: it retains one packet of
at most 16 KiB while its bounded SCTP receive buffer pauses input. Expiry still
runs during this wait and releases the holder's reservation after 30 seconds
without valid fragment progress, including while a rejection waits on the
bounded response queue.
If the waiting channel already holds assemblies, blocking its ordered input
would prevent their completion. Instead, only the new transfer is rejected:
a request receives a 503 using its initial fragment's bounded request ID.
Canonical request frames serialize `type` and `id` before the body. If that
identity is absent from the initial fragment, the transfer is drained without
dispatch and the caller's existing response deadline applies. Rejected and
expired transfers retain only bounded sequence metadata to drain fragments,
counted with pending assemblies toward the 32-transfer limit. The peer stays alive; the affected
request uses its existing response deadline and safe retry rules. Buffers grow only as bytes
arrive and their capacity is bounded by the reservation. Sparse declarations
therefore cannot build up 32 independent maximum-frame buffers. Application limits remain 32 requests, 24 streams globally, 8 streams per
account, 8 MB bodies, 64 KiB credited chunks and the existing 30-second response
deadline. SCTP send/receive buffers are bounded to 64 KiB; application ingress and
egress are bounded separately. Cancellation and revocation stop dispatch
immediately; a bounded 500 ms transport-close grace sends the SCTP stream reset
before shutting down UDP sockets.

The Rust network client records `direct` only after an authenticated API response
arrives over this DataChannel and a selected UDP ICE candidate pair is observed.
Relay observations come from the official response's `x-leo-transport: relay`
header. The web reports successful API/stream traffic through its transport observer (#103); this distinction prevents the
bench from mistaking UDP probe reachability for an authorized direct connection.

### Capacity scopes and candidate policy (#117)

Direct and relay application pools are deliberately **separate**, not one shared
installation pool. Direct uses 32 requests and 24 streams across all installation
peers, with eight streams per verified account across those peers. The relay
retains 32 authenticated requests and 24 streams per installation tunnel, with
eight browser streams per account across all installations in the official
process. For one installation the maximum combined allowance is therefore 64
requests / 48 streams, and at most 16 browser streams for one account (eight on
each route). Relay public downloads retain their independent four-slot pool;
MCP and direct authorization/signaling allowances are unchanged. Saturating
reassembly or direct application capacity never borrows fallback slots. These
transport limits do not count or cancel agent executions.

Installation mDNS is explicitly disabled (`MulticastDnsMode::Disabled`): it
binds no multicast listener on UDP 5353 and parses no multicast traffic. Bounded
`.local` names are removed from embedded SDP candidates and ignored on trickle
before calling the SDK. **#103/#104 must
use numeric host/reflexive candidates or the HTTPS relay**; an mDNS-only browser
LAN offer can fall back even on the same LAN. No automatic DNS resolver or new
installation listener compensates for this choice; #105 must qualify this case.

The existing shared signaling validator removes loopback, link-local,
unspecified, multicast and IPv4 broadcast destinations, including IPv4-mapped
IPv6 forms, from embedded and trickled candidates. A mixed offer keeps its usable
numeric candidates. An offer containing candidates but none usable returns a
clear 400; a candidate-free offer remains valid for trickle. Ignored trickle
candidates return 204 without entering ICE, so a browser/Android client can
continue negotiating with later usable candidates. Syntax, size, authorization
and DTLS fingerprint checks still apply to all original metadata. The installation also skips
its own special-address candidates. RFC1918 and IPv6 unique-local unicast
addresses remain permitted: authorized clients can share a LAN or routed VPN,
and neither side knows an authenticated list of the other's private subnets.
These addresses only cause bounded UDP ICE connectivity checks; they never
fetch an HTTP resource or authorize application access. Signed grants, observed
DTLS certificate comparison and immediate revocation remain mandatory.

The peer survives broadcast signal lag; a lost negotiation can time out and use
the relay while later grants still work. Completion is tracked by Tokio task ID,
so a failed/panicking job releases its authorization and peer registry entry.

## Delivered web transport (#103)

The official document starts every request and live subscription on the existing
HTTPS relay. Native `RTCPeerConnection` establishes one reliable ordered `leo.v4`
channel in the background, using the official grant, signal routes and STUN URLs.
A discreet **Direct / Relais** indicator describes the current default route;
individual successful API responses and accepted live batches emit a
`leo-transport-observation` event containing only route, method, path and cursor.
The bench observes these application events, never infers a route from ICE state.
Binary resource URLs and non-JSON uploads continue through the relay.

Grant renewal runs 30 seconds before the signed deadline, with a minimum
10-second delay; the local expiry timer remains armed until renewal is accepted.
If renewal does not extend a session-capped deadline, no further renewal is
attempted for that lease. A 30-second negotiation deadline,
10-second application heartbeat (a protocol HEAD request, with a
5-second response deadline starting after transmission), channel closure, ICE failure and signaling-reader loss all
return traffic to the relay. Unsupported clients/installations and refused local
network permission cause no visible connection error. Temporary failures retry
with exponential backoff from 30 seconds to five minutes, reset when an
authorized channel opens. `available: false`,
403 and permission denial stop direct attempts until the session or network
changes; a capacity 503 backs off while the relay remains usable. A recovery
observed by the existing installation availability check also resumes
negotiation, covering official restart/tunnel reconnection without polling
incompatible installations through the direct endpoints. Offline and
hidden documents do not negotiate. An established peer in a hidden document
closes after a 30-second grace period, stopping signaling, renewal and heartbeat;
visibility restores negotiation after any pending backoff deadline. A retry whose
deadline elapsed while hidden resumes when visible, without negotiating in the
background. Network-information changes restart negotiation only when `type` or
`effectiveType` changes, debounced by one second; rtt/downlink estimates leave the
peer, refusal state and backoff intact. Online/offline events act immediately.

A local closing channel or exhausted direct slots cannot dispatch a request:
the operation uses fallback capacity, including mutations and live subscriptions.
Once a complete request frame has been sent, the switching rules above apply.
The client response deadline is 35 seconds, allowing the dispatcher's 30-second
504 to arrive first; expiration cancels only that request, preserving other
requests and streams on the peer. A locally saturated heartbeat waits for its
next interval instead of closing a healthy peer. Outgoing frames interleave one
16 KiB fragment at a time, so large uploads do not monopolize reads, credits or
heartbeats; conservative payload reservations preserve the peer's aggregate
reassembly limits.

Actual Network Information identity changes and browser `online` events restart ICE through a
**fresh peer and fresh authorization**, with `iceRestart: true`, while the relay
serves traffic. This deliberately preserves #101's refusal of renegotiation and
second channels instead of extending the installation peer in the web ticket.
The old channel is closed; signals from an older negotiation cannot reactivate it.
A failed peer detected without a browser network notification follows the same
fallback/fresh-negotiation path. The server still closes old-key leases on tunnel
reconnection and scoped revocation, independently of the browser.

Reads interrupted in flight retry through HTTPS. Message sends retry with the
unchanged client ID/body; the exact existing "identifier already used" conflict
counts as delivery. Abort signals and other mutations are never retried. Live
subscriptions use the existing accepted cursor/history logic on both routes;
callbacks from replaced subscriptions are ignored. Closing a subscription cancels
its request, and transport changes never cancel agent execution.

The browser implements the existing binary envelope without changing the wire
protocol: 16 KiB packets, 13-byte headers, base64 bodies, 32 in-flight requests,
eight streams per account (the installation also enforces its global limits),
8 MB bodies, 64 KiB credited chunks and 30-second deadlines. Reassembly bounds
incomplete transfers, aggregate buffered bytes and assembly age. The sender waits
on `bufferedAmount` before exceeding 64 KiB; aborting a partially sent frame sends
the existing zero-length abandonment envelope before cancellation.

Following the decision recorded in #117, the web ignores mDNS `.local` candidates
before signaling or passing them to the WebRTC implementation. Numeric host and
server-reflexive candidates use the existing authenticated verifier and official
STUN module. Loopback, unspecified, link-local, multicast and broadcast
destinations are filtered too, including their IPv4-mapped IPv6 forms; private
unicast remains useful on authenticated LAN/VPN paths. The filter applies to
offer/answer candidate lines and trickle in both directions. A single 400/429
candidate refusal does not abandon negotiation with other usable candidates.
If no usable numeric path exists, the relay remains available. The
installation resolver/listener hardening belongs to #117.

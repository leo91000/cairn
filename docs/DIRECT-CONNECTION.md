# Direct connection with relay fallback

Decision: [ADR-0033](adr/0033-direct-connection-with-relay-fallback.md). This
document describes the data path, what the official service observes, the
switching rules and the validation plan. Status: planned (parent issue
[#98](https://github.com/leo91000/leo-agent-manager/issues/98)); the existing relay ([INSTALLATION-RELAY.md](INSTALLATION-RELAY.md))
remains the shipped transport and the fallback.

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
- Without the tunnel, no new direct connection can start and existing ones close
  at grant expiry. Agent executions are never stopped by transport changes.
- The installation never accepts anonymous local access and no local password is
  reintroduced.

## Switching rules

- Reads and live streams are retried on the other route; streams resume from
  their accepted cursor and history version, without loss or duplication.
- Message sends carry their client message ID; a retry that receives
  "identifier already used" counts as delivered.
- Other mutations in flight during a switch are not replayed and fail visibly,
  as a relay disconnection does today.
- Limits are identical on both routes: 32 requests in flight, 24 streams,
  8 MB bodies, 64 KiB stream chunks with credits, 30-second response deadline.
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

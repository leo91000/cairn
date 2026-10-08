# Android direct transport (#104)

Implementation follows ADR-0033 and the delivered v4 relay protocol. The relay
serves the initial screen while an authorized `leo.v4` DataChannel is negotiated.
Binary transfers, account operations and signaling continue over the relay.

## Test seams agreed by the ticket

- `LeoApi` requests and `live()` streams: JVM tests exercise route switching,
  safe replay, client-message deduplication and accepted cursor/history recovery.
- The v4 DataChannel adapter: the existing 13-byte, network-order fragmentation
  envelope, 16 KiB messages, bounded reassembly and cancellation.
- Instrumented Android client against the real installation/control plane:
  direct response, UDP filtering with relay recovery, network-change recovery
  and revocation during a stream. JVM/Robolectric results are not device evidence.

## Constraints

The installation verifier, existing dispatcher, limits and network bench are
reused. Transport closure cancels subscriptions, never agent executions. Old
leases close on tunnel reconnection with a new key. Unsupported installations
keep using the relay. Authorization renews before expiry through the official
service. No TURN, local password, anonymous local access or deployment is added.

Library selection and the measured universal APK impact are documented in
[the artifact research](../../docs/ANDROID-DIRECT-TRANSPORT.md). Device qualification
uses the [existing network bench](../../docs/NETWORK-BENCH.md#android-native-adapter-104);
the implementation PR links the exact-head CI and sanitized execution evidence.

## Native interoperability decisions

The pinned SDK advertises the optional `renomination` ICE extension. The delivered
v4 verifier deliberately accepts `trickle` only. Android removes exactly that
optional advertisement from its offer (including the native local description),
while preserving its DTLS fingerprint and ICE credentials; unknown future
attributes still fail verification and fall back to the relay. Replaying the
captured private offer through the existing `DirectSignal::valid` verifier went
from false to true with this single change. No verifier policy is widened.

The upstream [transport options](https://webrtc.googlesource.com/src/+/8990f2a572516b5ca0eba08b2982044d7a47e215/p2p/base/transport_description_factory.h)
define renomination as requiring both peers' support. The native dependency and
third-party notices are described in [the artifact research](../../docs/ANDROID-DIRECT-TRANSPORT.md).

On a default-network change, Android immediately closes the current traffic
adapter, resumes reads/subscriptions over the relay, then creates a new ICE peer
and authorization. The delivered installation accepts an offer for a fresh peer,
not renegotiation on an established peer; Android calls native `restartIce()` and
requests an ICE restart for the replacement. No agent execution is stopped.

## Wire failure and cancellation qualification

The `LeoApi` JVM tests verify relay recovery after malformed finite response
bodies/status/headers and malformed stream endings, preserving accepted stream
cursor/history. Wire frames are validated before reaching HTTP/live readers;
Base64 uses the existing Rust codec's canonical standard encoding. Non-replayable
mutations still fail visibly.

Adapter tests independently verify the 16,384-byte packet and 10,732,204-byte
frame caps, at most 32 incomplete assemblies, and the existing 13-byte abort
envelope when cancellation happens after the first outgoing fragment. An
individual cancellation leaves the peer available for a subsequent direct read.

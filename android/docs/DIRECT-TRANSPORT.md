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

Library selection, measured APK impact and validation evidence will be recorded
here and in the implementation PR before it becomes ready for review.

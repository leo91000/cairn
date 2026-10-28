# Android direct transport (#104)

Implementation follows ADR-0033 and the delivered v4 relay protocol. The relay
serves the initial screen while an authorized `cairn.v4` DataChannel is negotiated.
Binary transfers, account operations and signaling continue over the relay.

## Test seams agreed by the ticket

- `CairnApi` requests and `live()` streams: JVM tests exercise route switching,
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
keep using the relay. Authorization renews before expiry through the beacon
service. No TURN, local password, anonymous local access or deployment is added.

Library selection and the measured universal APK impact are documented in
[the artifact research](../../../docs/ANDROID-DIRECT-TRANSPORT.md). Device qualification
uses the [existing network bench](../../../docs/NETWORK-BENCH.md#android-native-adapter-104);
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
third-party notices are described in [the artifact research](../../../docs/ANDROID-DIRECT-TRANSPORT.md).

Android uses the web client's numeric ICE destination policy before signaling and
before handing received candidates to libwebrtc. Hostnames (including `.local`),
unspecified, loopback, link-local, multicast and broadcast addresses are removed
from trickle signals and SDP candidate lines, including IPv4-mapped IPv6 forms
of those addresses. Private unicast candidates remain usable; no DNS resolution
is added. An individual candidate refused with HTTP 400 or 429 is skipped while
other candidates continue negotiating. Authorization failures still stop the
peer and retain the relay. The same-LAN device case refuses one host candidate
per real connection and requires a successful direct response afterwards.

On a default-network change, Android immediately closes the current traffic
adapter, resumes reads/subscriptions over the relay, then creates a new ICE peer
and authorization. The delivered installation accepts an offer for a fresh peer,
not renegotiation on an established peer; Android calls native `restartIce()` and
requests an ICE restart for the replacement. No agent execution is stopped.

## Wire failure and cancellation qualification

The `CairnApi` JVM tests verify relay recovery after malformed finite response
bodies/status/headers and malformed stream endings, preserving accepted stream
cursor/history. Wire frames are validated before reaching HTTP/live readers;
Base64 uses the existing Rust codec's canonical standard encoding. Non-replayable
mutations still fail visibly.

Adapter tests independently verify the 16,384-byte packet and 10,732,204-byte
frame caps, at most 32 incomplete assemblies, and the existing 13-byte abort
envelope when cancellation happens after the first outgoing fragment. An
individual cancellation leaves the peer available for a subsequent direct read.

## Failure evidence

The native bench enables an internal `DirectTrace` only for its primary client.
The recorder keeps the last 128 events and preserves the first observed failure
even if that event leaves the buffer. It accepts enum categories and numeric
metadata; it never receives wire content, URLs, IDs, SDP, headers or exception
messages. Native logging remains disabled. The trace is buffered in memory and
written from the test's `finally` block before deliberate client cleanup.

Each event includes monotonic elapsed milliseconds, device wall time, the delta
between wall and monotonic elapsed time, and grant remaining milliseconds when
known. Native events include foreground/default-network transitions, ICE and
DataChannel states, grant updates/caps and the initiating close category for
each attempt. Response events include HTTP status and the actual direct-channel
response tag. `LARGE_REQUEST_BEFORE`/`AFTER` and adjacent route observations
bracket the real 1 MB application refusal.

`tests/network-bench.py` collects `<scenario>.transport.json` before interpreting
the instrumentation verdict, including on assertion failure. It also samples
guest/host wall-clock offset before and after instrumentation. Offset bounds
include ADB command RTT and the guest clock command's one-second quantization;
`hostWallDeltaMs` exposes host wall-clock movement during sampling. A missing
trace or clock sample is explicitly unavailable and does not replace the
original instrumentation failure. These samples cannot establish an earlier
failure's clock behavior or distinguish a transient clock change between samples
without the device event trace.

This evidence was added after the same-LAN failure in
[run 40696410719](https://github.com/leo91000/cairn/actions/runs/40696410719)
at source `06526eccfa1a7ba1ed3d54dcd5040335ee9e2668`: the real large POST returned
413, then the route assertion expected direct but observed relay after 120.394 s.
Its original artifacts do not identify the transport failure or clock behavior.
The later passing qualifications do not establish a cause or a causal repair.

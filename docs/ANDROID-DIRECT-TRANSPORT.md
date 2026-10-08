# Android direct transport: native dependency

Verified 2026-10-07 for #104. The transport and replay rules remain those of
[ADR-0033](adr/0033-direct-connection-with-relay-fallback.md) and
[DIRECT-CONNECTION.md](DIRECT-CONNECTION.md).

## Artifact choice

Use the pinned Maven Central artifact **`io.github.webrtc-sdk:android:150.7871.01`**,
the unprefixed `org.webrtc` build. The publisher's
[README](https://github.com/webrtc-sdk/android/blob/v150.7871.01/README.md) recommends
that coordinate; the [release](https://github.com/webrtc-sdk/android/releases/tag/v150.7871.01)
was published on 2026-08-31, and [Maven metadata](https://repo.maven.apache.org/maven2/io/github/webrtc-sdk/android/maven-metadata.xml)
lists it as the latest stable release, updated 2026-09-05. The
[POM](https://repo.maven.apache.org/maven2/io/github/webrtc-sdk/android/150.7871.01/android-150.7871.01.pom)
declares no transitive dependencies. No Stream account or hosted video SDK is needed.

The alternative `io.getstream:stream-webrtc-android:1.3.10` is still the latest
Stream stable artifact, published 2025-09-11 according to its
[Maven metadata](https://repo.maven.apache.org/maven2/io/getstream/stream-webrtc-android/maven-metadata.xml).
The current webrtc-sdk publication gives this delivery a more recent native
baseline. This is a maintenance judgement based on release history, not a
promise about future releases.

The downloaded [150.7871.01 AAR](https://repo.maven.apache.org/maven2/io/github/webrtc-sdk/android/150.7871.01/android-150.7871.01.aar)
has SHA-256 `0a1627b1a48c2bc17d9a40d62fc47bd45166f44a311e95917f147c402de379b0`.
Its manifest declares minimum Android API 21, compatible with Leo's minimum 26.
The published sources jar contains only a manifest: inspect `classes.jar` with
`javap` to verify the actual shipped interface, rather than assuming that current
upstream sources exactly match the binary.

## License and shrinking

The AAR's [POM](https://repo.maven.apache.org/maven2/io/github/webrtc-sdk/android/150.7871.01/android-150.7871.01.pom)
declares **BSD-3-Clause** for WebRTC. The build/publishing repository separately
uses [MIT](https://github.com/webrtc-sdk/android/blob/v150.7871.01/LICENSE).
The publisher provides a consolidated
[WebRTC and third-party notice](https://github.com/webrtc-sdk/android/blob/v150.7871.01/Licenses/WEBRTC.md),
including WebRTC's BSD notice and bundled dependencies' notices. Preserve that
notice in distribution materials; the established Android packaging location is
`android/app/src/main/assets/licenses/`, alongside existing font notices. Use
`webrtc-150.7871.01-NOTICES.txt` there, sourced from this exact release tag.
The downloaded notice is 788,358 bytes, SHA-256
`d1f9382c6878ac024155fd6d44a5977329108bb8b0a01cea40e4a2f1d7de252e`.

The 150.7871.01 AAR has **no consumer ProGuard rules**. Preserve the Java/JNI
interface in Leo's release rules with `-keep class org.webrtc.** { *; }`.
This mirrors the conservative rule shipped in the
[Stream core](https://github.com/GetStream/webrtc-android/blob/main/stream-webrtc-android/consumer-rules.pro).
Do not infer release safety from a debug-only build: verify a minified build as
well as an emulator build.

## Shipped interface and configuration

The following were checked with `javap -classpath classes.jar` on the exact AAR.
The [publisher's m150 Java implementation](https://github.com/webrtc-sdk/webrtc/tree/m150_release/sdk/android/api/org/webrtc)
explains callback ownership and behavior; binary inspection is the source of
truth for availability in the pin.

- Initialize `PeerConnectionFactory` once with the application context, then
  use `builder().createPeerConnectionFactory()` and
  `createPeerConnection(RTCConfiguration, Observer)`. Create a DataChannel before
  the offer; no audio/video tracks, EGL objects, capture permissions, or media
  streams are needed for a data-only connection. Release factory and peer native
  resources when their owner closes.
- `RTCConfiguration` exposes `iceServers`, `tcpCandidatePolicy`,
  `iceTransportsType`, and `continualGatheringPolicy`. Use only the authenticated
  control response's STUN URLs, `TcpCandidatePolicy.DISABLED`,
  `IceTransportsType.ALL`, and `ContinualGatheringPolicy.GATHER_CONTINUALLY`.
  With no TURN URLs and TCP candidates disabled, blocked UDP falls back to Leo's
  existing application relay, as required by ADR-0033.
- `createOffer(SdpObserver, MediaConstraints)`, `createAnswer`,
  `setLocalDescription`, `setRemoteDescription`, and `addIceCandidate` exist.
  Wait for the relevant successful SDP callbacks; queue remote candidates until
  the remote description is set. Send signaling through the existing control
  protocol, including the SDP fingerprint used by its grant verifier.
- `restartIce()` exists. Call it on the default-network transition and generate
  and signal the new offer; restarting ICE alone does not exchange new SDP with
  the installation. `setConfiguration(RTCConfiguration): boolean` also exists.
  Do not let the native renegotiation callback and the connectivity callback
  concurrently create offers. A default-network callback should be unregistered
  when its owning session closes. Android documents that the old default
  network's connections can be terminated on transition in
  [Read network state](https://developer.android.com/develop/connectivity/network-ops/reading-network-state).
- `PeerConnection.Observer` requires `onSignalingChange`,
  `onIceConnectionChange`, `onIceConnectionReceivingChange`,
  `onIceGatheringChange`, `onIceCandidate`, `onIceCandidatesRemoved`,
  `onAddStream`, `onRemoveStream`, `onDataChannel`, and
  `onRenegotiationNeeded`. `onConnectionChange` and
  `onSelectedCandidatePairChanged` are default methods.
- `DataChannel.Init` has `ordered=true`, `maxRetransmits=-1`,
  `maxRetransmitTimeMs=-1`, `negotiated=false`, and `id=-1`: these defaults provide
  the reliable ordered channel used by the existing peer. `DataChannel` exposes
  `send(Buffer): boolean`, `state()`, `bufferedAmount()`, `close()`, and `dispose()`.
  `DataChannel.Observer` requires `onBufferedAmountChange(long)`,
  `onStateChange()`, and `onMessage(Buffer)`. **Copy the received buffer inside
  the callback** before handing it to asynchronous work; native memory is released
  on callback return. `send` consumes the supplied ByteBuffer's remaining bytes.
- `getStats(RTCStatsCollectorCallback)` provides `RTCStatsReport.statsMap` and
  each `RTCStats.members`; use selected candidate-pair information for real
  route evidence. A requested direct route or an open channel does not by itself
  prove that a particular application request used it. Expose the route chosen
  by the request/stream transport to the tests and the discrete UI indicator.

Retain the existing 16 KiB fragmentation envelope and bounded reassembly rather
than adding a second wire format. Library reliability does not replace the
application's replay, cursor, deduplication, grant renewal, and revocation rules.

## Size measurement

Inspection of the exact AAR yields these native-library sizes. Entries are stored
uncompressed in this AAR; their sizes are **not** measured APK deltas.

| ABI | Native library bytes |
| --- | ---: |
| armeabi-v7a | 6,809,404 |
| arm64-v8a | 12,287,312 |
| x86 | 12,818,648 |
| x86_64 | 16,166,352 |

The same CI `assembleDebug` task produced these universal APKs, with identical
runtime, ABI packaging and build options (all four ABIs above):

| Build | Commit | APK bytes |
| --- | --- | ---: |
| Before native transport | `06c68695b934c413be3e9881f873abc15e201fff` | 18,815,941 |
| Native transport and packaged notices | `096d78af56a1f7f80c731bb7f49812b6be569316` | 67,306,210 |

The measured increase is **48,490,269 bytes (+257.7%)**, including the native
libraries, Kotlin adapter and consolidated notices. These are APK file sizes,
not installed sizes or an ABI-specific estimate. Source artifacts are the
[baseline Android CI run](https://github.com/leo91000/leo-agent-manager/actions/runs/37704381521)
and [native Android CI run](https://github.com/leo91000/leo-agent-manager/actions/runs/37715411140).
The native CI run also built the minified release successfully; no release-size
comparison is claimed. No ABI filter or split changes are included in #104.

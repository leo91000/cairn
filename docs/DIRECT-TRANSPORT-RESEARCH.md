# Direct transport research: P2P with relay fallback

Verified 2026-10-06 against primary sources; unverifiable facts are flagged.

## 1. WebRTC peer connections and data channels

**Signaling.** "The signaling component is not part of" WebRTC; trickle ICE "will significantly reduce the setup time" ([webrtc.org peer connections](https://webrtc.org/getting-started/peer-connections)). Channels come from `createDataChannel()` and carry strings or binary ([webrtc.org data channels](https://webrtc.org/getting-started/data-channels)).

**Reliability and ordering.** `RTCDataChannelInit` has `ordered` (default true), `maxPacketLifeTime`, `maxRetransmits`, `protocol`, `negotiated` and `id`. Setting both `maxPacketLifeTime` and `maxRetransmits` throws a `TypeError` ([W3C WebRTC](https://www.w3.org/TR/webrtc/)). RFC 8831 §6.1 requires ordered and unordered delivery with full or partial reliability (RFC 3758/RFC 7496). It also requires SCTP over DTLS (RFC 8261), and the RFC 8831 stack runs SCTP over DTLS over ICE/UDP ([RFC 8831](https://www.rfc-editor.org/rfc/rfc8831.html)).

**Message size.**
- Without message interleaving (RFC 8260), "the sender SHOULD limit the maximum message size to 16 KB to avoid monopolization" ([RFC 8831 §6.6](https://www.rfc-editor.org/rfc/rfc8831.html)).
- The SDP `max-message-size` attribute advertises what the receiver accepts. It defaults to 64K when absent, and 0 means "any size" ([RFC 8841 §6.1](https://www.rfc-editor.org/rfc/rfc8841.html)).
- `send()` throws above `RTCSctpTransport.maxMessageSize` ([W3C WebRTC](https://www.w3.org/TR/webrtc/)); Cairn must chunk large bodies.

**ICE.**
- ICE ([RFC 8445](https://www.rfc-editor.org/rfc/rfc8445.html)) gathers host, server-reflexive, peer-reflexive and relayed (TURN) candidates, and tests candidate pairs with STUN checks.
- The controlling agent nominates the selected pair (§2.3).
- An ICE restart changes ufrag and password. It is the only way to "change the destinations of data streams", and "data can continue to be sent using existing data sessions" during the restart (§9).
- ICE-lite is only for agents that "will always be connected to the public Internet" (§2.5). A NATed installation therefore needs full ICE.
- Trickle ICE ([RFC 8838](https://www.rfc-editor.org/rfc/rfc8838.html)) runs gathering and checks in parallel and ends with an end-of-candidates indication.
- Consent freshness expires after 30 s without a valid STUN response, after which the endpoint "MUST cease transmission" ([RFC 7675](https://www.rfc-editor.org/rfc/rfc7675.html)).

**STUN and TURN.**
- STUN "is not a NAT traversal solution by itself" but a tool for learning the NAT-mapped address via XOR-MAPPED-ADDRESS ([RFC 8489](https://www.rfc-editor.org/rfc/rfc8489.html)).
- TURN allocates a relayed address. Client-to-server transport can be UDP, TCP, TLS-over-TCP or DTLS. Default ports are 3478 (UDP/TCP) and 5349 (TLS/DTLS). TCP exists because "some firewalls are configured to block UDP entirely" ([RFC 8656 §3.1, §4.1](https://www.rfc-editor.org/rfc/rfc8656.html)).
- RFC 8656 does not mention port 443. Running TURN/TLS on 443 is a deployment choice. ALPN id `stun.turn` lets TURN-over-TLS be identified in the handshake ([RFC 7443](https://www.rfc-editor.org/rfc/rfc7443.txt)).
- WebRTC endpoints MUST support TURN over TCP and over TLS, and MUST support ICE-TCP candidates ([RFC 8835 §3.4](https://www.rfc-editor.org/rfc/rfc8835.html)).

**Authentication and why signaling must be authenticated.**
- Each side's SDP carries a `fingerprint` binding the session to its DTLS key pair ([RFC 8827 §4.1](https://www.rfc-editor.org/rfc/rfc8827.html)).
- WebRTC "utilizes self-signed rather than PKI certificates" ([W3C WebRTC](https://www.w3.org/TR/webrtc/)).
- Without HTTPS signaling, "any on-path attacker can replace the DTLS-SRTP fingerprints". Even with HTTPS, "the signaling server can potentially mount a man-in-the-middle attack" unless keys are verified independently, e.g. "against a value delivered out of band" ([RFC 8827 §9.1](https://www.rfc-editor.org/rfc/rfc8827.html)).

**What a TURN server sees.**
- After DTLS, peers share keys "not known to any third-party attacker" ([RFC 8827 §4.3](https://www.rfc-editor.org/rfc/rfc8827.html)).
- TURN extracts application data and forwards it; "applications that want end-to-end security should encrypt the data" ([RFC 8656 §3](https://www.rfc-editor.org/rfc/rfc8656.html)).
- With data channels, TURN therefore relays DTLS ciphertext and still sees peer IPs, timing, volume and credentials (USERNAME/REALM) ([RFC 8656 §21.1.6](https://www.rfc-editor.org/rfc/rfc8656.html)).

**mDNS host candidates.**
- Chrome 76 (Aug 2019) replaced private host IPs with `<uuid>.local` names, except for sites holding getUserMedia permission. Testing showed "a 2% relative reduction in connection success rate" ([PSA via IETF rtcweb archive](https://mailarchive.ietf.org/arch/msg/rtcweb/X9zT_6W7r3q49ods1SiMj0HQJq0)).
- The IETF draft defining this expired in 2022 without becoming an RFC ([datatracker](https://datatracker.ietf.org/doc/draft-ietf-mmusic-mdns-ice-candidates/)).
- Under `iceTransportPolicy: relay`, mDNS candidates are prohibited ([W3C WebRTC](https://www.w3.org/TR/webrtc/)).
- Consequence: a same-LAN direct path needs the installation's ICE agent to resolve `.local` candidates. Otherwise it falls back to srflx/prflx pairs.

## 2. Steam networking

https://partner.steamgames.com/doc/features/multiplayer/networking returned a Steamworks error on 2026-10-06 and could not be verified; this section uses the SDR page and the open-source library.

- **Relay.** SDR carries traffic over Valve's backbone. Relaying protects against DoS "because IP addresses are never revealed", and traffic is "authenticated, encrypted, and rate-limited" ([SDR doc](https://partner.steamgames.com/doc/features/multiplayer/steamdatagramrelay)).
- **Authorization model.** A "game coordinator" signs short-lived tickets that authorize "a specific client to talk to a specific gameserver, for a limited amount of time". It also issues certificates; a 48-hour expiry is recommended ([SDR doc](https://partner.steamgames.com/doc/features/multiplayer/steamdatagramrelay)).
- **Signaling.** Signaling must be a persistent, push-capable channel ("such as a websocket"), best-effort only. Connection setup typically takes 4–10 messages. Later signals are sent "if routing conditions change" ([SDR doc](https://partner.steamgames.com/doc/features/multiplayer/steamdatagramrelay)). The library tolerates "dropped, duplicated, or reordered" signals ([README_P2P](https://github.com/ValveSoftware/GameNetworkingSockets/blob/master/README_P2P.md)).
- **P2P and route selection.**
  - GameNetworkingSockets (BSD-3-Clause) does NAT traversal via ICE, using either a built-in ICE client or Google WebRTC's ICE. It uses "datagram transport" only: "We don't use DTLS or WebRTC data channels" ([README_P2P](https://github.com/ValveSoftware/GameNetworkingSockets/blob/master/README_P2P.md), [repo](https://github.com/ValveSoftware/GameNetworkingSockets)).
  - Route selection scores are "on a scale of milliseconds", start from route ping, and add configurable ICE/SDR penalties ([steamnetworkingtypes.h](https://github.com/ValveSoftware/GameNetworkingSockets/blob/master/include/steam/steamnetworkingtypes.h)).
  - The library provides its own crypto (AES-GCM-256, Curve25519) and reliability "lanes" ([README](https://github.com/ValveSoftware/GameNetworkingSockets)).
  - The open-source build "does not support accessing the relay network" ([SDR doc](https://partner.steamgames.com/doc/features/multiplayer/steamdatagramrelay)).
- **Why it does not run in a browser.**
  - It is a native C++ library on raw UDP with its own non-DTLS crypto ([README_P2P](https://github.com/ValveSoftware/GameNetworkingSockets/blob/master/README_P2P.md)).
  - Web pages get no raw UDP. Direct Sockets is scoped to Isolated Web Apps ([Direct Sockets explainer](https://github.com/WICG/direct-sockets/blob/main/docs/explainer.md)).
  - Browsers' only P2P transport is WebRTC.
- **Reusable ideas:** signed time-boxed tickets, signaling over an existing push channel, RTT route scoring with relay penalties.

## 3. Rust options for the installation peer

| Library | License | Status (checked 2026-10-06) | Data channels | TURN client | ICE-TCP | Model |
|---|---|---|---|---|---|---|
| [webrtc-rs/webrtc](https://github.com/webrtc-rs/webrtc) | MIT + Apache-2.0 (dual, per README) | v0.21.0 released 2026-09-19 ([releases](https://github.com/webrtc-rs/webrtc/releases)); pre-1.0, "minor bump may carry breaking changes" | Yes | Yes (`rtc-turn`; `turns:` URLs parsed ([rtc](https://github.com/webrtc-rs/rtc))) | Yes, active/passive ([rtc](https://github.com/webrtc-rs/rtc)) | Async layer over the sans-IO `rtc` core; Tokio/smol |
| [str0m](https://github.com/algesten/str0m) | MIT OR Apache-2.0 (README) | Active (pushed 2026-10-04) | Yes | **No** (TURN is "a way of obtaining sockets", left to the user) | README: "fully support… UDP and TCP" | Sans-IO, no internal threads; you bring sockets and NIC enumeration |
| [libdatachannel](https://github.com/paullouisageneau/libdatachannel) (C++) | MPL-2.0 | v0.24.6, 2026-09-26 | Yes | Yes, via libjuice | Unclear: libjuice lists RFC 6544 yet says "Only UDP is supported" ([libjuice](https://github.com/paullouisageneau/libjuice)) | Rust bindings exist (`datachannel` crate) |

webrtc-rs ships examples for ICE restart, ICE-TCP, mDNS, and trickle ICE with relay ([examples](https://github.com/webrtc-rs/webrtc/tree/master/examples)). Not verified: whether the async gatherer actually allocates TURN over TLS end-to-end.

Browsers must support TURN/TCP/TLS themselves ([RFC 8835](https://www.rfc-editor.org/rfc/rfc8835.html)), so the installation mostly needs UDP.

## 4. Android

- **Artifact.** `io.getstream:stream-webrtc-android` (Apache-2.0 wrapper of BSD-licensed libwebrtc) is at 1.3.10, released 2025-09-11 ([Maven Central](https://repo1.maven.org/maven2/io/getstream/stream-webrtc-android/maven-metadata.xml), [repo](https://github.com/GetStream/webrtc-android)).
- **API.**
  - `DataChannel.Init` mirrors the browser options: `ordered`, `maxRetransmitTimeMs`, `maxRetransmits`, `protocol`, `negotiated`, `id`.
  - `bufferedAmount()` and `Observer.onBufferedAmountChange` support flow control ([DataChannel.java](https://webrtc.googlesource.com/src/+/refs/heads/main/sdk/android/api/org/webrtc/DataChannel.java)).
  - `PeerConnection.restartIce()` and `ContinualGatheringPolicy.GATHER_CONTINUALLY` exist ([PeerConnection.java](https://webrtc.googlesource.com/src/+/refs/heads/main/sdk/android/api/org/webrtc/PeerConnection.java)).
- **Network changes.**
  - libwebrtc's NetworkMonitor already registers `ConnectivityManager` callbacks ([NetworkMonitorAutoDetect.java](https://webrtc.googlesource.com/src/+/refs/heads/main/sdk/android/api/org/webrtc/NetworkMonitorAutoDetect.java)).
  - Apps can add `registerDefaultNetworkCallback` to trigger ICE restart; on a default-network change, connections on the old one "are forcefully terminated" ([Android docs](https://developer.android.com/develop/connectivity/network-ops/reading-network-state)).
- **APK size.** No beacon figure was found. Measured from the 1.3.10 AAR on Maven Central ([artifact](https://repo1.maven.org/maven2/io/getstream/stream-webrtc-android/1.3.10/)), `libjingle_peerconnection_so.so` is:

  | ABI | Uncompressed | Zip-compressed |
  |---|---|---|
  | arm64-v8a | 11.5 MB | 5.4 MB |
  | armeabi-v7a | 6.6 MB | 3.9 MB |

  The AAR also contains x86/x86_64 builds.

## 5. TURN servers

- **coturn** ([repo](https://github.com/coturn/coturn)): BSD-style license, release 4.18.0 on 2026-09-08.
  - RFC 8656, RFC 6062, DTLS, ALPN; UDP/TCP/TLS 1.3/DTLS listeners; Prometheus, bandwidth limits, TURN REST API ([README](https://github.com/coturn/coturn)).
  - Ports are configurable (`tls-listening-port`, default 5349), so 443 is possible.
  - `denied-peer-ip` blocks relaying into private ranges "when the turn server is sitting behind a NAT" ([turnserver.conf](https://github.com/coturn/coturn/blob/master/examples/etc/turnserver.conf)).
- **Ephemeral credentials** ([draft-uberti-behave-turn-rest-00](https://datatracker.ietf.org/doc/html/draft-uberti-behave-turn-rest-00), expired 2014 but implemented by coturn via `use-auth-secret`/`static-auth-secret`):
  - username = `expiry-timestamp:userid`;
  - password = `base64(HMAC(secret, username))`;
  - the TURN server rejects expired timestamps on ALLOCATE.
  - Expiry "does not affect existing TURN allocations", so expiry alone cannot revoke an established session.

**Application relay versus TURN.**

| | App-level relay (today) | TURN |
|---|---|---|
| Where TLS ends | At the Beacon, so content is plaintext in its memory | DTLS is end-to-end between UI and installation ([RFC 8827 §4.3](https://www.rfc-editor.org/rfc/rfc8827.html)) |
| What the operator can do | Inspect, rewrite and enforce per request | Sees ciphertext plus metadata: IPs, ports, timing, byte counts, TURN username/user id ([RFC 8656 §21.1.6](https://www.rfc-editor.org/rfc/rfc8656.html)) |
| Protocol | Any; works over WSS/443 | UDP, or TCP/TLS fallback |
| Revocation | Operator can cut any request | The service can drop the allocation but cannot police content |

## 6. Direct versus relay success rates

- **IPFS/libp2p.** A 2026 study of 4.4M attempts in IPFS reports a 70% ± 7.1% hole-punching success rate, conditional on relay reservation and address discovery succeeding. TCP and QUIC were similar ([arXiv 2604.12484](https://arxiv.org/abs/2604.12484)). Not WebRTC.
- **Ford et al. (USENIX 2005).** 82% of 380 NATs were compatible with UDP hole punching, and 64% of 286 with TCP hole punching ([paper](https://www.usenix.org/legacy/event/usenix05/tech/general/full_papers/ford/ford_html/)). Old data.
- **WebRTC relay share.** No current primary figure from a browser vendor or large operator was found. A 2017 callstats.io "30% via TURN" figure survives only second-hand ([discuss-webrtc](https://groups.google.com/g/discuss-webrtc/c/5d_EJwM6iJM)); the original 404s, so it is unverified.
- **Cairn should measure its own rates** via `getStats` candidate-pair types.

## 7. Browser constraints

- **No CA certificate needed for data channels.** They authenticate a self-signed certificate pinned by the SDP fingerprint ([W3C WebRTC](https://www.w3.org/TR/webrtc/), [RFC 8827](https://www.rfc-editor.org/rfc/rfc8827.html)).
- **Direct HTTPS/WSS would need a CA certificate.** HTTPS pages block `http://` mixed content ([Mixed Content](https://w3c.github.io/webappsec-mixed-content/)).
- **WebTransport is not a substitute.** Its `serverCertificateHashes` accepts certificates of at most two weeks' validity ([WebTransport](https://w3c.github.io/webtransport/)), but it still needs a reachable inbound port and does not traverse NAT.
- **Private Network Access never covered WebRTC.** The PNA explainer covers fetch and WebSockets only ([PNA explainer](https://github.com/WICG/private-network-access/blob/master/explainer.md)). PNA was "put on hold" and replaced by Local Network Access (LNA), a permission prompt.
- **LNA will cover WebRTC, but not yet.** Chrome states "WebRTC connections to the local network are not yet gated on the LNA permission" ([Chrome blog](https://developer.chrome.com/blog/local-network-access)).
  - The LNA explainer says WebRTC candidates with local or loopback addresses should require the permission ([LNA explainer](https://github.com/WICG/local-network-access/blob/main/explainer.md)).
  - "LNA Restrictions for WebRTC" is an Intent to Prototype (May 2025). Chromestatus shows it as "Proposed" with no milestone ([blink-dev](https://groups.google.com/a/chromium.org/g/blink-dev/c/CDy8LAs-DoA), [chromestatus](https://chromestatus.com/feature/5065884686876672)).
  - A third-party "Chrome 146" claim is unverified.

## Implications for Cairn

**Recommended building blocks:**

1. **Keep the existing WebSocket tunnel and use it for three jobs:**
   - the authenticated signaling channel (Steam-style push, best-effort);
   - the always-available fallback;
   - the bootstrap path while ICE runs.

   Run the same multiplexed request/SSE framing over the tunnel and over a data channel, so switching paths is transparent to the application.
2. **Use WebRTC data channels:**
   - native `RTCPeerConnection` in the Vue SPA;
   - libwebrtc on Android, via stream-webrtc-android or a self-built AAR;
   - webrtc-rs 0.21 in the installation, because it has a TURN client, ICE-TCP, mDNS and ICE restart built in.

   str0m is the alternative if sans-IO control is preferred (needs a custom TURN client). Use reliable ordered channels, and chunk messages at 16 KB or less, or at `maxMessageSize`.
3. **Run coturn with REST-style ephemeral credentials** minted by the Beacon:
   - short TTL;
   - TURN/UDP, TURN/TCP and TURN/TLS, with TLS on 443;
   - `denied-peer-ip` for private ranges.
4. **Authorize every connection at the installation:**
   - The service signs a short-lived grant (user, role, installation, session, expiry) and sends it with the offer through the tunnel. The installation verifies it.
   - The installation pins its own DTLS certificate and reports its fingerprint at enrollment.
   - The Android app can pin that fingerprint out of band.
   - On revocation (pushed via the tunnel) or grant expiry, the installation closes the PeerConnection. Do not rely on TURN credential expiry, which leaves allocations alive.
5. **Switching:**
   - Prefer the direct pair, then TURN, then the app relay, scored on RTT with relay penalties (GNS-style).
   - Trigger ICE restart on Android default-network callbacks and on `iceconnectionstatechange` failures.
   - Fall back to the tunnel immediately while ICE recovers. Keep requests idempotent or resumable across paths.

**What the Beacon sees:**

| Transport | Visible to the Beacon |
|---|---|
| Direct | Signaling only: SDP with candidate IPs, ICE credentials and fingerprints, plus auth metadata. No payload. |
| TURN (if operated by Cairn) | The above, plus 5-tuples, timing, byte counts and the TURN username. Payload is DTLS ciphertext. |
| App relay | Full plaintext content. |

**Main risks:**

- **The service can still impersonate an installation.** It relays signaling, so it can substitute fingerprints ([RFC 8827 §9.1](https://www.rfc-editor.org/rfc/rfc8827.html)). The web SPA is served by that same service, so end-to-end protection against the service is limited on web. Native pinning helps on Android.
- **Same-LAN connections:** Chrome LNA prompts once shipped, and mDNS resolution in the Rust agent.
- **Library maturity:** webrtc-rs is pre-1.0, and stream-webrtc-android has not released in over a year.
- **Cost:** ~5.4 MB compressed native code per Android ABI; TURN bandwidth and abuse surface.
- **No reliable public success-rate baseline,** so build in telemetry from the start.
- **IP disclosure:** a direct path reveals each peer's IP to the other. Offer a relay-only policy where that matters ([RFC 8827 §6.4](https://www.rfc-editor.org/rfc/rfc8827.html)).

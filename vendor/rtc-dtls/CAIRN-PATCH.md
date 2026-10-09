# Cairn's pinned DTLS retransmission fix

Source: the unmodified crates.io `rtc-dtls` **0.21.0** archive, SHA-256
`b05fe638f13e60cf3ef8e1608d0e8d36cfb740cce4514e8d6662887eb6c17ff9`.
Upstream source revision: `c8321cab79ae745aad973d62034ec84d301e893c`,
[`webrtc-rs/rtc`](https://github.com/webrtc-rs/rtc/tree/c8321cab79ae745aad973d62034ec84d301e893c/rtc-dtls).
The MIT and Apache licenses come from that same revision.

Only `src/handshaker.rs` and `src/endpoint.rs` differ from that archive:

- `wait()` keeps the retransmission deadline when the peer's flight is incomplete
  or repeated. It cancels the deadline only after a complete next flight advances
  the state. Previously the lost flight could be left without any retransmission.
- An established endpoint still processes handshake repeats. The sender of the
  final flight resends its cached flight when parsing recognizes the preceding
  flight. The finished FSM returns when stable; the last-flight receiver does not
  answer a repeated final flight, avoiding ping-pong and duplicate completion.
  Previously local completion suppressed processing, so a lost final server
  flight could leave the remote peer waiting forever while local DTLS looked
  connected and SCTP could not open.

Both behaviors are required by the WAITING/FINISHED states in
[RFC 6347 section 4.2.4](https://www.rfc-editor.org/rfc/rfc6347.html#section-4.2.4).
No dependency version, crypto configuration, replay detector, certificate/grant
verification, revocation rule, network-loss profile or application deadline is
changed. This local patch avoids upgrading the whole WebRTC stack to an alpha.

The dependency is excluded from Cairn's workspace/formatting to preserve the
upstream source. The public DTLS endpoint regression tests live at
`crates/installation/tests/direct_dtls_loss.rs`; run them through the credential
isolating backend launcher:

```sh
CARGO_PROFILE_DEV_DEBUG=0 CARGO_BUILD_JOBS=4 pnpm test:backend --test direct_dtls_loss
```

Remove this patch when an approved pinned release contains the fix, retaining
these public-interface tests. The authenticated namespace bench verifies the
production browser, Rust client and installation paths separately (#141).

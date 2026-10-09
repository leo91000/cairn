# Cairn's pinned DTLS retransmission fix

Source: the unmodified crates.io `rtc-dtls` **0.21.0** archive, SHA-256
`b05fe638f13e60cf3ef8e1608d0e8d36cfb740cce4514e8d6662887eb6c17ff9`.
Upstream source revision: `c8321cab79ae745aad973d62034ec84d301e893c`,
[`webrtc-rs/rtc`](https://github.com/webrtc-rs/rtc/tree/c8321cab79ae745aad973d62034ec84d301e893c/rtc-dtls).
The MIT and Apache licenses come from that same revision.

Only `src/handshaker.rs` differs from that archive: `wait()` keeps the existing
retransmission deadline when the peer's flight is incomplete or repeated. It
cancels the deadline only when parsing a complete next flight advances the state.
Previously, receiving a repeated previous flight removed the deadline before
parsing failed to advance. The lost flight was never retransmitted, leaving ICE
connected but DTLS/DataChannel stuck until Cairn's unchanged negotiation deadline.

This is required by the WAITING state in
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

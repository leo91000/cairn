# Cairn's pinned DTLS retransmission fix

Source: the crates.io `rtc-dtls` **0.21.0** archive, SHA-256
`b05fe638f13e60cf3ef8e1608d0e8d36cfb740cce4514e8d6662887eb6c17ff9`, the
checksum recorded for this crate in Cargo.lock before the patch.
Upstream source revision: `c8321cab79ae745aad973d62034ec84d301e893c`,
[`webrtc-rs/rtc`](https://github.com/webrtc-rs/rtc/tree/c8321cab79ae745aad973d62034ec84d301e893c/rtc-dtls).
The MIT and Apache licenses come from that same revision.

## Provenance

Compared with that archive, this directory:

- patches four source files: `src/handshaker.rs`, `src/endpoint.rs`,
  `src/conn/mod.rs` and `src/handshake/handshake_cache.rs`;
- adds `LICENSE-MIT`, `LICENSE-APACHE` and this file;
- omits the archive's packaging metadata, `.cargo_vcs_info.json` and
  `Cargo.lock`. Cargo ignores a path dependency's lockfile: Cairn's Cargo.lock
  resolves every dependency.

Every other file, including `Cargo.toml`, is byte-identical. To verify:

```sh
curl -sSfLO https://static.crates.io/crates/rtc-dtls/rtc-dtls-0.21.0.crate
sha256sum rtc-dtls-0.21.0.crate
tar xzf rtc-dtls-0.21.0.crate
diff -rq rtc-dtls-0.21.0 vendor/rtc-dtls
```

`diff` must list exactly the differences above.

## Behavior

- `wait()` keeps the retransmission deadline when the peer's flight is incomplete
  or repeated. It cancels the deadline only after a complete next flight advances
  the state. Previously the lost flight could be left without any retransmission.
- An established endpoint answers its peer repeating the final flight. The sender
  of the final flight resends its cached flight once for each repeated, authenticated
  Finished identical to the one already received. Previously local completion
  suppressed processing, so a lost final server flight could leave the remote peer
  waiting forever while local DTLS looked connected and SCTP could not open.
- After completion, no other handshake record reaches the fragment buffer or the
  handshake cache, so the concluded verification never changes. The receiver of the
  final flight never parses again: it ignores repeats, which avoids ping-pong,
  duplicate completion and transcript re-verification. Replayed records are discarded
  before they can trigger a retransmission.

Retransmissions of the final flight are bounded by the peer: each needs a fresh,
authenticated record of its Finished, and the peer stops repeating its flight after
its own `maximum_retransmit_number`. The repeat must match the received Finished
byte for byte, alone and unfragmented in its record, as rtc-dtls and browsers send
this 12-byte message. A differently packed repeat is ignored rather than trusted.

These behaviors are required by the WAITING/FINISHED states in
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

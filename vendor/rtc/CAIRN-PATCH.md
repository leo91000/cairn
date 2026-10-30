# Cairn's pinned DataChannel teardown fix

Source: crates.io `rtc` **0.21.0**, archive SHA-256
`a0c5e4a8791908797789d63efd013d6987b1f87cce4255b94343d1fa9c69a81f`,
recorded in Cairn's lockfile before this patch. Upstream revision:
`c8321cab79ae745aad973d62034ec84d301e893c` in
[`webrtc-rs/rtc`](https://github.com/webrtc-rs/rtc/tree/c8321cab79ae745aad973d62034ec84d301e893c/rtc).

Compared with the archive, this directory patches only
`src/peer_connection/handler/sctp.rs`, `src/peer_connection/handler/mod.rs` and
`src/peer_connection/handler/datachannel.rs` and `src/data_channel/mod.rs`,
adds this file, replaces the local `.gitignore`,
and omits packaging metadata (`.cargo*`) and the archive's `Cargo.lock`.
The upstream MIT/Apache licenses are preserved. Every other file is
byte-identical to the archive. Cairn's root lockfile pins dependencies.

## Behavior

Closing a DataChannel starts an asynchronous SCTP stream reset. Previously the
handler immediately reported `SCTPStreamClosed` as well, releasing the wire
stream ID while SCTP and the peer were still closing that stream. A new channel
could reuse it and inherit the old channel's teardown: HTTP responses became
empty, or channel opening failed and the gateway fell back to Beacon.

Keep the ID reserved until SCTP reports its actual removal through
`AssociationLost`. That existing event path emits the final close and releases
the channel. The companion `rtc-sctp` patch waits for both reset directions before reporting
removal, and prevents duplicate or deferred reset processing from resetting a
replacement stream after legitimate ID reuse.

Timeout processing forwards association events through the same lifecycle path
as inbound packets, so exhausted reset retries notify the control channel even
if no later SCTP datagram arrives. Closed streams are removed from the parked
read set. Completing a deferred reset after the final buffered message is EOF;
it preserves the collected message batch instead of dropping it as an error.

Pending SCTP lifecycle events preserve per-channel read ordering. Final messages
still use the old handle, while a replacement DCEP OPEN applies the matching
pending close before accepting a new handle on that wire stream. The public
OnClose event waits for that handle's already-queued messages to be read; other
channels and Open events continue to progress. Normal event polling also forwards
reset completion produced by application reads, without needing another packet.
This covers final reset ACK, old payload and replacement DCEP sharing one batch.

No dependency version, connection deadline, certificate check, authorization,
HTTP route restriction or direct/relay selection policy changes.

A remote reset can close the SCTP write half before its final acknowledgement.
Expose Closing and reject new binary/text sends without charging buffer credit,
including while a final close event waits behind received messages. Previously
received messages still drain before OnClose, without an early public OnClosing
event. Already closed channels remain terminal when a late close is deferred.
Association shutdown also prevents new writes before SHUTDOWN-COMPLETE arrives.
Deferred close preserves a connecting channel's queued ACK and Open-before-Message ordering.

The regular workspace regression uses the same multithreaded Tokio runtime as
the gateway and repeatedly opens/closes eight concurrent HTTP connections,
asserting every traversal response and the absence of new relay connections:

```sh
pnpm test:backend -p cairn-beacon --test node_channel -- --test-threads=4
```

The dependency's library tests include deterministic regressions at the RTC
handler boundary for timeout-only notifications, reset while a response is
parked by backpressure, and close/replacement ordering in one read batch. Seed
its ignored standalone lockfile from the root to retain the pinned production
protocol graph; its upstream-only signal dev dependency is pinned separately:

```sh
cp Cargo.lock vendor/rtc/Cargo.lock
cargo metadata --manifest-path vendor/rtc/Cargo.toml --format-version 1 \
  --config 'patch.crates-io.rtc-sctp.path="vendor/rtc-sctp"' \
  --config 'patch.crates-io.rtc-dtls.path="vendor/rtc-dtls"' >/dev/null
cargo update --manifest-path vendor/rtc/Cargo.toml -p rtc-signal --precise 0.21.0 \
  --config 'patch.crates-io.rtc-sctp.path="vendor/rtc-sctp"' \
  --config 'patch.crates-io.rtc-dtls.path="vendor/rtc-dtls"'
pnpm test:backend --manifest-path vendor/rtc/Cargo.toml -p rtc --lib \
  --config 'patch.crates-io.rtc-sctp.path="vendor/rtc-sctp"' \
  --config 'patch.crates-io.rtc-dtls.path="vendor/rtc-dtls"'
```

Verify provenance by extracting the pinned `.crate` and comparing its tree with
this directory; only the files listed above may differ. Remove this patch when an
approved pinned upstream release contains the fix, retaining the gateway test.

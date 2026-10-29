# Cairn's pinned DataChannel teardown fix

Source: crates.io `rtc` **0.21.0**, archive SHA-256
`a0c5e4a8791908797789d63efd013d6987b1f87cce4255b94343d1fa9c69a81f`,
recorded in Cairn's lockfile before this patch. Upstream revision:
`c8321cab79ae745aad973d62034ec84d301e893c` in
[`webrtc-rs/rtc`](https://github.com/webrtc-rs/rtc/tree/c8321cab79ae745aad973d62034ec84d301e893c/rtc).

Compared with the archive, this directory patches only
`src/peer_connection/handler/sctp.rs`, adds this file, replaces the local `.gitignore`,
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

No dependency version, connection deadline, certificate check, authorization,
HTTP route restriction or direct/relay selection policy changes.

The regular workspace regression uses the same multithreaded Tokio runtime as
the gateway and repeatedly opens/closes eight concurrent HTTP connections,
asserting every traversal response and the absence of new relay connections:

```sh
pnpm test:backend -p cairn-beacon --test node_channel -- --test-threads=4
```

Verify provenance by extracting the pinned `.crate` and comparing its tree with
this directory; only the files listed above may differ. Remove this patch when an
approved pinned upstream release contains the fix, retaining the gateway test.

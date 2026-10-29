# Cairn's pinned SCTP stream-reset fix

Source: crates.io `rtc-sctp` **0.21.0**, archive SHA-256
`161e3eb55695d4ce7492a690b6a27dc43bfeee43fd5d4af0423de1ace49456c1`,
recorded in Cairn's lockfile before this patch. Upstream revision:
`c8321cab79ae745aad973d62034ec84d301e893c` in
[`webrtc-rs/rtc`](https://github.com/webrtc-rs/rtc/tree/c8321cab79ae745aad973d62034ec84d301e893c/rtc-sctp).

Compared with the archive, this directory patches only
`src/association/mod.rs`, `src/association/association_test.rs` and
`src/endpoint/endpoint_test.rs`, adds the
MIT/Apache licenses from the same upstream revision, this file and `.gitignore`,
and omits packaging metadata (`.cargo*`) and the archive's `Cargo.lock`.
All other files are byte-identical to the archive. Cairn's root lockfile pins
the dependency graph; a standalone test lockfile is local and ignored.

## Behavior

An incoming stream reset may be retransmitted, or its completion may be deferred
until an unread response has been drained. Previously either path could apply a
reset again after its wire stream ID had been reused for a new DataChannel.
The replacement channel then closed during opening or lost its HTTP response.

A stream remains reserved until its incoming reset completes and the peer
acknowledges its outgoing reset, explicitly or through a reciprocal reset.
An In Progress reply keeps retransmission active. Reciprocal resets start the
reconfiguration timer even when they originated on the read path, preserving an
existing deadline while unrelated streams exchange traffic. Concurrent
local and reciprocal closure share one outgoing request instead of issuing a
second reset against the same stream. Replacement DATA arriving before the final
ACK is not acknowledged against the old generation; reliable SCTP retransmits it
after teardown completes instead of losing its DCEP message.

A terminal reset refusal ends the unusable SCTP association and notifies its
channels, rather than retrying a rejected request or reusing an unsafe ID.
Already established closed associations reject new streams and late packets.
The gateway and manager also end a node peer when its control DataChannel closes,
so SCTP loss cannot leave a stale peer advertised as connected while ICE remains up.

Keep the last completed incoming reset sequence for each wire stream ID across
reuse, using SCTP serial-number comparisons. A repeated or older reset is still
answered but cannot reset the replacement stream or wait for its unrelated data.
A newer reset still closes it. The history has at most 65,536 entries per
association because SCTP stream IDs are 16 bits; it does not grow with the number
of HTTP connections. Deferred multi-stream resets retain their pending work
without repeating a reset already completed for another stream in the request.

This preserves the pinned stack, transport limits, certificates, authorization,
HTTP route restrictions and direct/relay deadlines. It narrows reset application;
it does not implement the other reconfiguration extensions of
[RFC 6525](https://www.rfc-editor.org/rfc/rfc6525.html#section-5.2).

## Validation

The concurrent gateway regression is part of the regular workspace backend suite:

```sh
pnpm test:backend -p cairn-beacon --test node_channel -- --test-threads=4
```

The standalone dependency tests also cover both reset directions, In Progress
responses, terminal refusals, retry deadlines under unrelated traffic,
duplicate/older resets, serial-number wraparound and deferred reset completion
after stream reuse. An endpoint test delays the final reset ACK until replacement
DATA has arrived and verifies eventual byte-for-byte delivery after retransmission. The upstream delivery regressions still assert
intact payloads and final stream removal, allowing the reciprocal ACK round trip
before asserting that the wire stream was released:

```sh
cargo generate-lockfile --manifest-path vendor/rtc-sctp/Cargo.toml
pnpm test:backend --manifest-path vendor/rtc-sctp/Cargo.toml -p rtc-sctp
```

Verify provenance by extracting the pinned `.crate` and comparing its tree with
this directory; only the files listed above may differ. Remove this patch when an
approved pinned upstream release includes the repair, retaining the gateway test.

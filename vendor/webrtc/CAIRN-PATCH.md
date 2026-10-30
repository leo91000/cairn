# Cairn's pinned local DataChannel registration fix

Source: crates.io `webrtc` **0.21.0**, archive SHA-256
`b3cc7db94f14a20c9401202f7300f38071dcac4446177b40e12dc2df34b2256b`,
recorded in Cairn's lockfile before this patch. Upstream revision:
[`4be2e3d6f99787ac438d28f9f704d343b1aa1498`](https://github.com/webrtc-rs/webrtc/tree/4be2e3d6f99787ac438d28f9f704d343b1aa1498).

Compared with the archive, this directory patches only
`src/peer_connection/mod.rs`, `src/peer_connection/driver.rs` and
`src/data_channel/mod.rs`, adds this file,
replaces the local `.gitignore`, and omits packaging metadata (`.cargo*`) and the
archive's `Cargo.lock`. The upstream MIT/Apache licenses are preserved. Every
other file is byte-identical to the archive. Cairn's root lockfile pins the
production dependency graph.

## Behavior

The asynchronous wrapper previously released the RTC core lock after creating a
local channel, then registered the caller's event receiver. An already-running
driver could complete DCEP during that gap and treat the local channel as a new
incoming channel. Its `OnOpen` went to a different receiver; the caller timed out
despite its channel already being Open, and the gateway unnecessarily used Beacon.

Acquire the event registry and core locks before creating the channel, then
register its recipient before releasing either lock. There is no await between
creation and registration. Cancellation while waiting for either lock leaves the
core unchanged. Driver event dispatch releases the core lock before accessing
the recipient registry, so the critical section does not invert a nested lock order.

This preserves channel handles, protocol messages, certificate checks,
authorization, direct/relay policy, buffering and connection deadlines. It does
not upgrade the transport stack.

A bounded sender checks terminal channel readiness before waiting for credit
or returning buffer-full. A channel retained for SCTP reset completion cannot
strand blocked binary/text sends after closure. Already admitted bytes and the
receive lifecycle remain unchanged; unbounded sends retain their existing fast
path and reach the synchronous RTC write guard.

## Validation

Two deterministic library regressions cover a driver Open event racing recipient
registration and cancellation while registration is blocked. They use the actual
creation and event-dispatch methods, controlled polling and existing mutexes;
neither test needs a sleep, packet loss or wall-clock timeout. Both fail against
the original source. The enclosing Beacon regression also checks 128 short-lived
direct connections in batches of eight, their exact traversal rejection and no
added relay connections:

```sh
pnpm test:backend -p cairn-beacon --test node_channel -- --test-threads=4
```

In-memory library regressions also park bounded `writable`, binary and text
senders behind unacknowledged DATA, close the still-registered channel and
assert closure on their next credit notification. Nonblocking sends report
closure before buffer-full without charging additional bytes. The fixture
establishes the real RTC cores by exchanging byte buffers and virtual deadlines;
it opens no sockets and uses no wall-clock sleeps.

Seed the ignored standalone lockfile from the root and retain the pinned local
protocol patches; pin the upstream-only signal development dependency separately:

```sh
cp Cargo.lock vendor/webrtc/Cargo.lock
cargo metadata --manifest-path vendor/webrtc/Cargo.toml --format-version 1 \
  --config 'patch.crates-io.rtc.path="vendor/rtc"' \
  --config 'patch.crates-io.rtc-sctp.path="vendor/rtc-sctp"' \
  --config 'patch.crates-io.rtc-dtls.path="vendor/rtc-dtls"' >/dev/null
cargo update --manifest-path vendor/webrtc/Cargo.toml -p rtc-signal --precise 0.21.0 \
  --config 'patch.crates-io.rtc.path="vendor/rtc"' \
  --config 'patch.crates-io.rtc-sctp.path="vendor/rtc-sctp"' \
  --config 'patch.crates-io.rtc-dtls.path="vendor/rtc-dtls"'
pnpm test:backend --manifest-path vendor/webrtc/Cargo.toml -p webrtc --lib \
  --config 'patch.crates-io.rtc.path="vendor/rtc"' \
  --config 'patch.crates-io.rtc-sctp.path="vendor/rtc-sctp"' \
  --config 'patch.crates-io.rtc-dtls.path="vendor/rtc-dtls"'
```

Verify provenance by extracting the pinned `.crate` and comparing its tree with
this directory; only the files listed above may differ. Remove this patch when an
approved pinned upstream release contains the fix, retaining the regressions.

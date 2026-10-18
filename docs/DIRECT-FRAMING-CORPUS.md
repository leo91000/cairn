# Direct framing corpus: inventory and decision

Architecture campaign [C6 / #268](https://github.com/leo91000/cairn/issues/268),
part of [#240](https://github.com/leo91000/cairn/issues/240).
Inventory date: 2026-10-18. Reference main:
`e59e6de5cf8de2b26fdcd9908a05fd3ab06114f9`.

## Discriminating trial

Before designing a shared fixture format, inspect the three implementations and
their existing behavioral tests. Identify which inputs and expected outcomes
could actually be shared, and which assertions require a transport-specific
harness. This is a source inventory, not a runtime interoperability measurement.

The campaign audit reference is
`19c47868801840c494d24285b89e2e71a6fc8e20`. Comparing it with the main reference
above produces no diff for the following implementations and test suites:

- [Rust envelope decoder](../crates/protocol/src/data_channel.rs) and
  [account-capacity tests](../crates/protocol/tests/member_capacity.rs).
- [Rust real-peer tests](../crates/beacon/tests/direct_peer.rs).
- [Web channel](../apps/web/src/direct-channel.ts) and
  [transport tests](../apps/web/tests/direct-transport.test.ts).
- [Android channel](../apps/android/app/src/main/java/build/cairn/app/data/DirectTransport.kt)
  and [transport tests](../apps/android/app/src/test/java/build/cairn/app/data/DirectTransportTest.kt).

In particular, Rust's framing coverage is not embedded in `data_channel.rs`:
the real-peer suite and protocol capacity suite must be included in the inventory.
The application protocol remains v4. All three use a 13-byte envelope (version
byte 1, then network-order transfer ID, total and offset), packets of at most
16,384 bytes, and a maximum frame size of 10,732,204 bytes.

## Existing scenarios

Names below identify existing tests at the reference commit. A dash means that
this inventory did not find a corresponding assertion in the named suites;
it does not mean that the implementation accepts that input.

| Scenario | Rust | Web | Android | Shareable part and remaining assertions |
| --- | --- | --- | --- | --- |
| Large frame split into ordered packets | `fragmented_request_and_response_keep_body_bytes_and_verified_capabilities` (real peer) | `reassembles fragmented UTF-8 responses and discards an abandoned partial response` | `large application responses use existing ordered 16 KiB fragments` | Packet boundaries and reconstructed bytes overlap. Rust also verifies identity/capability replacement on a real request; clients verify response delivery and direct routing. Payload sizes and directions differ. |
| Declared frame exceeds the limit | `excessive_fragment_declaration_closes_only_direct_and_relay_remains_usable` (real peer) | `falls back on an oversized fragment without keeping the interrupted read pending` | `oversized or excessive incomplete transfers close direct without blocking relay` (`frame`) | A small packet declaring an excessive total is common to all three. Rust and Android use limit + 1; web uses 20,000,000. Closing direct and preserving relay require the existing harnesses. |
| Abort incomplete transfer (`total=offset=0`, no payload) | `direct_stream_uses_the_same_body_with_credit_and_cancel_during_fragmentation`; `reassembly_waiters_survive_abort_close_and_expiry_of_the_holder` (real peer) | Reassembly test above; `fragments large bodies at 16 KiB, waits for bufferedAmount, and abandons a cancelled transfer` | `canceling a fragmented send aborts only its transfer and peer remains usable` | The abort bytes overlap. Web tests reception and emission; Android's named test asserts emission, not reception. Rust additionally verifies reservation release and waiter progress. |
| Packet exceeds 16 KiB; too many incomplete transfers | — | — | Oversized-transfer test above (`packet`, `assemblies`) | Existing explicit invalid-input assertions are Android-specific in these suites. Other decoders also enforce limits; this is not evidence of common test coverage. |
| Invalid finite-response body, status or headers | — | — | `invalid finite response closes direct and safe read recovers on relay` | Response validation plus safe read recovery. Rust's typed deserialization and the clients' dispatch rules are different test interfaces. |
| Reassembly refusal before dispatch | `a_busy_member_upload_is_rejected_and_drained_before_its_next_small_frame` (protocol); `interleaved_uploads_on_one_peer_reject_only_the_excess_transfer` (real peer) | `replays a fragmented mutation refused before dispatch through relay without closing direct` | `reassembly refusal sends a mutation on relay and preserves the healthy peer` | Shared refusal marker, but complementary roles: Rust produces it without dispatch; clients consume it to authorize replay. A JSON example cannot replace either guarantee. |
| Streaming, credits, cancellation and route recovery | `direct_stream_uses_the_same_body_with_credit_and_cancel_during_fragmentation` (real peer) | `moves live streams between routes using only the accepted cursor and history` | `direct stream falls back at accepted cursor without missing or duplicate events` | Application exchanges overlap, but ordered actions, credits and accepted cursors matter. Static valid/invalid JSON alone cannot express the tested behavior. |
| Account reservations, owner headroom, progress and expiry | Both protocol capacity tests; real-peer capacity/expiry tests | — | — | Installation policy, not a common client framing rule. |

## Decision

Defer the shared executable corpus while the protocol remains stable, as allowed
by #268. The inventory does find useful overlap; it does **not** establish that
there are no common scenarios. The best first candidate is the oversized-total
packet, followed by ordered reassembly. Adopting these today would add three
fixture loaders and their integration while retaining the network and client
recovery tests that carry the substantive assertions. No current protocol
change requires updating those inputs together, so the maintenance saving has
not been demonstrated. This is a qualitative maintenance decision, not a
benchmark result or a case-count threshold.

A JSON-frame-only corpus would also miss the shared malformed-envelope case:
the invalid total is rejected before any complete JSON exists. A future trial
must distinguish packet bytes and sequences from the decoded application frame.
Do not silently treat the three decoders as identical validators. For example,
web requires finite-response status 200–599, Android initially accepts integer
status 100–999, and Rust deserializes a `u16` before application handling. Web
uses fatal UTF-8 decoding; Android constructs a UTF-8 string with replacement.
These observations are source-level differences, not adjudicated protocol bugs
or permission to change current acceptance rules in this documentation change.

Reopen on the next behavioral change to `data_channel.rs` or an intentional
framing/validation change in a client. Start with the affected row above and
demonstrate that one input and expectation can replace independently maintained
cases across the three suites. Keep directional expectations explicit: Rust
receives requests and clients receive responses; Rust's decoder is also used by
the real-peer test client to read responses. Retain tests for identity, relay
recovery, reservation policy, real network negotiation and native device behavior.
If the trial cannot simplify maintenance, abandon the shared corpus rather than
creating an abstraction solely for uniformity.

## Reproducing and validating the inventory

From the repository root, with the reference commit available:

```sh
git diff --exit-code 19c47868801840c494d24285b89e2e71a6fc8e20 e59e6de5cf8de2b26fdcd9908a05fd3ab06114f9 -- \
  crates/protocol/src/data_channel.rs crates/protocol/src/lib.rs \
  crates/protocol/tests/member_capacity.rs crates/beacon/tests/direct_peer.rs \
  apps/web/src/direct-channel.ts apps/web/tests/direct-transport.test.ts \
  apps/android/app/src/main/java/build/cairn/app/data/DirectTransport.kt \
  apps/android/app/src/test/java/build/cairn/app/data/DirectTransportTest.kt
rg -n 'FrameDecoder|EncodedFrame' crates --glob '*.rs'
rg -n '^async fn |^fn ' crates/beacon/tests/direct_peer.rs crates/protocol/tests/member_capacity.rs
rg -n '^it\(' apps/web/tests/direct-transport.test.ts
rg -n 'fun `' apps/android/app/src/test/java/build/cairn/app/data/DirectTransportTest.kt
```

Read the named test bodies and the decoder paths, rather than deriving coverage
from test names alone. This change only records the inventory and decision; it
changes no executable code, fixture, UI, storage or persisted format. Local
validation is the reference comparison, source/link checks and `git diff --check`.
No new runtime, browser or device result is claimed. The applicable GitHub
quality workflow still has to pass on the PR head; Android's standalone workflow
is path-filtered and does not apply to these documentation paths.

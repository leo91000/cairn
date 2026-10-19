# Bounded direct/relay qualification (#308)

The aggregate `packet-loss` router selects one outgoing IPv4 datagram in each
five-packet block on each side. It includes TCP, STUN, DTLS and every peer. Its
fixed seed does not bound consecutive losses of one SCTP DATA fragment. Reliable,
ordered delivery therefore does not imply completion before the independent
heartbeat or request deadline. See ADR-0033 and `NETWORK-BENCH.md`.

Qualification must distinguish direct delivery under bounded recoverable DATA
loss from safe relay recovery when a required exchange exceeds the existing
budget. Unrestricted routing ratios are measurements, not delivery guarantees.
No product deadline, reliability setting, dispatcher or authorization rule changes.

## Fixtures and proof

`network_qualification` is a dedicated synthetic installation executable. It
uses the real installation service, HTTP router, authenticated relay and direct
peer. The normal `cairn` launcher supplies no overrides. `run_with_factory` is an
explicit dependency-construction seam; no production fault environment variable
or HTTP endpoint is introduced. Public-alias announcement still observes actual
successful ICE sends through `AliasRuntime`.

The fixture supplies a transparent crypto provider and socket runtime. Every
crypto operation, result and encrypted byte is delegated unchanged to the pinned
provider. Successful AEAD sealing correlates SCTP chunk metadata with the exact
ciphertext/tag digest. The runtime parses DTLS records and suppresses only an
exact registered match. It parses SCTP DATA and PPID 53; DCEP and SCTP control
traffic do not count as application DATA loss. A DTLS content-type-23 record
alone is never sufficient evidence. Unsupported crypto layouts, malformed data
or missing correlation fail qualification. The fixture does not decrypt browser
UDP or claim incoming-browser DATA interception.

A private Unix socket acknowledges each plan before the operation begins. Each
plan names the next fixture peer, or the current established peer. A fragment
is selected by its DATA metadata within that eligible peer, never by encrypted
size, a process-global packet ordinal or a timing guess. The fixture drops whole
UDP datagrams, so `droppedDataChunks` inventories every binary DATA chunk removed
with the target. It does not claim that a datagram contains only one chunk.
Successful-pass counters advance only after the runtime accepts the whole send.
No keys, payloads, grants, SDP, cookies, CSRF or connection identities enter proof
output. Counts, fixed phase names, sizes and monotonic timings are bounded.

The qualification browser document imports the shipped `InstallationTransport`
and `liveConnection` consumer. API, signaling, DTLS and authentication are real;
the document is fixture presentation. The existing native application journeys,
namespace topology and security tests remain separate mandatory coverage.

The browser owner and invited authenticated Rust member use separate accounts
to stay within the existing eight-peer per-account authorization limit. Each
negotiation remains fresh; no admission quota or lease policy changes.

## Gates

| Case | Injected fault | Required result |
| --- | --- | --- |
| Browser fragmented response | Drop a selected binary DATA fragment's initial transmission and first retry; pass subsequent emissions. | Accepted direct live batch, complete direct read and POST, intact synthetic content, exactly one stored message; two proven DATA drops and a subsequent accepted send; no heartbeat fallback. |
| Rust request, three fresh peers | The explicit helper option sends a fragmented safe GET, with initial selected DATA and first retry dropped at the client. | 3/3 authenticated direct responses with the exact marker; proven drops and later accepted target transmission. |
| Rust fragmented response, three fresh peers | Apply the same bounded plan at the installation. | 3/3 authenticated direct responses with exact marker and fail-closed DATA proof. |
| First application, zero messages | Allow the authenticated channel to open, then suppress installation application DATA. | Exact heartbeat diagnosis at the unchanged health boundary, completed relay POST and accepted relay batch, visible expected content and exactly one stored client submission; no completed direct operation before fallback. |
| First application, headers only | Pass complete successful stream-start headers but starve binary DATA carrying the batch and finite response. | The same recovery assertions; passed headers are positively evidenced and never counted as an accepted batch. |
| Established application | First complete a direct read and accept a direct batch. Arm DATA starvation during an idempotent POST, stream and distinct mutation without an idempotency key. | Heartbeat relay recovery with the original message ID; one stored submission; exact accepted-cursor resume with no missing/duplicate synthetic event; non-idempotent mutation fails visibly and is never replayed. |
| Rust response starvation | Open early, queue the real GET, observe real dispatch/response production, suppress response DATA through the unchanged outer deadline. | Exactly `response/timedOut=true`, zero completed direct response, actual authenticated relay read after failure, intact marker. This does not permit response failures in direct-delivery cases. |
| Opening starvation | Suppress correlated SCTP INIT/INIT-ACK transmissions after DTLS authentication through the unchanged opening budget. | Browser closes within the unchanged opening budget without open/heartbeat; authenticated relay POST and accepted batch work during and after failure, exactly once. Rust reports exactly `ice/timedOut=true`, no open or dispatched request, authenticated relay body. Both peers prove handshake drops. |

`network_direct_client` retains its one outer 30-second direct timeout. Phase
records show remaining budget at channel-open, request-queued, first message,
complete response and deadline expiry, followed by transport state before local
close. A virtual-time regression demonstrates that `phase=response` may have
only one second of remaining budget after a late opening. This does not establish
the packet chain of the historical F2 or F5 CI failures.

The SCTP integration test uses the pinned public `rtc::sctp` endpoints and virtual
`Instant`. Dropping one necessary fragment at 0, 1, 3 and 7 seconds blocks ordered
delivery until 15 seconds while later DATA is forwarded. Dropping only the first
two attempts recovers at 3 seconds, preserving exact bytes, ordering and exactly
once delivery. These are protocol mechanism tests, not authenticated fixtures.

## Red/green evidence and duration

Run the controlled starvation fixture with `CAIRN_QUALIFICATION_OLD_ORACLE=1` to
execute the unchanged completed-direct prerequisite after independently proving
authenticated relay rescue. It must fail. Run the same plan under the bounded
contract to require recovery. `CAIRN_QUALIFICATION_NEGATIVE=1` disables the bounded
drop plan while leaving its proof requirements strict; the direct delivery alone
must not make that run pass. The repository SCTP negative control suppresses
all later selected-fragment retransmissions:

```sh
CAIRN_SCTP_NEGATIVE_CONTROL=1 pnpm test:backend -p cairn-installation --test sctp_loss_contract recoverable_loss_requires_exact_ordered_delivery
```

That command must fail the three-second exact-delivery assertion; ordinary tests
run without the flag. These flags are consumed only by test code.

All scenarios run without scenario retries. Record local wall time, child CPU,
RSS limitations, host resources and artifact provenance. Separate one-time cold
build/setup cost from fixture runtime and compare the complete network job with
its unchanged CI capacity limit. A corrected gate is never reported as a pass of
the original oracle.

Stress reports include per-router aggregate IPv4 packet/byte counts, dropped and
forwarded counts, and elapsed time. Counters snapshot only completed router
decisions at normal fixture shutdown; an interrupted in-progress send is excluded.
Forwarded counts mean the raw socket accepted a packet; application acceptance
is checked separately. The counters do not identify SCTP DATA or establish a
delivery bound. The router
writes once on teardown, keeping its original per-block decisions and packet
stream intact. Missing or inconsistent snapshots fail qualification. The existing
125-packet-per-side regression also verifies25 actual drops and100 forwards.

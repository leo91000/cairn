# Direct / relay network bench

Ticket [#102](https://github.com/leo91000/cairn/issues/102),
[ADR-0033](adr/0033-direct-connection-with-relay-fallback.md).

## Run locally

Linux, `iproute2` (including `ss`), `iptables`, `util-linux`, Python 3, `/dev/net/tun`, the pinned
Rust/Node/pnpm runtimes and passwordless sudo are required. No KVM,
VM, real email provider or agent credentials are needed. Docker is needed only
for disposable Postgres and the published STUN-port check. Use an empty disposable
Postgres database, never a deployed service. The usual browser fixture launches
both real binaries and uses an in-memory email adapter. Agent workers are disabled.

```sh
pnpm install --frozen-lockfile
CARGO_PROFILE_DEV_DEBUG=0 CARGO_BUILD_JOBS=4 cargo build --locked --workspace --bin cairn --bin cairn-beacon --example network_direct_client --example network_stun_server
pnpm build
pnpm exec playwright install --with-deps chromium
docker run -d --name cairn-network-postgres -e POSTGRES_USER=cairn -e POSTGRES_PASSWORD=test-only -e POSTGRES_DB=cairn_beacon_test -p 127.0.0.1:5432:5432 postgres:17-alpine
export CAIRN_BEACON_TEST_DATABASE_URL=postgres://cairn:test-only@127.0.0.1:5432/cairn_beacon_test
python3 tests/network_probe_test.py
python3 tests/network_bench_test.py
python3 tests/stun_docker_test.py
```

Each command launches a complete authenticated Chromium session and a Rust client:

```sh
python3 tests/network-bench.py same-lan --output test-results/network/same-lan.json
python3 tests/network-bench.py mdns-only-client --output test-results/network/mdns-only-client.json
python3 tests/network-bench.py nat-client --output test-results/network/nat-client.json
python3 tests/network-bench.py nat-installation --output test-results/network/nat-installation.json
python3 tests/network-bench.py nat-both --output test-results/network/nat-both.json
python3 tests/network-bench.py udp-blocked --output test-results/network/udp-blocked.json
python3 tests/network-bench.py symmetric-nat --output test-results/network/symmetric-nat.json
python3 tests/network-bench.py symmetric-client --output test-results/network/symmetric-client.json
python3 tests/network-bench.py same-server --output test-results/network/same-server.json
python3 tests/network-bench.py network-change --output test-results/network/network-change.json
python3 tests/network-bench.py packet-loss --output test-results/network/packet-loss.json
```

Run sequentially; the bench holds a lock and refuses overlapping runs. It uses
198.18.102.0/24 and 198.18.103.0/30 for its private internet and 10.102.1.0/24 and
10.102.2.0/24 for participants. The only host change is a temporary veth address;
all forwarding, NAT and filtering rules live in disposable namespaces. Processes
running Chromium, Cairn and the Rust session client use the caller's UID; only
network configuration and the TUN packet router require root. Finally blocks
stop processes, remove namespaces/links and delete private fixture data. Stop the
disposable database when finished: `docker rm -f cairn-network-postgres`.

Add `--probe-only` to any command for a fast network-only check without Postgres
or browsers. The Rust probe is compiled directly with the pinned `rustc`; it uses
only the standard library and never serves installation data.

## Network assertions

| Scenario | Actual topology / constraint |
| --- | --- |
| same-lan | Browser and installation share the same LAN bridge/subnet; numeric host candidates. |
| mdns-only-client | Chromium default mDNS privacy on an untranslated client, with client STUN UDP blocked after topology probes; the installation retains STUN, NAT and inbound filtering. Ignored client `.local` candidates leave no usable numeric hole-punch path; the browser uses relay and the numeric Rust client uses direct. |
| nat-client | Stateful source NAT and unsolicited inbound filtering on the client router. |
| nat-installation | The same constraint on the installation router. |
| nat-both | Independent NAT/filtering on both sides. |
| udp-blocked | Every forwarded UDP datagram is dropped on both routers; TCP remains usable. |
| symmetric-nat | Both routers allocate UDP source ports per flow/destination and deny unsolicited inbound packets. The two probe destinations have distinct fixed mappings; arbitrary UDP destinations use fresh random port allocation. |
| symmetric-client | Destination-dependent client NAT faces an installation with ordinary Docker-like NAT/filtering; relay expected, no installation port published. |
| same-server | Beacon STUN behind a directly published DNAT port on the installation host; external client source preserved, installation hairpin reports a gateway; explicit public-IP alias restores the authorized direct route. |
| network-change | The client moves to a new source address during a live stream; old-address sockets are closed. |
| packet-loss | A userspace TUN router drops every fifth outgoing IPv4 packet on both sides, including TCP; no random seed or optional netfilter/netem module. |

UDP probes send fixed sequence markers to two diagnostic listeners. Reports show
sent/received counts, whether the source was translated and the number of observed
mappings on both sides. The UDP probe discards delayed replies from earlier
sequences until the current reply or its deadline. The beacon Rust Binding responder (via its diagnostic example launcher) listens at
198.18.102.1:3478, before the host-facing masquerade; mapped addresses therefore
identify each actual NAT router (198.18.102.2/.3), rather than a shared host
address. Tests assert these reflexive addresses. NAT routers drop unsolicited UDP to their own ports as well as forwarded UDP,
and private host candidates behind NAT have no internet route; otherwise
conntrack can create artificial reverse-flow mappings before hole punching.
The CLI tests assert these effects, including repeated
exact packet-loss counts. These diagnostic listeners cannot access installation
content and are not a WebRTC peer or an anonymous local installation interface.

## Route and switching evidence

The JSON report contains the topology probes, expected route and successful
application observations: browser send, Rust read, and stream resume after a
network change. Each includes `route`, `operation` and monotonic `elapsedMs`.
Browser evidence comes from the web transport's `cairn-transport-observation`
events after successful API responses and accepted live batches, followed by
visible conversation data. Finite relay responses use `x-cairn-transport: relay`;
direct responses arrive through the real authorized DataChannel. The observer
contains no payload or credentials. The default-route indicator alone is not
evidence of a successful application request.

Rust first verifies the same conversation marker through HTTPS, then requests a
signed grant and negotiates with the real installation DataChannel. It records
`direct` only after the API response arrives on that channel and a selected UDP
ICE pair is observed (`candidatePair.local` / `.remote`). Both clients expect
**direct** for LAN, ordinary NAT, network change, same-server and packet loss;
UDP blocked, symmetric NAT and symmetric client expect **relay**. In the
additional mDNS-only client case, the browser expects relay and Rust expects direct. Neither uses
a fixed route label. `--expect-route` and `--expect-rust-route` override the
scenario defaults; a mismatch fails the bench.

With Rust direct expected, packet-loss qualification samples three independent
Rust negotiations. At least
one must deliver its read directly; all three on relay fail qualification.
Individual relay reads are accepted only with the fixed diagnostic
`directFailure: { phase: "ice", timedOut: true }`, since the deterministic router
also drops handshake packets and ICE retains its strict 30-second limit.
Other failures and all other scenario route expectations remain strict. Every
sample's actual route stays in the report, including successful fallback reads.

Packet-loss reports also include `directNegotiations`: `attempts`, `direct`,
`relay`, and `ratio` (direct responses divided by attempts, from 0 to 1). These
counts describe only the three independent Rust negotiations, using their actual
application-response routes; browser bootstrap and stream observations are
excluded. The summary is written before qualification assertions, so an all-relay
failure still records its zero direct ratio. Compare this field in
`direct-relay-network-evidence/packet-loss.json` across CI runs. The requirement
for at least one direct read and the exact ICE-timeout diagnostic for relay reads
are unchanged. Explicit relay expectations also remain strict.

The numeric qualification browser is launched with Chromium's
`--disable-features=WebRtcHideLocalIpsWithMdns` option so that host-candidate ICE
paths can be tested explicitly. The production web does not set this option or
request media permission. The additional `mdns-only-client` scenario keeps Chromium's
default privacy policy with client STUN denied against an inbound-filtered installation; following #117,
ignored `.local` candidates and no usable numeric alternative must leave application traffic on the authenticated relay.
Both policies and actual operation routes are recorded in the same bench report.
The mDNS-only case also records and asserts that every real browser ICE candidate
is a `.local` host candidate, with no numeric reflexive alternative.

Same-LAN also exercises a browser-facing local-network permission denial:
CDP denies Chromium's `local-network-access` permission, and an init script
injects `NotAllowedError` at `RTCPeerConnection.createOffer`. The real
installation, relay, API reads and live stream remain active; observations must
show relay and the page must have no unhandled error. This checks the web's
refusal path, not an actual native permission prompt: the fixture's origin is
loopback, outside the public-to-local origin transition described by
[Chrome's LNA guidance](https://developer.chrome.com/blog/local-network-access).
That guidance also still lists WebRTC gating as a limitation. Native prompt
qualification on a public HTTPS origin remains #105.

The network-change scenario measures the real direct-to-relay stream recovery,
then checks reestablishment of direct and a Rust read. Same-LAN also blocks all
client UDP during a browser message send: the message must arrive once, the
live stream must resume on relay within 25 seconds, and the message list must
contain one client submission. Restoring UDP must reestablish direct. An beacon
restart then replaces the signing key; fresh negotiation and subsequent logout
revocation are exercised without restarting the browser or installation.

Same-LAN and UDP-blocked exercise member removal, logout (including another tab
of the same session) and detachment during active browser streams on the selected
route. The removed member receives no further accepted batches, while the owner's
stream continues. Access checks deny the removed/detached account immediately;
logout redirects the session's pages to sign-in. Reports retain the checked
revocation scopes. Anonymous requests and installation loopback access still
fail. Qualification across Android and production hosts remains #105.

`udp-blocked` demonstrates that the existing relay remains usable even when every
UDP probe fails. The existing `journeys-beacon-relay` and Rust relay/security
suites remain the regression checks for owner/member roles, withdrawal, session
expiry/revocation, detach, rotation and definitive revocation. The bench does not
change these contracts, local authentication, storage or central persistence.

## CI

The separate **direct-relay network bench (no KVM)** job downloads the same real
binaries/frontend as browser journeys, checks network effects, then runs all ten
scenarios sequentially without retries. JSON reports and Playwright failure diagnostics are retained as
`direct-relay-network-evidence`.
Evidence contains no
cookies, claim codes, machine credentials or conversation exports.

## Residual packet-loss measurements (#126)

Seven local runs on 2026-10-08 compared the merged #119 implementation with
the #126 candidate using the unchanged `packet-loss` scenario and assertions. The
base tree is `4427d79b47499c322862c99df07161140d90ca90`, shared by #119 head
`43106bd` and merge `7c842df`. Base binaries/frontend came from #119's CI artifacts;
the candidate binaries were built locally with the pinned toolchain. Each run
used a fresh authenticated owner, installation, browser and Linux namespaces.

| Build | Run | Result | Largest outbound application frame (JSON bytes) | Fragmented frames / pending transfers |
| --- | --- | --- | --- | --- |
| #119 | 1 | Pass; browser send + title: 9,582 ms | Not instrumented | Not instrumented |
| #119 | 2 | Pass; browser send + title: 15,984 ms | 855 | 0 / 0 |
| #119 | 3 | Message POST not observed within 10 s (line 165) | 685 | 0 / 0 |
| #119 | 4 | Route stayed relay at 35 s (line 161) | 0 | 0 / 0 |
| #126 | 1 | Message POST succeeded; title absent at 10 s (line 166) | 855 | 0 / 0 |
| #126 | 2 | Message POST not observed within 10 s (line 165) | 855 | 0 / 0 |
| #126 | 3 | Route stayed relay at 35 s (line 161) | 0 | 0 / 0 |

A passive browser probe counted binary DataChannel sends from their 13-byte
headers: declared total, offset and completion. It did not change packets,
deadlines, network effects or retries. Only aggregate sizes/counts and the public
transport observations' method, route and elapsed time were retained; no payloads
or credentials were recorded. Run #126/1 reproduces the exact #119 title failure
after the successful messages POST (9,515 ms after document initialization),
with 11 packets, no fragmentation and no outstanding transfer.

These results exclude reassembly waiting/rejection as the cause of that title
failure: every request fits one packet, reservations are released synchronously
on decoding, and this single-owner session has no competing member reservation.
The #126 member-only admission paths are not used. Runs that never establish
direct have not sent any application packet at all. The measurements establish
this narrower conclusion; they do not establish general reliability under loss
or diagnose the remaining ICE/POST/view latency. The 10-second POST/title and
35-second route assertions are unchanged. Qualification remains #105.

## Published beacon STUN port

`python3 tests/stun_docker_test.py` publishes a disposable Binding-only Python
fixture on a Docker UDP port using a pinned image digest. An external Linux
namespace at `198.18.104.2` queries the host's `198.18.104.1` address.
The test waits for the UDP listener before probing; XOR-MAPPED-ADDRESS must
retain the external client's address and source port (`localPort`). This exercises the
actual Docker DNAT path; a gateway address from a userland proxy fails the check.
The fixture mounts only the diagnostic script read-only, removes its container
and namespace in `finally`, and saves `test-results/network/docker-stun.json`.
The namespace bench now uses this same beacon Rust responder, rather than
only the Python fixture. Its report identifies `stunResponder: beacon-rust`.
Real UDP integration tests also cover receive-error recovery and IPv4/IPv6 mappings.
The authenticated bench deliberately sets `CAIRN_DIRECT_STUN_URLS=""` to verify
that the installation uses the beacon STUN source and still connects directly.
The production host must repeat source-preservation and firewall validation
before release (#113/#105); this ticket performs no deployment or host firewall
change. Hairpin behavior and the optional port-preserving public-IP alias are
covered by the authenticated `same-server` scenario above.


Qualification #105 must additionally probe UDP 3478 from an **external IPv6**
host through the production Docker publication, comparing the full IPv6 source
and source port with XOR-MAPPED-ADDRESS. An IPv6 Docker userland proxy can replace
the source with a gateway (or translate to IPv4); IPv4 DNAT evidence does not
prove IPv6 preservation. Do not publish AAAA/STUN IPv6 readiness until this
separate check succeeds. The local namespace/Docker bench remains IPv4 evidence.

## Android native adapter (#104)

`--android` reuses this bench's Beacon, authenticated installation,
STUN responder, UDP probes, router filtering and cleanup. It requires a booted
real emulator, `adb`, built debug/application-test APKs and the same disposable
Postgres database. The launcher isolates live agent credentials. For example:

```sh
cairn-android emulator start 30 --accept-licenses --aosp
mise exec -- apps/android/gradlew -p android assembleDebug assembleDebugAndroidTest
ANDROID_SERIAL=emulator-5580 python3 tests/network-bench.py same-lan --android --output test-results/android-direct/same-lan.json
ANDROID_SERIAL=emulator-5580 python3 tests/network-bench.py udp-blocked --android --output test-results/android-direct/udp-blocked.json
ANDROID_SERIAL=emulator-5580 python3 tests/network-bench.py network-change --android --output test-results/android-direct/network-change.json
cairn-android emulator stop
```

The emulator runs outside the Linux client namespace. Temporary host routes
connect it to the bench installation topology; they disappear with the owned
veth. Host firewall policy stays untouched. The LAN case observes a real native
application response, selected-installation UI indicator, renewal before the
180-second lease expiry and logout revocation during a live stream. The blocked
case drops installation UDP and observes continued relay reads throughout the
30-second ICE deadline. The network case actually disables/enables emulator
Wi-Fi, observes `ConnectivityManager` move to cellular and back, and requires
relay fallback followed by fresh authorizations and successful direct responses.
This qualifies Android switching; it does not claim emulator traffic originated
inside the client namespace or that the synthetic mobile link is a carrier NAT.

JSON reports contain actual successful response routes and device SDK/ABI.
Sibling `.instrumentation.txt` files contain JUnit results. Credentials are
written only to a private fixture file/app storage and removed in cleanup; no
SDP, cookies, authorization IDs or logcat are uploaded. The **Android direct-relay
device bench** CI job runs all three cases on API 36, fails on any failed case
and retains only these sanitized reports.

## Packet-loss investigation (#141)

The reported CI baseline is 8 failed jobs out of 29 (27.6%). Runs
[37830006941](https://github.com/leo91000/cairn/actions/runs/37830006941) and
[37907266093](https://github.com/leo91000/cairn/actions/runs/37907266093) fail the
initial `direct` assertion under 35 s; other historical failures concern title
arrival or three Rust negotiations remaining on relay. These are distinct
failure modes, not eight interchangeable ICE failures.

### DTLS retransmission

Eight fresh, local promotion-only runs reproduced the exact initial failure
once (12.5%). ICE connected at 1,936 ms, DTLS stayed connecting, no DataChannel
message was sent, and the unchanged negotiation deadline closed the peer at
30,661 ms. The route remained relay throughout the 35-second assertion.

Pinned `rtc-dtls` 0.21.0 cleared its retransmission deadline before parsing a
complete next flight. Partial or repeated previous flights could leave a lost
certificate flight without any retransmission. A temporary, targeted probe of
the real authenticated bench dropped a 659-byte client certificate datagram:
it was never resent before the fix; afterwards retransmissions occurred at
+1,000 and +2,001 ms and the channel opened at 4,210 ms. The probe was removed.

A second defect affects the final server flight. The server marks itself complete
when sending it, but an established endpoint no longer processed the client's
repeated preceding flight, nor resent its cached final flight. A public endpoint
regression loses that final flight: the server considers DTLS connected while
the client remains waiting. It failed in 0.41 s before correction. This explains
how local DTLS can look connected while the remote peer cannot open SCTP, matching
the separate CI diagnostics `DTLS Connected / SCTP Connecting`.

A temporary header-only loss router confirmed this in the authenticated bench:
discard the first encrypted Finished on each client-side flow. Before final-flight
recovery, all three Rust reads used relay after 30 s, with DTLS Connected and SCTP
Connecting; their final record was emitted only once. After recovery, the same
probe passed with Rust direct 3/3 and the cached final record retransmitted.
Chromium already retransmitted its final flight in the control. No ciphertext,
credentials or payload was logged, only record sizes/epochs/sequences and timings.
The targeted loss was removed before ordinary qualification.

Three public DTLS endpoint tests use virtual time: lost server flight/repeated
ClientHello, lost Certificate/repeated server flight, and lost final server
flight/repeated client Finished. The first two failed before the timer fix;
the third failed before final-flight recovery. All three pass after the fixes
in 0.07 s, including application delivery and application replay rejection:

```sh
CARGO_PROFILE_DEV_DEBUG=0 CARGO_BUILD_JOBS=4 pnpm test:backend --test direct_dtls_loss
```

The fixes retain the timer until a complete next flight advances the handshake,
and answer a recognized repeat with the cached final flight after local
completion. They preserve crypto configuration, replay protection, grants,
fingerprints, revocation and every product deadline. The exact 0.21.0 archive
and licenses are retained; only two upstream source files differ. See
[`vendor/rtc-dtls/CAIRN-PATCH.md`](../vendor/rtc-dtls/CAIRN-PATCH.md).
No WebRTC or other dependency version is upgraded.

### Response observation and valid heartbeat fallback

The first qualification matrix, [37947373445](https://github.com/leo91000/cairn/actions/runs/37947373445),
passed three benches and failed one on observing a POST after 10 s, after
promotion had succeeded. Six jobs were cancelled for diagnosis. A reduced
prefix reproduced the same assertion failure in 1/20 natural-loss runs:
POST observed **direct at 18,142 ms**, title at 19,056 ms, same open channel.
No forced loss or phase shift was used. Same-lan/NAT sends took 149–386 ms.
The existing client contract already permits 35 s per response and 30 s for
fragment reassembly; the shorter observer could expire before valid recovery.

The next main CI, [37952457396](https://github.com/leo91000/cairn/actions/runs/37952457396),
promoted at 1,661 ms, but sent a message on relay at 13.865 s and closed the
channel at 16,666 ms. Rust used direct 3/3. Its qualification matrix was
interrupted after three successes. Reproduction CI
[37956987153](https://github.com/leo91000/cairn/actions/runs/37956987153)
recorded 41 independent packet-loss cases and five failures: two title observers,
one relayed browser send, one interrupted chat creation, and one 0/3 Rust result.
These measurements are not full eleven-scenario qualification runs.

Thirty reduced local sends passed. A temporary loss-phase sweep then reproduced
the relayed send and identified its cause: heartbeat sent at 13,342 ms, cancelled
at 18,343 ms for its five-second response deadline, followed by relay. This is
the documented health policy, not the initial negotiation defect. Another phase
received the title after the original 10 s window, with a direct send at
13,656 ms. Phase-controlled probes do not count as rates of the original profile
and are removed after diagnosis.

The packet-loss scenario therefore prepares its empty conversation on the
**authenticated relay**, as it already does bootstrap reads. Chat creation is
not safely replayable after transport loss. It then requires the browser's
initial direct promotion under the unchanged 35 s assertion and sends the
idempotent message under loss. POST and title observers allow 35 s only for this
profile. A relayed message is accepted only when the browser diagnostic confirms
**heartbeat timeout** and earlier successful authenticated direct traffic.
Unknown fallbacks still fail. Three independent Rust negotiations still require
at least one direct response; opening-timeout fallback remains the only accepted
Rust relay reason. Other scenarios retain their original observer windows and
route expectations. All anonymous-access, identity, revocation and security
checks remain in place.

No heartbeat delay changes: it is still sent every 10 s with 5 s for its response
after transmission. A metadata-only `cairn-direct-heartbeat-timeout` event records
that existing decision, and a public web regression verifies the timeout, relay
selection and preservation of the idempotent message body.

### Reproduction and qualification

The normal router still discards exactly every fifth outgoing IPv4 packet on
both sides. There are no retries inside a scenario. Run the complete authenticated
seam with the original network topology:

```sh
CARGO_PROFILE_DEV_DEBUG=0 CARGO_BUILD_JOBS=4 python3 tests/network-bench.py packet-loss --output test-results/network/packet-loss.json
```

Each run writes a bounded sibling `packet-loss.transport.json`, even on failure.
It contains ICE/DTLS/DataChannel states, counters, monotonic timings and the
heartbeat-timeout label; never candidates, SDP, addresses, certificates,
credentials or application content. Rust `directDiagnostics` separates ICE,
DTLS and SCTP states: the historical phase name `ice` covers the entire wait for
DataChannel opening, rather than identifying ICE as the cause.

After the first timer fix alone, ten full local packet-loss benches passed
consecutively, with Rust direct 15/30; valid fallback does not mean every
negotiation opens direct. Those intermediate-source results do not qualify the
final head. Exact-head full CI, every qualification run/rerun and measured final
rates are recorded with the separate Standards/Spec reviews in
[PR #142](https://github.com/leo91000/cairn/pull/142).
Qualification remains #105; this change performs no merge, release or deployment.

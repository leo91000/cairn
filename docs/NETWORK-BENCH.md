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

The reported CI baseline is 8 failed network jobs out of 29 completed runs
(27.6%). Run [37907266093](https://github.com/leo91000/leo-agent-manager/actions/runs/37907266093)
shows the browser remaining `relay` for the unchanged 35-second assertion.
The eight local, fresh-session promotion-only runs reproduced that exact symptom
once (1/8, 12.5%), before any message send or Rust read. The temporary
promotion-only mode was removed after diagnosis. The original full baseline run
passed with one of three Rust negotiations direct; a pass therefore does not
mean every negotiation used direct.

The failing browser's ICE pair was connected at 1,936 ms, but DTLS stayed
`connecting`, the DataChannel never opened, and no application message was sent.
Cairn's unchanged negotiation deadline closed it at 30,661 ms. This is a product
handshake failure, rather than a reason to relax the route assertion.

The pinned `rtc-dtls` 0.21.0 `wait()` cleared its retransmission timer as soon as
it received handshake traffic, even when a partial or repeated previous flight
could not advance the handshake. If a certificate flight was lost and the peer
repeated its previous flight, retransmission stopped permanently. A targeted
loss in the real authenticated bench dropped a 659-byte client certificate
packet: before the fix it was sent only once and the route assertion failed;
after the fix it was retransmitted at 1,000 ms and 2,001 ms, and the DataChannel
opened at 4,210 ms. This extra targeted loss was removed; the normal router still
drops exactly every fifth outgoing packet on each side.

The public DTLS endpoint regressions simulate time and lose one whole flight,
then deliver the peer's repeated previous flight before the affected timer.
Both client/server cases failed deterministically before the change (0.08 s),
then passed (0.07 s), including application delivery and replay rejection:

```sh
CARGO_PROFILE_DEV_DEBUG=0 CARGO_BUILD_JOBS=4 pnpm test:backend --test direct_dtls_loss
```

The fix retains the timer until a complete next flight advances the handshake.
It changes no deadline, signed grant, certificate verification, revocation,
replay detector or fallback condition. Only this patch is applied to the original
0.21.0 source, whose provenance and licenses are retained in
[`vendor/rtc-dtls/CAIRN-PATCH.md`](../vendor/rtc-dtls/CAIRN-PATCH.md); no WebRTC
version or other dependency pin is upgraded.

Reproduce with the original full authenticated seam, without retries or changed
route expectations:

```sh
CARGO_PROFILE_DEV_DEBUG=0 CARGO_BUILD_JOBS=4 python3 tests/network-bench.py packet-loss --output test-results/network/packet-loss.json
```

Each run writes a sibling `packet-loss.transport.json`, even on an assertion
failure. It contains bounded browser ICE/DTLS/DataChannel state transitions and
counters with monotonic timings, never candidates, SDP, URLs, certificates,
credentials or application content. Rust observations include `directDiagnostics`
with ICE, DTLS and SCTP states before closure. The existing `directFailure.phase`
label `ice` denotes the wait for DataChannel opening; these separate states
avoid mistaking every opening timeout for an ICE failure. Existing route/fallback
assertions remain unchanged.

Exact-head CI, all post-fix rates, every qualification run/attempt and the final
Standards/Spec review are recorded in [PR #142](https://github.com/leo91000/leo-agent-manager/pull/142).
Release qualification remains #105; this change performs no release or deployment.

### POST observation under packet loss (#141)

The first qualification matrix of the DTLS fix (`94bb9fb`, run
[37947373445](https://github.com/leo91000/leo-agent-manager/actions/runs/37947373445))
passed three full benches and failed one on the POST observation, after the
browser had already established direct at 3,539 ms. The remaining six jobs were
cancelled for diagnosis. This was not the 35-second direct-promotion failure.

A reduced authenticated browser prefix, using the original packet-loss profile
and original 10-second assertions, reproduced this second symptom once in 20
local runs. That red run observed the POST **directly at 18,142 ms**, then its
heading at 19,056 ms, on the same connected ICE/DTLS/DataChannel. No phase shift
or forced loss was used in that run. For comparison, the unchanged CI's same-lan
and NAT-only browser sends completed in 149–386 ms. The two direct requests
therefore completed within the existing product contract; the 10-second test
observation failed earlier.

Sending the first message creates a conversation and then posts its message:
these are two sequential requests. `DirectChannel` and
[the direct-connection contract](DIRECT-CONNECTION.md) give **each response 35
seconds**, including transport recovery. Only the packet-loss bench now observes
the conversation response and then the message response with that same per-request
window. Other scenarios retain their original observation timeout. Product
response and negotiation deadlines, the initial 35-second route assertion, title
assertion, packet-loss profile, direct-route checks and security assertions stay
unchanged. This is a justified test-contract correction in addition to the
independent DTLS retransmission product fix above.

# Direct / relay network bench

Ticket [#102](https://github.com/leo91000/cairn/issues/102),
[ADR-0033](adr/0033-direct-connection-with-relay-fallback.md).

## Run locally

Linux, `iproute2` (including `ss`), `iptables`, `conntrack`, `util-linux`, Python 3, `/dev/net/tun`, the pinned
Rust/Node/pnpm runtimes and passwordless sudo are required. No KVM,
VM, real email provider or agent credentials are needed. Docker is needed only
for disposable Postgres and the published STUN-port check. Use an empty disposable
Postgres database, never a deployed service. The usual browser fixture launches
both real binaries and uses an in-memory email adapter. Agent workers are disabled.

```sh
pnpm install --frozen-lockfile
CARGO_PROFILE_DEV_DEBUG=0 CARGO_BUILD_JOBS=4 cargo build --locked --workspace --bin cairn --bin cairn-beacon --example network_direct_client --example network_qualification --example network_stun_server
pnpm build
pnpm exec vite build --config tests/network-qualification.vite.ts
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
python3 tests/network-bench.py qualification --output test-results/network/qualification.json
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
| same-server | Beacon STUN behind a directly published DNAT port on the installation host; external client source preserved, installation hairpin reports a gateway; explicit public-IP alias withheld until outbound ICE prepares the mapping; delayed browser trickle makes an eager alias receive the first check and remap Docker MASQUERADE. |
| network-change | The client moves to a new source address during a live stream; old-address sockets are closed. |
| packet-loss | A userspace TUN router drops one outgoing IPv4 packet per five-packet block on both sides, including TCP. A fixed, side-specific BLAKE2s seed varies its position; no optional netfilter/netem module. |

UDP probes send fixed sequence markers to two diagnostic listeners. Reports show
sent/received counts, whether the source was translated and the number of observed
mappings on both sides. The UDP probe discards delayed replies from earlier
sequences until the current reply or its deadline. The beacon Rust Binding responder (via its diagnostic example launcher) listens at
198.18.102.1:3478, before the host-facing masquerade; mapped addresses therefore
identify each actual NAT router (198.18.102.2/.3), rather than a shared host
address. Tests assert these reflexive addresses. NAT routers drop unsolicited UDP to their own ports as well as forwarded UDP,
except the same-server installation host: its INPUT path confirms unsolicited
checks to unpublished host ports, reproducing Docker conntrack tuple collisions.
There is still no DNAT or incoming forwarding to the installation. The
`sameServerMappings` evidence records local/public source ports and first-seen
timings throughout negotiation, so a failed tuple expiring before the route
assertion cannot hide a remap. All observed peer mappings must preserve ports.
Private host candidates behind NAT have no internet route; otherwise
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
**direct** for LAN, ordinary NAT, network change and same-server. Packet loss
uses the qualification contract below;
UDP blocked, symmetric NAT and symmetric client expect **relay**. In the
additional mDNS-only client case, the browser expects relay and Rust expects direct. Neither uses
a fixed route label. `--expect-route` and `--expect-rust-route` override the
scenario defaults; a mismatch fails the bench.

Packet-loss reports include `directNegotiations`: `attempts`, `direct`, `relay`
and `ratio`, based on the actual application-response routes of three independent
Rust negotiations. Browser bootstrap and stream observations are excluded. These
are measurements, not a minimum-direct-ratio gate. The
[current qualification contract](#current-qualification-contract-308) determines
which direct deliveries and diagnosed fallbacks must succeed.

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
binaries/frontend as browser journeys, checks network effects, then runs the eleven original scenarios and bounded qualification
sequentially without retries. JSON reports and Playwright failure diagnostics are retained as
`direct-relay-network-evidence`.
Evidence contains no
cookies, claim codes, machine credentials or conversation exports.

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

## Diagnosing loss and fallback

The deterministic router drops one outgoing IPv4 packet per five-packet block on
each side, including TCP. It varies the dropped position using the fixed seed;
aggregate 20% loss does not guarantee delivery of a particular SCTP exchange.
Use [bounded qualification](NETWORK-QUALIFICATION.md) to separate recoverable DATA
loss from starvation that must end in safe relay recovery.

Packet-loss setup creates its conversation on the authenticated relay because
chat creation is not safely replayable. The idempotent message is then sent under
loss. Heartbeats keep their 10-second interval and five-second response deadline.
Fallback must have an observed heartbeat timeout, opening deadline or failed peer;
unknown early failures remain errors. Product timers are not extended by the bench.

A bounded sibling `packet-loss.transport.json` records ICE/DTLS/DataChannel
states, counters, monotonic timings and heartbeat-timeout labels, even on failure.
It excludes candidates, SDP, addresses, certificates, credentials and content.
Rust `directDiagnostics` separates ICE, DTLS and SCTP; the historical phase label
`ice` covers the whole DataChannel-opening wait and does not identify the cause.
The patched DTLS retransmission and final-flight rules are documented in
[the vendored patch provenance](../vendor/rtc-dtls/CAIRN-PATCH.md). Run their public
endpoint regressions through the isolated backend launcher:

```sh
CARGO_PROFILE_DEV_DEBUG=0 CARGO_BUILD_JOBS=4 pnpm test:backend --test direct_dtls_loss
```

## Current qualification contract (#308)

Use [bounded qualification](NETWORK-QUALIFICATION.md). The original 20% router,
seed, topologies, security checks and product deadlines remain unchanged.
`packet-loss` retains authenticated recovery, accepted batches, exactly one stored
message and diagnosed failure/deadline checks. Promotion and three Rust routing
outcomes (including 0/3 direct) are measured. `oldDeliveryPrerequisites` records
the old oracle's individual predicates; `historicalDeliveryOracle` evaluates
their conjunction with `executed: false`. It is a comparison of delivery
prerequisites, not an execution or pass of the old oracle. A revised pass is never
an old-oracle pass.
A Rust stress fallback must prove expiration of the unchanged shared 30-second
budget, with no HTTP refusal; its phase is reported, not universally permitted
in bounded direct-delivery cases. Browser fallback requires an observed heartbeat,
opening deadline or failed peer. Unknown early failures remain red.

Mandatory `qualification` supplies the replacement positive coverage: actual
recoverable DATA loss must deliver direct, three fresh Rust request and response
pairs must all complete direct, and named starvation plans must recover precisely
and safely. Its injector fails closed on unarmed, unobserved or uncorrelated loss.
No retry or larger product/job deadline is introduced.

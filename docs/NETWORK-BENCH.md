# Direct / relay network bench

Ticket [#102](https://github.com/leo91000/leo-agent-manager/issues/102),
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
CARGO_PROFILE_DEV_DEBUG=0 CARGO_BUILD_JOBS=4 cargo build --locked --workspace --bin leo --bin leo-official --example network_direct_client
pnpm build
pnpm exec playwright install --with-deps chromium
docker run -d --name leo-network-postgres -e POSTGRES_USER=leo -e POSTGRES_PASSWORD=test-only -e POSTGRES_DB=leo_official_test -p 127.0.0.1:5432:5432 postgres:17-alpine
export LEO_OFFICIAL_TEST_DATABASE_URL=postgres://leo:test-only@127.0.0.1:5432/leo_official_test
python3 tests/network_probe_test.py
python3 tests/network_bench_test.py
python3 tests/stun_docker_test.py
```

Each command launches a complete authenticated Chromium session and a Rust client:

```sh
python3 tests/network-bench.py same-lan --output test-results/network/same-lan.json
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
running Chromium, Leo and the Rust session client use the caller's UID; only
network configuration and the TUN packet router require root. Finally blocks
stop processes, remove namespaces/links and delete private fixture data. Stop the
disposable database when finished: `docker rm -f leo-network-postgres`.

Add `--probe-only` to any command for a fast network-only check without Postgres
or browsers. The Rust probe is compiled directly with the pinned `rustc`; it uses
only the standard library and never serves installation data.

## Network assertions

| Scenario | Actual topology / constraint |
| --- | --- |
| same-lan | Browser and installation share the same LAN bridge/subnet. |
| nat-client | Stateful source NAT and unsolicited inbound filtering on the client router. |
| nat-installation | The same constraint on the installation router. |
| nat-both | Independent NAT/filtering on both sides. |
| udp-blocked | Every forwarded UDP datagram is dropped on both routers; TCP remains usable. |
| symmetric-nat | Both routers allocate UDP source ports per flow/destination and deny unsolicited inbound packets. The two probe destinations have distinct fixed mappings; arbitrary UDP destinations use fresh random port allocation. |
| symmetric-client | Destination-dependent client NAT faces an installation with ordinary Docker-like NAT/filtering; relay expected, no installation port published. |
| same-server | Official STUN behind a directly published DNAT port on the installation host; external client source preserved, installation hairpin reports a gateway; explicit public-IP alias restores the authorized direct route. |
| network-change | The client moves to a new source address during a live stream; old-address sockets are closed. |
| packet-loss | A userspace TUN router drops every fifth outgoing IPv4 packet on both sides, including TCP; no random seed or optional netfilter/netem module. |

UDP probes send fixed sequence markers to two diagnostic listeners. Reports show
sent/received counts, whether the source was translated and the number of observed
mappings on both sides. The UDP probe discards delayed replies from earlier
sequences until the current reply or its deadline. A STUN fixture listens at
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
Browser evidence comes from the web transport's `leo-transport-observation`
events after successful API responses and accepted live batches, followed by
visible conversation data. Finite relay responses use `x-leo-transport: relay`;
direct responses arrive through the real authorized DataChannel. The observer
contains no payload or credentials. The default-route indicator alone is not
evidence of a successful application request.

Rust first verifies the same conversation marker through HTTPS, then requests a
signed grant and negotiates with the real installation DataChannel. It records
`direct` only after the API response arrives on that channel and a selected UDP
ICE pair is observed (`candidatePair.local` / `.remote`). Both clients expect
**direct** for LAN, ordinary NAT, network change, same-server and packet loss;
UDP blocked, symmetric NAT and symmetric client expect **relay**. Neither uses
a fixed route label. `--expect-route` and `--expect-rust-route` override the
scenario defaults; a mismatch fails the bench.

The network-change scenario measures the real direct-to-relay stream recovery,
then checks reestablishment of direct and a Rust read. Same-LAN also blocks all
client UDP during a browser message send: the message must arrive once, the
live stream must resume on relay within 25 seconds, and the message list must
contain one client submission. Restoring UDP must reestablish direct. An official
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
UDP probe fails. The existing `journeys-official-relay` and Rust relay/security
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

## Published official STUN port

`python3 tests/stun_docker_test.py` publishes a disposable Binding-only Python
fixture on a Docker UDP port using a pinned image digest. An external Linux
namespace at `198.18.104.2` queries the host's `198.18.104.1` address.
The test waits for the UDP listener before probing; XOR-MAPPED-ADDRESS must
retain the external client's address and source port (`localPort`). This exercises the
actual Docker DNAT path; a gateway address from a userland proxy fails the check.
The fixture mounts only the diagnostic script read-only, removes its container
and namespace in `finally`, and saves `test-results/network/docker-stun.json`.
The official Rust responder is separately covered by real UDP integration tests.
The production host must repeat source-preservation and firewall validation
before release (#113/#105); this ticket performs no deployment or host firewall
change. Hairpin behavior and the optional port-preserving public-IP alias are
covered by the authenticated `same-server` scenario above.

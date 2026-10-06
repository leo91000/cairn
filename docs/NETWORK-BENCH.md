# Direct / relay network bench

Ticket [#102](https://github.com/leo91000/leo-agent-manager/issues/102),
[ADR-0033](adr/0033-direct-connection-with-relay-fallback.md).

## Run locally

Linux, `iproute2` (including `ss`), `iptables`, `util-linux`, Python 3, `/dev/net/tun`, the pinned
Rust/Node/pnpm runtimes and passwordless sudo are required. No KVM, Docker network,
VM, real email provider or agent credentials are needed. Use an empty disposable
Postgres database, never a deployed service. The usual browser fixture launches
both real binaries and uses an in-memory email adapter. Agent workers are disabled.

```sh
pnpm install --frozen-lockfile
CARGO_PROFILE_DEV_DEBUG=0 CARGO_BUILD_JOBS=4 cargo build --locked --workspace --bin leo --bin leo-official
pnpm build
pnpm exec playwright install --with-deps chromium
docker run -d --name leo-network-postgres -e POSTGRES_USER=leo -e POSTGRES_PASSWORD=test-only -e POSTGRES_DB=leo_official_test -p 127.0.0.1:5432:5432 postgres:17-alpine
export LEO_OFFICIAL_TEST_DATABASE_URL=postgres://leo:test-only@127.0.0.1:5432/leo_official_test
python3 tests/network_bench_test.py
```

Each command launches a complete authenticated Chromium session and a Rust client:

```sh
python3 tests/network-bench.py same-lan --output test-results/network/same-lan.json
python3 tests/network-bench.py nat-client --output test-results/network/nat-client.json
python3 tests/network-bench.py nat-installation --output test-results/network/nat-installation.json
python3 tests/network-bench.py nat-both --output test-results/network/nat-both.json
python3 tests/network-bench.py udp-blocked --output test-results/network/udp-blocked.json
python3 tests/network-bench.py symmetric-nat --output test-results/network/symmetric-nat.json
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
| symmetric-nat | Both routers use different UDP source ports per destination and deny unsolicited inbound packets. |
| network-change | The client moves to a new source address during a live stream; old-address sockets are closed. |
| packet-loss | A userspace TUN router drops every fifth outgoing IPv4 packet on both sides, including TCP; no random seed or optional netfilter/netem module. |

UDP probes send fixed sequence markers to two diagnostic listeners. Reports show
sent/received counts, whether the source was translated and the number of observed
mappings on both sides. The CLI tests assert these effects, including repeated
exact packet-loss counts. These diagnostic listeners cannot access installation
content and are not a WebRTC peer or an anonymous local installation interface.

## Route and switching evidence

The JSON report contains the topology probes, expected route and successful
application observations: browser send, Rust read, and stream resume after a
network change. Each includes `route`, `operation` and monotonic `elapsedMs`.
Browser evidence comes from successful requests to the actual official
`/api/installations/{id}/api/...` endpoint, followed by visible conversation data;
Rust verifies the same conversation marker through that endpoint. Anonymous
requests fail, anonymous installation loopback access fails, and the old official
cookie fails after logout. The network-change scenario measures from the address
change to the first successful resumed stream, with a 30-second upper bound, then
checks a Rust read and the retained conversation.

The shipped transport is currently **relay in every scenario**. #100/#101/#103
provide signaling, the installation WebRTC peer and web route selection. This
bench does not implement those tickets or claim that UDP reachability proves an
authorized direct session. `--expect-route direct` fails against the current
binaries; it never silently accepts a relay result or skips a test. Qualification
of real direct sessions and direct-to-relay timing must use observations of the
real DataChannel once those transports exist. Today's network-change duration is
**relay stream recovery**, not a direct-to-relay measurement.

`udp-blocked` demonstrates that the existing relay remains usable even when every
UDP probe fails. The existing `journeys-official-relay` and Rust relay/security
suites remain the regression checks for owner/member roles, withdrawal, session
expiry/revocation, detach, rotation and definitive revocation. The bench does not
change these contracts, local authentication, storage or central persistence.

## CI

The separate **direct-relay network bench (no KVM)** job downloads the same real
binaries/frontend as browser journeys, checks network effects, then runs all eight
scenarios sequentially without retries. JSON reports and Playwright failure
context are retained as `direct-relay-network-evidence`. Evidence contains no
cookies, claim codes, machine credentials or conversation exports.

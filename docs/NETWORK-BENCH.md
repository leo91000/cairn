# Direct / relay network bench

Ticket [#102](https://github.com/leo91000/leo-agent-manager/issues/102).
The bench uses real Linux network namespaces, the shipped official-service and
installation binaries, Chromium headless and a Rust client. It needs Linux and
passwordless sudo for network setup, not KVM. Use a disposable official Postgres
database; never point it at a deployed service.

The first network slice can be checked with:

```sh
python3 tests/network_bench_test.py
python3 tests/network-bench.py same-lan --probe-only --output /tmp/leo-network.json
```

The Rust UDP probe carries only fixed test markers. It is a network diagnostic,
not a direct installation transport, and cannot access installation data.
Production authorization, storage and relay contracts remain unchanged.

The current shipped transport is the relay. Direct signaling, installation
WebRTC and application route selection are delivered by #100, #101 and #103;
this bench must not claim that a UDP probe proves an authorized direct session.

# Public outbound UDP from agent microVMs

Date: 2026-10-16
Status: Accepted
Ticket: [#167](https://github.com/leo91000/cairn/issues/167)

Agent microVMs previously allowed only TCP and UDP DNS. This prevented agents
from testing STUN/WebRTC, QUIC and other UDP applications even when their remote
endpoints were public. There was no protocol-specific isolation requirement
behind the DNS-only default.

Allow public IPv4 UDP on every port after the same source-address and destination
fences as TCP. Deny private and reserved ranges, and deny every address of the
controller through INPUT. Keep IPv6 disabled. Accept inbound packets only as
conntrack ESTABLISHED/RELATED replies to the assigned guest address; explicitly
drop other inbound traffic independently of the host FORWARD policy. The reply
exception excludes traffic arriving from another microVM tap.

Use a slot-specific hashlimit bucket keyed by the validated guest source address:
50 NEW UDP packets per second, burst 100, shared across all ports and destinations.
DNS has no bypass. Unreplied packets consume tokens until conntrack considers
the exchange established. Established exchanges bypass this admission limit.
A ten-second idle expiry cannot refill the bucket sooner than its normal refill.
Network setup fails closed when the required kernel support is unavailable.

This admission guard reduces rapid creation of UDP tuples from a compromised VM.
It is not a bandwidth limit on established traffic, nor does source validation
prevent a public peer from replying with more bytes than it received. No new
inbound listening capability is granted. A host-level bandwidth policy, if
required, is a separate decision.

Qualification uses the real Firecracker/tap/iptables/NAT path: Internet STUN,
private/reserved and controller drop counters, source spoofing, unsolicited
inbound traffic under an ACCEPT host policy, and a bounded burst to an isolated
peer. The existing TCP smoke stays in place. See [MICROVMS](../MICROVMS.md).

There is no persisted-format change. The policy applies when a VM network is
created, including after controller restart; existing running controllers retain
the policy of their binary. Rolling back and recreating the network restores the
DNS-only policy.

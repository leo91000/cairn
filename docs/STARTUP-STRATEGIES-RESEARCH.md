# Startup strategies: reaching 500 ms

Research date: 2026-09-12. This note evaluates architecture and experiment design;
it does not establish measured latency. Hardware measurements belong in the
accompanying benchmark report. Upstream references are pinned to Firecracker
1.17.0, the version configured by this project.

## Recommendation after the initial experiments

Prefer a bounded pool of independently booted, unused VMs with warmed tool
binaries and cached toolkit setup, while starting a **fresh Codex process** after
account and agent policy assignment. Retaining a recent conversation's VM can
likewise avoid repeated boot without retaining its Codex process. Consider
persistent Codex or snapshots only if whole-path measurements show that the
simpler approach cannot meet the target.

The concurrent VPS investigation reports approximately 216–292 ms to initialize
a fresh Codex process in a prepared guest, versus about 3.2 s for a cold 4-GiB
guest. A persistent process reaches local `thread/start` in about 40 ms; a restored
snapshot with persistent Codex reaches usable readiness in about 190 ms including
its private disk copy, while creating the full 4-GiB snapshot takes about 19 s.
These preliminary figures exclude account authentication, external MCPs and
inference; consult the benchmark report for final samples and phase definitions.
They favor the simpler prepared-guest path before accepting session-retention or
snapshot complexity. This is an engineering inference from experiments, not an
upstream performance guarantee.

The target should be **message accepted by manager → prompt submitted to Codex**.
Report first model output separately. A sub-500-ms local RPC response does not
prove sub-500-ms authenticated model submission or a sub-500-ms first reply.

## What the current application actually does

The current controller creates a private disk, network and jail, then boots a
2-vCPU, 4-GiB VM. It starts Firecracker with `--no-api`, so snapshot experiments
need an explicitly enabled local API socket. Guest readiness uses 50-ms polling.
The guest starts an execution process for each attempt, and `chat_process::run`
starts and closes its Codex app-server session for that attempt. Keeping only the
guest kernel alive therefore does not eliminate Codex initialization or login.
Sources: [host](../crates/installation/src/microvm/host.rs),
[guest](../crates/installation/src/microvm/guest.rs),
[chat process](../crates/installation/src/chat_process.rs), [RPC](../crates/installation/src/rpc.rs).

Persistent disks already provide restart correctness, and projects are opened on
demand. Retain both properties; memory retention must remain an optional cache.
The configured four 4-GiB execution slots live in a controller capped at 20 GiB.
Unused and recently used VMs must share that resource budget with active work.
Source: [existing execution contract](MICROVMS.md).

## Options and tradeoffs

| Strategy | Work removed from the request path | Main remaining cost / concern |
| --- | --- | --- |
| Improve cold boot and toolkit setup | Repeated avoidable initialization | Kernel boot, auth and MCP still run |
| Unused, already booted VM | Disk, network and kernel setup | Starting Codex and binding account/policy |
| Unused VM with initialized Codex | Above plus process initialization | Account and per-agent configuration must be applied correctly |
| Retained conversation VM and Codex | Repeated VM/process/thread setup | Account changes, expiry, resource pressure and background processes |
| Snapshot of a pristine booted guest | Most boot work, without keeping every VM resident | Restore integration, demand faults, clock/network/entropy repair |
| Snapshot of initialized Codex | Above plus process initialization | Duplicated userspace state and stale connections; most complex |

These are architectural expectations. Compare actual usable readiness, not just
whether an API request returned successfully.

## Snapshot constraints that affect this application

Snapshot restore maps memory on demand with copy-on-write. The backing memory
file must stay immutable and available while the VM runs. Disk images are managed
separately. Existing vsock connections close on restore, although guest listeners
survive. Full snapshots fault in guest memory; snapshot creation therefore has
costs absent from a restore-only chart. Diff snapshots are marked developer
preview. Compatibility depends on the software/hardware environment.
[Snapshot lifecycle](https://github.com/firecracker-microvm/firecracker/blob/v1.17.0/docs/snapshotting/snapshot-support.md).

For Cairn, reconnect the auth relay and output stream after restoration. Pair the
memory snapshot with its matching private disk state. Never restore several
clones against one writable disk. Benchmark after touching the useful guest
working set, and include snapshot preparation/storage separately.

The API permits changing the host TAP and vsock socket path on load. It also has
`clock_realtime` for advancing x86 kvmclock on restore. These controls do not apply
the application's account or project policy.
[API schema](https://github.com/firecracker-microvm/firecracker/blob/v1.17.0/src/firecracker/swagger/firecracker.yaml).

The initial VPS experiment did not pass its guest-clock assertion with
`clock_realtime: true` alone. Treat clock correction as an unresolved validation
gate until the actual guest clocksource, elapsed time and explicit guest-side
correction are verified. An API option's existence is not proof that this image
meets the required clock behavior.

Clones otherwise retain the original guest IP configuration. Either isolate
identical guest networks in separate namespaces, or change guest addresses/routes
alongside the host TAP override. Upstream's example explicitly does not establish
a production security or performance result.
[Networking for clones](https://github.com/firecracker-microvm/firecracker/blob/v1.17.0/docs/snapshotting/network-for-clones.md).

VMGenID allows supported Linux kernels to reseed their kernel RNG after restore,
but there is a notification race and it does not fix userspace cached random
values, tokens or identifiers. Avoid capturing credentials or customer workloads
in a reusable template. Verify the actual built kernel configuration and provide
an explicit post-restore readiness barrier before starting account-bound code.
[Entropy for clones](https://github.com/firecracker-microvm/firecracker/blob/v1.17.0/docs/snapshotting/random-for-clones.md).

The project merges an x86 defconfig with its own fragment: absence of an option
in that fragment does not prove absence from the built kernel. Inspect the image's
`/opt/cairn-vm/kernel.config`.
[Kernel build](../Dockerfile), [configuration fragment](../deploy/microvm/kernel.config).

## Practical retention design

Keep allocation separate from execution attempts. A controller-owned reservation
can be `preparing`, `unused`, `assigned`, `idle`, or `retiring`; assignment to a
conversation is irreversible. Never return a used guest to the unused pool.
Retain one owner and exclusive disk lock across attempts. Manager events and
persisted Codex session IDs remain the recovery authority.

Start with one unused VM and a short idle TTL, then tune from measured hit rate.
Evict idle/prepared VMs before delaying active work. Track guest allocation,
controller cgroup memory, Firecracker RSS/PSS, page cache and idle CPU; a quiet
4-GiB guest is neither proof of 4 GiB resident usage nor permission to exceed the
controller budget. Pausing can stop guest CPU work but is not memory reclamation.

Idle YOLO guests may contain background processes. Define whether those keep
running during retention. Revalidate agent policy, model, MCP configuration and
account selection at every new attempt; restart Codex when an incompatible change
cannot be applied safely. Retaining a process must not silently pin the account
and violate the existing highest-capacity selection rule.

In the current implementation, thread setup carries model, reasoning, working
directory, sandbox, instructions and writable roots. MCP URLs, tool allowlists
and stdio server configuration instead enter through `-c mcp_servers…` process
arguments; the short-lived gateway grant enters through `CAIRN_MCP_RUN_TOKEN` in
the process environment. Calling `thread/start` or changing model settings does
not, in this code, replace those process arguments or its inherited environment.
A retained Codex process therefore needs an explicitly tested grant/configuration
rebinding mechanism, or must restart when these change. The fresh-process option
already preserves this boundary and is the recommended initial experiment winner.
Sources: [MCP run configuration](../crates/installation/src/mcps.rs),
[process setup](../crates/installation/src/main.rs),
[thread setup](../crates/installation/src/chat_process.rs), [RPC launch](../crates/installation/src/rpc.rs).

Memory ballooning can reclaim pages, but needs a configured device/guest driver
and an explicit target policy; shrinking a guest can fail to reclaim the target
or introduce memory pressure. It is a separate experiment rather than a free
optimization of the initial pool.
[Balloon device](https://github.com/firecracker-microvm/firecracker/blob/v1.17.0/docs/ballooning.md).

## Benchmark acceptance criteria

- Alternate variants on identical images and CPU/RAM limits. Record first start
  separately; do not flush the production host's global caches.
- Measure disk/network, guest RPC, toolkit, Codex initialization, account binding,
  MCP readiness and model submission as separate phases.
- Include one message, repeated messages, long-idle wakeup, concurrent allocation
  and pool-empty fallback. Small samples support medians, not credible tail SLAs.
- Check retained edit/session correctness, account/policy changes, disconnects,
  cancellation and controller restart. A fast path that loses these is rejected.
- Report idle CPU, total memory, disk allocation and refill time alongside latency.
  A prewarmed result moves work earlier; it does not make that work free.

The investigation should select the simplest strategy whose **whole measured
request path** meets the goal. Do not turn a marketing microVM boot number, an
unauthenticated `thread/start`, or a snapshot API acknowledgment into a promise
about actual chat response time.

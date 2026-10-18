# 0040 — Durable exclusive node reservations

Date: 2026-10-13

## Decision

Give explicitly authorized runs a bounded reservation of their current node's
execution envelope. Block new admission and speculative VM preparation, suspend
other live VMs, and report acquisition only after their pauses and any requested
memory headroom are confirmed. Controller, authentication, leases, disk safety
and recovery services stay active. This excludes other Cairn guest execution;
it does not allocate every physical host CPU or stop software outside Cairn.

The installation owner grants this permission separately with a node's
`exclusiveAgents`. Existing nodes have no grants. The current agent must still
have node access and cover its original run permissions. Dependencies must be
explicit, active attempts of the same agent on the same node; a new attempt
cannot inherit a member's privilege. `list_nodes` reports eligibility and whether
a reservation is active without exposing another agent's run or reason.

Requests use a durable FIFO order, an ID and monotonically increasing ownership
epoch. A node accepts at most one reservation. The manager closes its admission
gate transactionally before asking the controller. The controller independently
serializes admission and ownership, records every pause target before changing
CPUs, and acknowledges acquisition only after resource validation. Failed
acquisitions roll back; a release installs a durable epoch tombstone so a delayed
acquire cannot reclaim the node. Manager recovery observes the controller's epoch
and reconciles a reservation absent from a restored manager database.

`acquire_exclusive` has a duration of 1–900 seconds and a response wait of
0–3,600 seconds. The independent `acquisitionTimeoutSeconds` bounds queued
eligibility to 1–3,600 seconds (default 300). A zero response wait returns the
queued state immediately without expiring its queue entry or claiming acquisition.
`exclusive_status`, `release_exclusive` and `renew_exclusive`
operate on the same request. Repetition does not extend its original duration.
Explicit renewals may extend a live acquisition up to a one-hour lifetime;
the manager reports the old confirmed expiry until renewal is acknowledged.
Queues are bounded to 64 requests per node, with at most four node reconciliations
in flight. Terminal request history expires after 30 days. Reachable controllers
are also inspected when a restored manager database contains no requests.
Web and Android expose the owner, reason, expiry, suspended tasks and
an owner-controlled release action.

The controller enforces durations with Linux `CLOCK_BOOTTIME`, using the remaining
duration sent by the manager rather than comparing clocks on different nodes.
Retries do not move the local deadline. A persisted boot deadline also prevents
clock skew from extending a renewal beyond the one-hour lifetime; the wall-clock
expiry remains a display value.

## Composable suspension and recovery

One arbiter owns VM state transitions. Durable reasons include storage pressure,
capture, retention, terminal revocation, and an exclusive reservation ID/epoch.
Removing one reason cannot resume a VM held by another. Reasons bind to the
physical VM and current attempt; stale callbacks cannot resume a reused VM.
Retained ownership transfers before setup, while exclusive suspension waits for
setup to complete. Inbox imports and pending state confirmations use the same
arbitration rather than resuming behind an exclusive reservation.

Administrative suspension compensates the execution budget only after the CPU
pause is acknowledged. Manager checkpoint accounting is monotonic per attempt
and persists a coherent budget/observation pair. Security deadlines and access
revocations remain in wall time; an expired execution lease cannot be resumed.

Expiration, cancellation, completed work, movement, changed permissions and
durable waiting cause release. Saving `sleep` or `wait_for` also records release
intent in the same transaction. Before peers resume, the controller confirms
that the privileged group is paused or stopped. It then removes this reservation's
reason, including from the owner, and returns the node to ordinary sharing.
An uncertain teardown is an error, never proof that a VM stopped.
If an execution's logical completion is lost, release waits for a separate
physical-cleanup acknowledgment before removing its stale active entry. A panic
that loses VM ownership during startup or shutdown remains fenced and requires
controller recovery; a closed acknowledgment channel never proves cleanup.

On a controller/container restart, the existing PID-namespace fencing contract
still applies: processes are gone, disks and native sessions remain recoverable.
CPU suspension is exact continuity on a surviving node, not an exact-process
guarantee after physical node loss. External services keep advancing; the agent
must inspect ambiguous external effects rather than repeat commands blindly.

## Memory reclamation without device reset

All VMs already share the node's budget
([shared-node-budgets decision](0012-shared-node-budgets.md)). CPU suspension retains RAM.
A request may also specify a minimum available RAM target. Acquisition verifies
both current cgroup headroom and host `MemAvailable`, including after paging.
This is confirmed headroom at acquisition, not a separate per-VM RAM quota.

For insufficient headroom, reclaim pages of paused live VMMs with
`process_madvise(MADV_PAGEOUT)`. Pin process identity with a pidfd and hold the
physical VM/suspension guards until the kernel operation finishes. Verify private
swap before advice: zram without disk writeback or encrypted dm-crypt block
devices; reject plain swap, unqualified swap files and zram with backing storage.
The controller has `SYS_PTRACE` and `SYS_NICE` in its own PID namespace.

This preserves the original process, its KVM state, nested emulator, block
transports and vsock streams. We deliberately do not restore active Firecracker
snapshots: the current snapshot templates are anonymous, snapshotting resets
vsock, and active nested KVM restoration is not qualified. A RAM dump on every
CPU-only reservation would add avoidable I/O. Unsupported or insufficient paging
returns an explicit failed acquisition after rollback; advised bytes never stand
in for measured headroom.

Pageout is cooperative kernel work. Its time limit is checked between bounded
advice batches; a kernel call can outlast that limit. Cancellation cannot drop
ownership while a blocking syscall still touches the VM. Authentication and
administrative-time inspection therefore do not wait behind the pageout lock.

## Validation

Meaningful regressions cover overlapping reasons, lost acknowledgments, old
epochs, ownership handoff, orphaned timers, monotonic recovered budgets, fair
manager requests, permission revocation, partial rollback, memory qualification,
private swap, and release-before-wait. Existing storage capture, retained startup
and inbox behavior retain their tests.

The image CI runs `tests/runner-exclusive-smoke.mjs` against the exact candidate
image. With private zram on a disposable host it requires actual memory reclamation.
If the host kernel cannot load zram (observed on hosted Azure 6.17.0-1022),
positive reclamation becomes optional: any successful memory acquisition still
checks residency and headroom, and any refusal must complete rollback.
CPU and nested-vCPU continuity
remain required. A skipped check does not qualify RAM reclamation on that host.
It checks the same command/VMM PIDs and buffer contents, nested-vCPU progress,
admission fencing, checkpoint arbitration, reservation expiry and automatic resumption.
Optional Android mode checks that the same booted emulator survives pause/reclamation.
Hosted AMD Android boot is still unqualified (see NESTED-KVM-RESEARCH.md), so this
mode is skipped in ordinary image CI and can be requested with the
`exclusive-android` workflow-dispatch input. Run it on a compatible Intel host
before claiming nested Android continuity:

```sh
CAIRN_EXCLUSIVE_REQUIRE_RECLAIM=true CAIRN_EXCLUSIVE_ANDROID=true \
  node tests/runner-exclusive-smoke.mjs '<exact candidate image digest>'
```

Android UI cases run on JVM and on an actual AOSP emulator.

Local metadata measurements and their limits are recorded in
[EXCLUSIVE-NODE-MEASUREMENTS.md](../EXCLUSIVE-NODE-MEASUREMENTS.md). They are not
production VM acquisition, paging, network or filesystem latency measurements.

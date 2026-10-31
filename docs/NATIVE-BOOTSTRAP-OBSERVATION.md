# Native bootstrap observations

Native preparation can continue after the manager's `Executing on node` event
and before systemd receives a workload. An observed restored environment spent
822 seconds in that interval, while another same-node environment spent 4.9
seconds. Both executions succeeded. Their files, sizes, identities and caches
differed; those observations do not establish which operation caused the delay.

The native controller now observes the actual admission and execution call sites.
This is technical diagnosis, not coding-engine progress or an execution state
machine. It does not change launch, leases, cancellation, storage ownership,
filesystem barriers, identity translation, ACLs, capabilities or batch sizes.

## Reading the current or retained observation

The existing authenticated native-controller protocol accepts:

```text
GET /runs/<attempt-uuid>/bootstrap
Authorization: Bearer <node-controller-credential>
```

The response contains `source: "memory"` and a `bootstrap` object. Its identities
are the attempt, optional conversation, controller runtime, pinned tooling
runtime, optional Linux boot and unique controller session. `invocation` is a
session-wide monotonic sequence, independent of cache eviction. It distinguishes
a new admission after an earlier rejected admission on the same attempt.
Missing context remains null, rather than being inferred from another
attempt or a mutable conversation marker.

The fixed phases cover admission wait, plan validation, storage admission,
workspace ownership, mount, identity provisioning, filesystem initialization,
temporary preparation, identity reconciliation, home refresh, staging ownership,
runtime preparation, pool accounting and workload launch. Phase durations
accumulate if an existing wait loop revisits a phase.

`status: "preparing"` means this bootstrap invocation has not ended. `submitted`
means the existing `systemd::spawn` call returned a child for `systemd-run`.
**It does not prove unit readiness, an agent-of-code turn, physical close or a
successful result.** `rejected`, `incomplete` and `already_running` likewise
describe the observed bootstrap invocation, not the final conversation outcome.
Final elapsed time freezes; the normal execution protocol remains authoritative.

`/health` adds bounded `bootstrapObservation` aggregates, without attempt IDs.
Observation lookup performs no filesystem, mounted-volume or process inspection;
the authenticated route retains the existing controller credential check.
Contended aggregate observation is explicitly unavailable; omitted
observations and export losses are reported, never converted into healthy work.

The controller retains at most 256 observations. Completed bootstrap reports may
be evicted to observe a new attempt; active preparation reports are not evicted.
If all slots contain active preparation, observation is omitted and counted. A
404 means unknown or evicted, not completed. Conversation deletion removes its
in-memory observations and disables late updates. A controller restart clears
the cache; session IDs prevent confusing later invocations with old reports.

## Reconciliation cost

The real reconciler records marker matches, replay/new transitions, entries seen
and changed, batches published, entries applied, pending JSON bytes read/written,
successful syncfs, pending retirements and completed directory synchronizations.
Entries count visits, not distinct inodes. Bytes are ownership-journal JSON bytes,
not encoded block-storage size or user data size.

Fixed cost buckets record calls and elapsed microseconds for scan, capture,
apply, journal publication, pending readback, syncfs and pending retirement.
Operations count attempted calls, including unsuccessful or interrupted calls;
completed-barrier counters only advance after the existing operation succeeds.
Costs are diagnostic observations, not a transactionally sealed storage report.
They do not describe every instruction in the bootstrap or certify durability.

Matching-identity fast paths remain intact. Interrupted ownership metadata is
replayed before checking that fast path. The observer does not supply a cache
that could skip this recovery or avoid inspecting required filesystem metadata.

## Export and privacy

Memory observation remains available independently of logging. Export is enabled
only when the existing `cairn_performance=info` tracing target is enabled at
controller startup; a disabled target creates no exporter thread or channel.
Phase changes and a final bounded report are offered to a 32-entry channel using
`try_send`; repeated visits to the same phase are rate-limited. Per-inode counters
do not export, log or perform I/O. An optional detached observer sends one
non-blocking Unix datagram to journald, with a maximum entry size of 8192 bytes.
It neither shares stdout's writer lock nor occupies Tokio's blocking pool. A
full channel, unavailable journal, saturated socket or oversize report loses
diagnostic export, not execution. These losses are counted in memory. No retry,
memfd fallback, new workflow timeout or telemetry flush barrier is introduced.

The [systemd native journal protocol](https://systemd.io/JOURNAL_NATIVE_PROTOCOL/)
defines the datagram transport and newline-separated fields. The report contains
only fixed phase/counter names, bounded identities, times and numeric costs. It
never accepts paths, prompts, command lines, credentials, file contents or raw
UID/GID maps. Existing journal retention controls exported observations.

For example, on the native node, a late diagnostic can query its technical log:

```sh
journalctl -u cairn-native-node.service CAIRN_OPERATION=native_bootstrap \
  CAIRN_ATTEMPT_ID=<attempt-uuid> -o json
```

`CAIRN_BOOTSTRAP_JSON` contains the report. Correlate its controller session,
boot, runtime, attempt and invocation; journal order alone is not ownership.
Export is best-effort: the absence of a journal entry is never proof of an idle,
healthy, finished or failed attempt. These diagnostic updates must not advance a
coding-engine progress clock or acknowledge a pending user message.

## Qualification

Regression tests hold the real controller admission and real home reconciliation
while reading observation, health and renewing a lease. They cover a full or
closed observer channel, blocked exporter shutdown, bounded retention, late
updates after deletion and repeat admission identity. The real reconciler's
33-file ACL fixture exercises two unchanged journal batches and seven unchanged
directory synchronization barriers, then a zero-scan matching-identity fast path.
Existing recovery tests continue to verify mode/capability and interrupted inode
restoration.

The backend CI additionally prints `CAIRN_BOOTSTRAP_OBSERVER_MEASUREMENT` for
100,000 in-memory counter/timer updates with disabled and enabled observation,
CPU/wall time, debug profile, parallelism and serialized report size. This
measures observer hot-path overhead only. It is not a matched cold/warm storage
benchmark or evidence that the 822-second bootstrap has been accelerated. Those
measurements require a controlled immutable tree and a disposable qualified host
before changing migration or its durability contract.

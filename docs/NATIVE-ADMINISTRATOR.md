# Native administrator on a node

The node has one privilege choice: **Native protected** (the existing default) or
**Native administrator**. There is no conversation sudo switch and no agent
privilege-mode permission. All conversations placed on a node use its confirmed
mode. Firecracker and protected native hardening remain unchanged.

Administrator runs the ordinary Cairn workload account with unrestricted,
passwordless `sudo`, on the real host filesystem. It omits the protected unit's
managed root, read-only host mounts, capability, device and privilege restrictions.
It keeps the managed conversation HOME, workspace/output, pinned tools and
per-attempt configuration binds. Root can enter the system manager's namespaces,
install host software and administer services. Use a machine or VM that the owner
has already isolated for this purpose. `sudo`, `visudo`, `runuser` and the standard
`/etc/sudoers.d` include must be installed; the mode protocol capability identifies compatible controllers, and activation
validates actual noninteractive sudo before reporting administrator applied.
Enrollment and migration never enable administrator automatically.

Root can read all data and credentials present on this machine, including the
supervisor, node identity and other local executions. Cairn does not grant any
additional remote accounts or resources: credentials obtained on the host still
carry their existing remote permissions. Root can alter the supervisor, its
credentials, services and processes, leave processes outside the managed cgroups,
and defeat CPU/RAM budgets, disk monitoring, leases, cancellation and cleanup.
Those controls continue to operate normally for cooperative workloads. Their
acknowledgements are **not** tamper-proof fencing or revocation against hostile
root on its own host. No claim here extends the native boundary to Firecracker.

## Review and admission

The owner changes the mode from node details on the web or Android. Preview
lists trusted account IDs, admitted agents (including all-node grants), existing
conversations (including potential first placements still in the automatic queue)
and scheduled tasks, interruptions and host powers. Confirmation
is bound to that snapshot for five minutes and consumed once. Concurrent changes
to those inputs require another preview. The mode choice stays on the node.

The explicit confirmation reviews the displayed agents for that administrative
node. A future node or newly created agent is never covered by an earlier global
or automatic selection. New explicit node access is reviewed by the owner through
the existing agent permission editor or node access step, which describes host
sudo. Agent saves supply transient `nativeNodeReviews` node IDs from that informed
review; node grants and trusted-person edits require transient
`confirmNativeAdministration: true`. Neither is a persisted agent privilege
mode. An owner tool call can review selected node IDs while preserving wildcard
placement access; other administrative nodes remain unreviewed. A wildcard alone does not approve administrator admission. Removing access
or changing backend removes that node's review; regranting a wildcard cannot
revive it. Adding trusted people on an administrator node requires the existing
node editor's informed owner confirmation. Native provenance, trusted people,
current membership, node access and delegation ceilings remain mandatory.

Public Cairn MCP and `cairn_installation` share `change_native_node_mode`:

```text
change_native_node_mode(nodeId, change: { mode }) -> previewId, preview
change_native_node_mode(nodeId, change: { mode, previewId, confirm: true })
list_nodes() -> nativeModeRequest, nativeModeObserved, nativeModeStatus
```

An authenticated owner can confirm. A delegated installation agent, or a legacy
MCP credential without verified owner identity, can submit a request but receives
`awaiting-owner` for elevation. The owner reviews that request in node details;
the AI cannot turn its own host into administrator mode. Other management
authorization and scope/revocation checks run again in the mutation transaction.
Tools and UI expose the same warnings, preview inputs and observed state.

## Transition and recovery

A request records a unique generation and immediately blocks new placements and
lease renewals. Active attempts are marked for controlled restart. On the next
heartbeat the connector submits the generation to the local controller, even
while execution readiness is false. Under its admission lock the controller
persists `pending`, cancels every preparation/workload, stops complete managed
cgroups including process descendants, waits for disk ownership and checkpoint
cleanup, then changes its sudo policy. It persists and reports `applied` only
after those steps succeed. The manager requires the matching observed generation
before admitting work; an already reserved stale plan is rejected locally too.

All new messages, schedules, retries, resumptions, delegation and automatic,
pinned or preferred placement use current node admission and the confirmed mode.
Existing environments stay on their node unless explicitly moved; pinned work
waits for that node. Incoming directing people still contribute to provenance.
No new placements on a transitioning node overtake its stop. Existing work keeps
files and thread history; an interrupted engine resumes under the confirmed mode
with an explicit effective-mode notice. The node and conversation details show
that effective mode separately from the requested transition.

A crash retains the pending generation; an incomplete or failed transition
refuses new execution. Restart recovers owned work before serving requests. The
same request is retried cooperatively by the connector; an applied generation is
idempotent. Unconfirmed stop, unavailable sudo, disk cleanup failure or durable
write failure cannot produce a protected success claim. Owners can retry from
node details after diagnosing the supervisor. Durable-write errors remain failed
and visible in health even when the disk cannot record the error. During an
unconfirmed transition, clients name the last confirmed mode and explicitly
mark current privileges unconfirmed. A first preparation interrupted before any
engine session exists restarts with the preserved files and conversation history;
an available engine session is still resumed.

Returning to protected removes the Cairn sudo rule and restores the existing unit
restrictions for new executions, after stopping old ones. It **does not** undo
administrative changes, clean or restore the machine, or create a new trust
boundary. A root-modified host requires the owner's own recovery/reinstallation
procedure before it can be trusted again.

## Compatibility and qualification

Epoch 13 / SQLite 12 fence older managers before they read administrative records.
Old nodes remain protected by default and cannot change mode without capability
`nativeModeProtocol: 1`. New plans carry the exact reviewed mode/generation.
Native state budgets carry a strict `nativeModeProtocol` marker: old native
controllers reject that on-disk budget, and the installer's independent epoch
check forbids ordinary cross-epoch fallback. See [automatic migrations](AUTOMATIC-MIGRATIONS.md).
No workspace, checkpoint, encryption or S3 encoding changes are introduced; the
existing storage measurements remain the reference.

`scripts/tests/native_runtime_test.py` runs only as root with
`CAIRN_NATIVE_DISPOSABLE_HOST=1` and loopback synthetic S3. It exercises ordinary
UID plus host sudo and a real `/etc` write, protected denial of sudo/personal
paths, concurrent conversations, root process descendants, both transitions,
retained files and host changes, idempotent generation retry and effective health
state. CI supplies the disposable VM; never run that fixture on the owner's node.
Backend tests cover unknown modes, old supervisors, unapproved users, snapshot
races, pending/failed/stale generation admission and future wildcard grants.

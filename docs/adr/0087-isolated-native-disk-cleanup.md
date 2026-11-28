# Isolate native physical disk cleanup from execution availability

Native logical completion, workload fencing and physical disk retirement are
separate observations. A thread blocked in a kernel operation cannot be cancelled
by a Tokio timeout; its runtime must not participate in node shutdown.

Retain a per-conversation cleanup record outside its mounted filesystem before
transferring physical close to a dedicated OS worker. This worker owns the actual
workspace and disk leases through flush, normal
unmount, seal and nonce-verified kernel retirement. A bounded observation returns
an isolated failure with the controller's diagnostic while the worker keeps its
ownership. Admission, deletion and restore cannot reuse a quarantined disk.
A bounded active-worker pool queues additional durable entries; queueing never
releases a disk. Incomplete atomic-write temporaries are not recovery receipts.
The run's initialization, supervision and imported/artifact filesystem accesses
also execute on independent runtimes. Cancellation has a bounded observer;
physical cleanup waits for the still-owned execution/access guards and protection
task; those joins never precede the bounded cleanup observer.
A per-attempt launch gate makes a final workload fence exclude future submission:
prepare systemd arguments before the gate, then retain it through submission and
unit observation. Cancellation rechecks the gate before accepting a stopped
workload. Shared base-cache eviction retains its short global storage lock. The native
controller bounds its final runtime shutdown without releasing uncertain disk
receipts. Incoming seed/home copy bytes are durably reserved under the pool gate
before copies proceed concurrently. Artifact/import copies reserve their bounded
incoming bytes under the same gate; interrupted reservations survive until
recovery. Mounted I/O never holds that global accounting gate. Admission charges pending
reservations conservatively; supervision and health count actual retained bytes,
so materialized copies do not trigger limits through a duplicate reservation.
The attempt's suspension and active marker are settled before removing quarantine,
including completion after the observer has already reported failure.

Startup inventories and attests native mounts, prepares a private backend view
that removes only its attested copies, then recovers each recorded disk outside
the controller's liveness path. The host itself still uses ordinary unmount.
Unknown mounts and ownership records remain refusals, never deletion authority.
Native mount preparation and status observations also use independent runtimes.
They retain their journal maintenance executor after returning a live volume;
reopening can ask consolidation to yield without a lingering workspace lock.
Native physical workers do not consume the shared VM metadata-I/O or consolidation
permits. Physical worker/status capacity is released once that operation ends,
not after potentially indefinite background consolidation retries.
Older supported kernels that explicitly refuse asynchronous deletion fall back
to their existing synchronous command on the same isolated owning worker. No
fallback occurs for other failures. Native async deletion acknowledgements are followed by an explicit device
identity/absence check; Cairn retains receipts on uncertainty. VM semantics stay
unchanged.
Native absence observation preserves the encoded kernel result: the pinned
ublk library's legacy retry can mask `ENODEV` as `EOPNOTSUPP` on kernels without
legacy opcodes. Only in that case, an independent read-only encoded query checks
the kernel device registry. A missing sysfs entry or unsupported query alone
never establishes retirement. Pending retirement errors reach the conversation
with their native operation and cause instead of a generic internal error.

The authenticated native result may attest that its workload was fenced while
physical cleanup remains pending. It is not PhysicalFinalized. The manager stops
automatic replay and exposes that cause; only workload reservations may be released.
Physical state and retained disk capacity survive controller restart. Recovery
fences every recorded workload; an independent observer publishes its pending
result only after the observation deadline. Ordinary completed recovery retains
the existing interrupted-attempt result and retry policy. A dead backend can require an owned I/O abort before a
D-state workload can stop; this fallback cannot publish a workload-fenced result
until the subsequent stop succeeds. Diagnostic updates retain that proof without claiming retirement.
The manager persists the attempt's fenced cleanup response before asking for
optional retirement evidence. A second wait on the same attempt can confirm
physical closure after the disk flushes and retires. Recovery resumes an
interrupted cleanup observation and commits its diagnostic with publication.
The recorded execution failure continues to block automatic replay because
the workload's exit status remains unknown.
After DELETE has acknowledged a pending cleanup, the manager gives an already
settled successful turn a short window to observe retirement. If that
observation cannot confirm retirement, the manager releases the execution
reservation and durably defers settlement with a five-minute deadline.
The manager resumes bounded observations across restarts on a timer independent
of ordinary recovery. Observations are tracked by execution attempt. Waiting
attempts receive a slot before earlier attempts retry. The manager settles the
saved reply only when DELETE returns the
attempt-bound `NativeCleanupRetired` receipt, which also checks the current
conversation quarantine. A missing proof at the deadline publishes the original
cleanup failure. The manager stores the verdict with its fence until it commits
the run result. That publication retains the original turn end so later messages
stay queued for the next turn. Environment detachment settles the saved verdict
before replacing the execution checkpoint. Without proof it publishes the
original cleanup diagnostic. Expiring a fenced environment preserves an
explicit Stop and does not create a new one.
The live manager checkpoint serializes fencing and reloads its durable evidence
before heartbeat or output persistence can overwrite it; placement lease renewal
continues independently while the fence waits.

The native marker advances to `ublk-rootless-v3` before a mounted disk is
handed to execution. This reader accepts v1/v2 and upgrades only its marker;
v2 controllers reject v3 rather than ignoring the cleanup quarantine.
The manager records a published failure in the checkpoint's existing last-error
field. A deferred settlement also preserves the pending diagnostic and deadline
in the checkpoint until it can settle. A run
interrupted before its first v3 mount has no new pending physical close state.
No journal format change or automatic migration of disk content is introduced.
The cleanup record is additive controller state and is removed only after verified
retirement. Rollback must not ignore these records: older native controllers are
not compatible with a live cleanup quarantine.

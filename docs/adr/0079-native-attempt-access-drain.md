# Drain native filesystem access per attempt

This supersedes the post-fence global-gate drainage in
[ADR-0077](0077-native-close-with-independent-backend-namespaces.md). Its backend
namespaces, fencing, physical close and ownership contracts remain unchanged.

The node-wide storage gate orders pool accounting, policy changes, preparation,
shared project archives and bases, identities and host staging. It can be held
during network transfer or slow preparation. A fenced attempt must not wait for
those unrelated operations merely to establish that its own live filesystem
access has drained. This decision does not identify a historical gate holder.

Each controller `Attempt` owns one live-access mutex, also retained by its
finalizer. Artifact snapshots and project imports are the authenticated routes
that access the mounted filesystem outside execution/finalization. They acquire
the existing process-safe global gate **before** the attempt mutex. After both
acquisitions, they revalidate the active attempt, requested conversation, mutex
identity and cancellation. A request waiting for the global gate holds no
attempt mutex, so it cannot delay close. A request queued before controller
cancellation cannot access an already closed filesystem when the pool later
becomes free.

The existing non-GET handler owns its entire route task. After admission, that
task retains both guards until the operation settles; no nested task is needed.
Dropping or timing out its HTTP caller detaches only that caller; it does not
abort a blocking filesystem open or release its guards early. The file receiver
must flush buffered writes even when a later transfer frame fails. A dropped
HTTP caller does not cancel an already queued POST: its route still revalidates
the active attempt and controller cancellation after pool admission. The global
gate still orders admitted writes against every other pool mutation and policy
update, including shared cache/base operations.

Finalization still publishes cancellation, positively fences the workload and
settles its protection task before draining access. It drains only this attempt's
mutex, with the existing 300-second bound and retry behavior, then releases that
mutex before physical close. Late admissions refuse cancellation. Flush, unmount,
journal sealing and nonce-verified device retirement retain their lifecycle
mutexes and logical/physical leases through success, errors and cancellation.
Another attempt for the same conversation still needs its execution owner lock.
Capture and growth retain their existing mount/journal synchronization.

The attempt mutex is controller-local because all live filesystem requests use
that controller. Connector seed, inbox and archive operations mutate host
staging rather than the mounted journal and retain their existing global gate.
No cross-process pool serialization is removed. On controller death, the old
in-memory mutex is not authority: startup must still fence/recover workloads,
mounts and devices before opening the controller socket. No persistent mutex
registry or new ownership receipt is introduced.

There is no lock-order cycle: ordinary access acquires global, then attempt,
then briefly the attempt map for revalidation. Close waits for neither the
global gate nor the attempt map while holding the attempt mutex, and releases
it before acquiring the mount lifecycle mutex. Policy/admission cannot acquire
an attempt mutex before the global gate.

Validation uses the actual handlers and global lock, a blocking artifact-open
barrier, queued requests, caller cancellation, and the existing real-journal
physical-phase tests. Disposable CI separately qualifies namespace isolation and
native ublk/systemd ownership. Controlled timing measurements compare the former
global drain with scoped drainage under the same held pool lock; they are not
production network or kernel-retirement latency measurements. Data and object
formats, grants, pool reserves, transfer concurrency and timeouts are unchanged.

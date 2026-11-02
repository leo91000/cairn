# Native close with independent backend namespaces

The post-fence mutation-drain scope below is superseded by
[ADR-0079](0079-native-attempt-access-drain.md). The independent namespaces and
physical ownership contracts remain in force.

A fenced native conversation can wait in flush, unmount, journal sealing or
ublk retirement. Holding the node's storage gate during those waits blocks
unrelated conversations. Removing that gate alone would break
[ADR-0052](0052-native-ublk-workspaces-and-idle-eviction.md): opening a backend
currently copies and removes inherited native mounts while the gate stabilizes
that inventory.

Prepare an immutable, recursively private backend mount namespace after startup
has fenced workloads, recovered verified host mounts, retired stale devices and
recovered interrupted journals. Reject any remaining native mount. Every native
backend enters a private clone of that clean namespace on its dedicated OS thread
before device ADD and before spawning queue threads. It first unshares filesystem
attributes, as required by `setns`. VM backends keep their existing namespace.
There is no fallback to copying the controller's current mount view.

After cancellation is published, cgroup fencing is positively confirmed and the
protection task has settled, acquire and release the global storage gate to drain
admitted artifact and project mutations. Later mutations observe cancellation
and refuse. Perform the physical close under the conversation's lifecycle mutex
and disk leases. Unmount does not establish retirement: errors and cancellation
after unmount retain the logical and physical leases through completed sealing,
nonce-verified device cleanup and private attachment retirement. Only then may
the controller release the workspace owner and publish its terminal result.

New ordinary directories remain visible in the clean namespace; new host mounts
do not. Changing storage mount topology requires a controller restart. Before ADD,
compare the state, journal and private jail directory device/inode identities and
storage submount inventory between controller and backend. A mismatch rejects
admission. The template never refreshes from a live controller mount view.

This changes ADR-0052's inherited-copy mechanism; its cgroup, flush, non-lazy
unmount, journal, grant and device-token ownership contracts remain. Startup still
recovers older backends through their original receipts. Policy and budget updates
may proceed during close; journal admission and live volume controls retain their
own synchronization. Capture, growth and close retain the same per-mount mutex.

The scope is post-execution close. Admission, initial preparation, seed transfer
and preparation-error cleanup retain their existing global gate and can still
serialize unrelated work. This decision neither identifies a live gate holder
nor recovers an already blocked kernel operation.

Validation combines deterministic physical-phase barriers with real journal and
ownership locks, private namespace tests using ordinary bind mounts, and the
existing disposable native ublk/systemd qualification. The phase tests are not
kernel-retirement qualification; the latter is required before merge. Local
measurements are distinct from production network latency.

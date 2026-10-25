# Reopening a stopped conversation disk

A VM shutdown cancels its `Volume` to release pending remote reads and writes.
Publication responses and inspections can still own that volume, or just its
`LazyDisk`, after the VM exits. The registry previously returned the cancelled
volume to a new boot. Its first writes then failed with `Disk stopped` before
reaching the journal. During ext4 journal recovery this can cause a guest panic;
the controller's later interruption error hides the original write rejection.

Reopening now waits for those references to drain before creating a fresh volume
and source. The registry tracks the journal separately so a remaining journal
reader cannot be bypassed. Healthy mounted volumes keep their existing reuse
path. Replacement also waits for journal ownership to end. The asynchronous wait
releases the registry mutex and bounded I/O permit between probes, polls at most
every 20 ms and returns an explicit unavailable error after 60 seconds. A direct
synchronous open returns that error immediately while the old readers remain.

This preserves the one-writer journal lock, durable frames, authorization checks,
publication generations and receipts described in ADRs 0007, 0021 and 0027. It
does not reset or repair a disk. The tests reproduce the failed next write before the fix, then
verify delayed reopening, unchanged prior bytes, new writes, a bounded wait,
independent disk admission and replacement fencing with a retained journal.

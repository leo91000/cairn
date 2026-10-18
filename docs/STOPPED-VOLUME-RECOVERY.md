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
does not reset or repair a disk. Persisted formats and the compatibility epoch
are unchanged. The tests reproduce the failed next write before the fix, then
verify delayed reopening, unchanged prior bytes, new writes, a bounded wait,
independent disk admission and replacement fencing with a retained journal.

## Local read/write qualification

Run alone through the credential-isolating backend launcher:

```sh
pnpm test:backend -p cairn-installation --lib stopped_volume_reopen_measurement -- \
  --ignored --nocapture --test-threads=1
```

Initial paired sample on 2026-10-17: Linux, Intel Core i9-14900K, 32 logical CPUs,
ext4 on local NVMe under `/var/tmp`, debug Rust build. The filesystem had about
303 GiB available. Other host work was running; no CPU isolation or filesystem
cache eviction was performed. Each scenario uses a fresh synthetic 4 KiB disk,
one initial 4 KiB write, a new journal reader, one first 4 KiB read, 256 warm
reads, then 16 durable 4 KiB writes and sync. The reference drops all old owners
before reopening. The recovery scenario cancels the old volume and retains it
for a synthetic 25 ms before release.

| Measurement | Normal reopen reference | Cancelled volume, delayed reader release |
| --- | ---: | ---: |
| Open including journal validation and any wait | 27.703 ms | 70.891 ms |
| First read from new reader, warm host cache | 0.094 ms | 0.058 ms |
| 256 warm reads, total | 1.169 ms | 1.144 ms |
| 16 writes and sync, total | 24.980 ms | 172.848 ms |
| Process CPU during open/read/write workload | 26.619 ms | 31.192 ms |
| Journal and metadata file bytes after workload | 137,117 | 137,117 |
| Remote encoded object bytes | 0 | 0 |

The host-cache state is warm, so the first-read result is not a cold-storage
latency measurement. Write latency is variable on the loaded host; this sample
does not establish a throughput change. The paired byte counts cover local
journal/SQLite/segment files, not an S3 encoding benchmark. There is no network
or production VM in this fixture. The synthetic reader delay and polling time
must not be presented as measured production recovery overhead.

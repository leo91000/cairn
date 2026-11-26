# Native close drainage under unrelated pool contention

[ADR-0084](adr/0084-native-attempt-access-drain.md) scopes close drainage to
the affected attempt. The benchmark compares that barrier with the old barrier
that acquired the shared storage pool gate.

## Reproduce on disposable CI

The backend workflow runs the production controller concurrency tests before
the full workspace suite. Its isolated nextest process prints one
`NATIVE_CLOSE_GATE_MEASUREMENT` JSON record. To select only that measurement:

```sh
node scripts/dev/test-backend.mjs --nextest --workspace --lib \
  -E 'package(=cairn-installation) & test(scoped_close_contention_measurement)' --no-capture
```

Eight alternating reference/candidate pairs use the same real process-safe pool
gate. An unrelated task retains it for 50 milliseconds. The reference performs
the exact former acquire/drop barrier; the candidate calls `StorageClose` with
an independent attempt's gate. Both then write, fsync and read the same 4 KiB raw
host-file bytes. Samples include admission, write/fsync, first read after a
per-file `POSIX_FADV_DONTNEED` hint, immediate warm read, process CPU, file bytes
and content hash. Process CPU is meaningful only in an isolated test process.
The record includes CI image, kernel, CPU count and workload conditions.

The first read uses a per-file cold-cache hint. Successful advice leaves
actual page-cache eviction uncertain. The fixture measures local host-file
bytes and coordination time. The disposable native storage workflow exercises
ublk, physical retirement and systemd ownership separately.

The correctness claims come from barriers and state, rather than an fsync SLA:
unrelated pool ownership cannot block the initial close drain; same-attempt
admitted access must settle; a queued or cancelled request cannot touch a closed
filesystem; cancelled and timed-out callers retain their admitted IO guards.
Real journal/lease tests, private namespace tests and disposable native
ublk/systemd qualification remain required. A fixture result does not identify
the historical pool owner or predict a production speedup.

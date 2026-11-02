# Native close drainage under unrelated pool contention

[ADR-0079](adr/0079-native-attempt-access-drain.md) narrows a synchronization
barrier. It does not change disk or object encoding, network transfer, reserve,
quota, grant, publication receipt, physical unmount or retirement algorithms.

## Reproduce on disposable CI

The backend workflow runs the production controller concurrency tests before
the full workspace suite. Its isolated nextest process prints one
`NATIVE_CLOSE_GATE_MEASUREMENT` JSON record. To select only that measurement:

```sh
node scripts/test-backend.mjs --nextest -p cairn-installation --lib \
  -E 'test(scoped_close_contention_measurement)' --no-capture
```

Eight alternating reference/candidate pairs use the same real process-safe pool
gate. An unrelated task retains it for 50 milliseconds. The reference performs
the exact former acquire/drop barrier; the candidate calls `StorageClose` with
an independent attempt's gate. Both then write, fsync and read the same 4 KiB raw
host-file bytes. Samples include admission, write/fsync, first read after a
per-file `POSIX_FADV_DONTNEED` hint, immediate warm read, process CPU, file bytes
and content hash. Process CPU is meaningful only in an isolated test process.
The record includes CI image, kernel, CPU count and workload conditions.

The first read is cold-hinted: successful advice does not prove page-cache
eviction. There is no global cache drop, block-device probe, production network
measurement or actual ublk retirement in this fixture. Raw file bytes are not
encoded S3 object bytes. Object encoding is unchanged and is qualified by the
existing reference/candidate native storage workflow, separately from this
coordination measurement.

The correctness claims come from barriers and state, rather than an fsync SLA:
unrelated pool ownership cannot block the initial close drain; same-attempt
admitted access must settle; a queued or cancelled request cannot touch a closed
filesystem; cancelled and timed-out callers retain their admitted IO guards.
Real journal/lease tests, private namespace tests and disposable native
ublk/systemd qualification remain required. A fixture result does not identify
the historical pool owner or predict a production speedup.

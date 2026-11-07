# Qualifying journal recovery

Decision: [ADR-0072](adr/0072-fast-journal-recovery-and-receipt-reconciliation.md).
Record quantitative results in the PR description and retain raw output only
in temporary files or CI artifacts, following the
[storage qualification rules](STORAGE-COMPRESSION.md#qualification).

## Workload and measured operations

The ignored `journal_reopen_measurement` test builds a synthetic append history
by alternating a nonzero byte with a sparse zero overwrite. Fixture construction
uses bounded write batches and finishes before timed operations. This exercises
obsolete frames without allocating their entire logical payload on disk; report
both logical journal bytes and allocated bytes. Sparse fixture allocation is
not a compression ratio.

Each trial opens a fresh `LazyDisk`, reads 4 KiB once, reads it another 256 times,
then writes 4 KiB sixteen times and synchronizes. Subsequent trials verify the
new bytes written by the preceding trial. Reopening is cold with respect to
the in-process journal/index and data caches; the host page cache is not flushed.

`REOPEN_PERF` JSON rows report wall-clock open latency, process CPU from
`getrusage(RUSAGE_SELF)`, bytes read during open from `/proc/self/io` (`rchar`),
first/warm data-read latency, writes-plus-sync latency and journal allocation.
Remote encoded bytes are zero: this harness makes no remote reads or uploads.
It does not measure network, compression, restore or production startup latency.

## Reproduce

Use identical toolchain/profile settings in the reference and candidate
worktrees. When the reference predates this harness, copy only
`crates/installation/src/storage/lazy/tests.rs` into its test source, retaining
all reference production code. Record both source identities and the harness
revision in the PR description. Run the harness alone in each worktree:

```sh
TMPDIR=/var/tmp CAIRN_REOPEN_BENCH_GIB=16 CARGO_BUILD_JOBS=8 \
  CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 \
  node scripts/test-backend.mjs -p cairn-installation --lib \
  journal_reopen_measurement -- --ignored --nocapture --test-threads=1
```

`CAIRN_REOPEN_BENCH_GIB` selects logical history size. The optional
`CAIRN_REOPEN_BENCH_FRAME_KIB` selects zero-frame size, a positive divisor of
4,096 KiB. Smaller frames exercise more header/index entries at the same byte
count. Compare matching workloads and profiles, recording host CPU, filesystem,
kernel, concurrent workloads and the number of trials. Include cold/warm reads,
writes, CPU and encoded bytes; report limits rather than extrapolating a local
measurement into production network performance.

Use a disk-backed temporary directory with enough free space for the reserve
configured by the tests. A small or busy tmpfs can otherwise refuse discard
admission even before the test's synthetic node quota is reached. On developer
machines with a real Cairn installation, web uninstall-command tests also need
an isolated filesystem view because their mocked cleanup still checks for an
existing `/var/lib/cairn-node` directory. Preserve the real installation.

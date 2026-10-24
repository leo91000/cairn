# Installed package approval reads

Local native observation on 2026-10-23. Reproduce alone:

```sh
CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 \
  mise exec pnpm@12.3.4 -- pnpm test:backend -p cairn-installation --lib \
  plugins::registry::measurement::compare_inline_and_offloaded_package_reads -- \
  --ignored --exact --nocapture --test-threads=1
```

The [raw samples](package-approval-read.json) compare the same real file loader
and manifest/regular-expression validation, directly and through its new
`spawn_blocking` boundary. Both load the same 263-byte JSON manifest and 9-byte
Luau file, with identical package digests. The reference runs first. Each case
writes both files six times, flushing each with `sync_all`, then loads them six
times. The first load reopens the files; subsequent loads also reopen them.
OS caches are retained throughout: this is not cold physical I/O. The first
offloaded load includes lazy blocking-pool initialization and scheduling.

| Metric, ms | Inline reference | Offloaded candidate |
| --- | ---: | ---: |
| First read wall / loader-thread CPU | 0.929 / 0.920 | 1.041 / 0.783 |
| Five warm reads median wall / loader-thread CPU | 0.250 / 0.249 | 0.431 / 0.409 |
| Six-read batch process CPU | 2.237 | 3.282 |
| Six writes median wall / process CPU | 3.057 / 0.120 | 2.837 / 0.050 |

Read wall time includes file loading, validation, digest computation and, for
the candidate, blocking-pool scheduling. Loader CPU is sampled inside the thread
which performs that work. Process CPU includes all threads in this test process;
batch totals are preferable to individual short asynchronous samples. All
samples are retained. No throughput or tail percentile is inferred from six
loads, and the unchanged writes' timing differences establish no improvement.

This moves synchronous file I/O and validation off nested form/credential stacks.
It adds no stored field, table, envelope, file payload or write operation.
The measurements exclude approval-record SQL reads, authorization, encryption,
metadata bytes, database pages, Luau execution and OAuth/network requests. They
observe a small package, not maximum-size packages, concurrent admission or a
production installation. The captured approval is checked again after the blocking
read: an installed-package lookup therefore uses two approval-record reads instead
of one. These SQL reads are excluded from this loader observation. The original
approval snapshot and effect/commit rechecks remain required.

The motivating default-stack regression is
`owner_resource_forms_never_retry_an_uncertain_parent_refresh`, through the actual
owner HTTP router and native worker. Before the correction, GDB identified
2,092,672 bytes of nested stack at the overflow, including a 494,144-byte router
frame; no recursive renewal loop was present. Moving only individual credential
futures to the heap did not fix it and those experiments were removed. The
blocking boundary first let that regression complete, but adding the final
approval-record recheck exposed another overflow in the same path, this time in
owner-policy HTTP. GDB measured a 620,144-byte management-router frame and
2,092,384 bytes of nested stack. The router now constructs its direct-management
future in a separate boxed function before polling it, so unrelated OAuth and
configuration branches do not occupy the stack of a running worker form. No new
task is spawned: the current task's authority remains in scope. The same test
then completes with the default stack and unchanged deadlines (0.73 seconds of
execution), retaining the final approval recheck. It still proves that an
uncertain refresh is not repeated for either an active or an inert parent. Broad
integration qualification is recorded separately.

Host: native Linux 7.2.9-arch1-1 x86-64, Intel Core i9-14900K, 32 logical CPUs
reported by `lscpu`, 12 available to Rust. Rust 1.97.1 uses the unoptimized test
profile with debug information disabled. Temporary files are on `/tmp`, whose
filesystem reports `ext2/ext3`. No other test/build was launched by this agent
during this observation. Unrelated host activity, caches and scheduling remain
uncontrolled. This does not qualify a production data transition.

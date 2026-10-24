# Accepted events during authority outages

A native owner-policy failure retains its origin through route binding and
capability checks. Before an external effect begins, its 503 returns to the
supervisor and the admitted event stays queued with its existing payload, routing
and handler-attempt count. Replacing the worker and rechecking authority do not
spend the package's five-error budget. HTTP mutation sends and program execution
mark the invocation before the effect starts; subsequent failures retain the
existing finite package-error handling. Generic provider errors do not acquire
this native authority classification. HTTP receipts continue to prevent replay
of an uncertain mutation. Explicit permission failures retain their checks.

The real-worker regression first reproduced deletion after five unavailable
lookups (0.42s). A second regression reproduced the same loss when route binding
succeeded but the first capability's authority check failed (0.46s). On the
corrected source, six unavailable pumps retain the same admission across each of
the four tested pre-submission checks. Restoring the same owner permits one
conversation submission and source redelivery remains a duplicate. Separate
real-worker witnesses confirm that five deliberate Luau errors still remove a
broken event and that a committed HTTP write followed by authority failure keeps
its pending receipt, finite error budget and exactly one provider request. These
tests exercise signed ingress, the worker and native submission, with a synthetic
authority server; they do not exercise a real Slack workspace or final provider
answer.

No persisted field, key family, index, encryption envelope or version is added.
This remains inside the unqualified epoch-14 / SQLite-13 draft boundary. Existing
queue capacity remains 50 events per instance. Retained admissions continue to
occupy that capacity during outages, as intended; the old deletion was data loss.

## Storage observation

```sh
pnpm test:backend -p cairn-installation --test plugin_routes \
  authority_outages::measure_authority_outage_queue_storage \
  -- --ignored --exact --nocapture --test-threads=1
```

Use pinned pnpm 12.3.4 and Rust 1.97.1. The observation passed in 1.453s including
the launcher for the earlier pre-dispatch-only observation; those
[earlier samples](PLUGIN-AUTHORITY-OUTAGE.local.json) are retained. A second
observation on the native authority/effect-tracking source passed in 1.677s.
Its [raw samples](PLUGIN-AUTHORITY-OUTAGE.native-authority.local.json) compare 50
synthetic native Store events in the old fourth-error shape with the same events
retained unchanged. Both shapes encode to **876 JSON bytes per event / 43,800
bytes for 50**. The reference's attempt counter and later deadline use the same
encoded width. No actual message content is present in these samples.

Each case reopens the Store after seeding. One read actor executes the first
connection-cold read and five warm reads, followed by six same-value writes.
The OS page cache remains warm. Read timings exclude actor scheduling; write
timings include it. CPU samples use the process clock. All columns below are ms;
warm and write columns show medians.

| Shape | Cold wall | Cold CPU | Warm wall | Warm CPU | Write wall | Write CPU |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Handler-retry reference | 0.567057 | 0.893948 | 0.015621 | 0.015941 | 4.219580 | 0.260551 |
| Authority-retained | 0.598723 | 1.125287 | 0.032020 | 0.033180 | 3.606026 | 0.222827 |

Host: Linux 7.2.9-arch1-1/glibc 2.44, Intel Core i9-14900K, 12 available threads.
Cgroup CPU/memory limit files were unavailable. Load was 14.09/19.18/20.24; I/O
pressure over 10s was some 32.32% / full 25.22%. Case order was fixed and unrelated
host load uncontrolled. These short samples establish neither a speedup nor a
production latency bound. A private software emulator was running during the
second observation; there were no other task-owned builds or tests.

Serialized bytes exclude SQLite pages, WAL and indexes. These measurements cover
the unchanged Store record shapes and read/write primitives, not the runtime
dispatch/retry selector, avoided retry writes, authority/network calls, VM startup,
provider work, end-to-end recovery or production forward migration.

# Observed completion read marks

The unread review changes each read watermark from the request's current time to
its observed completion time. Writes keep the maximum previous watermark, so an
older request cannot consume unseen work or lower a newer mark. The existing
SQLite `kv` encoding and account prefix remain unchanged.

Reproduce the local storage comparison with:

```sh
CARGO_INCREMENTAL=0 pnpm test:backend -p cairn-installation --test reads read_watermark_measurements -- --ignored --nocapture
```

The ignored benchmark uses the real `Store` and `Db` implementation and compares
200 unconditional `Db::set` calls (the reference) with 200 `Db::kv_as` plus
monotonic `Db::set` calls in a transaction. Each variant makes 100 write
transactions and 100 warm reads of the account's marks. Cold reads use reopened
SQLite connections and empty statement caches; the OS page cache stays warm.
CPU uses `CLOCK_PROCESS_CPUTIME_ID`, including the SQLite actor threads.

Local run: Linux 6.12.109 x86_64, KVM VM, Intel Xeon, 12 exposed CPUs, 30 GiB RAM,
Rust 1.97.1 debug build, SQLite WAL on the run's overlay filesystem. Browser and
Gradle validation were running concurrently; these timings are diagnostic,
not production latency or isolated host benchmarks.

| 200 marks | Reference overwrite | Observed monotonic watermark |
| --- | ---: | ---: |
| Encoded key + JSON value bytes | 13,400 | 13,400 |
| Cold read, ms | 0.469 | 0.403 |
| Warm read mean, ms | 0.224 | 0.213 |
| Write transaction mean, ms | 0.737 | 1.503 |
| Read CPU per batch, ms | 0.227 | 0.216 |
| Write CPU per batch, ms | 0.741 | 1.507 |

The byte count excludes SQLite page, index and WAL overhead. The comparison
measures local database work, without HTTP, Beacon, relay or production network
latency. Client requests now carry `{ key, finishedAt }` objects and are split
into batches of at most 200; legacy string-key requests remain accepted.

## Blocked mission dismissals and snoozes

Reproduce the dismissal storage comparison with:

```sh
pnpm test:backend -p cairn-installation --test reads dismissal_storage_measurements -- --ignored --nocapture
```

This benchmark uses the real `Store` and SQLite `Db` methods. Both variants seed
one account watermark and 200 task read marks. The reference performs 100 batches
of 200 monotonic read-mark writes, then reads the watermark and read-mark prefix.
The new variant instead writes 200 dismissal records per transaction, half dismissed
and half snoozed for 24 hours, and reads the additional dismissal prefix. Reads
use the same typed deserialization as the account projection; feed construction,
summary lookup and conversion to lookup maps are excluded from both variants.
There are 100 write transactions and 100 warm reads per variant. The cold read
reopens SQLite connections and statement caches; the OS page cache stays warm.

Local run on 2026-10-20: Linux 6.12.109 x86_64, KVM VM, Intel Xeon, 12 exposed
CPUs, 30 GiB RAM, Rust 1.97.1 debug build, SQLite WAL on the run's overlay
filesystem. Android's full test/lint validation ran concurrently. Process CPU
uses `CLOCK_PROCESS_CPUTIME_ID` and includes SQLite actor threads. Timings are
diagnostic local measurements, without HTTP, relay, Beacon or production network
latency.

| Per-account workload | Reference: 200 read marks | New: 200 marks + 200 dismissals |
| --- | ---: | ---: |
| Encoded key + JSON value bytes, including watermark | 13,431 | 34,331 |
| Cold prefix read, ms | 0.549 | 0.751 |
| Warm prefix read mean, ms | 0.217 | 0.544 |
| Write transaction mean, ms | 1.586 | 0.886 |
| Read CPU per batch, ms | 0.223 | 0.551 |
| Write CPU per batch, ms | 1.582 | 0.891 |

The added records occupy 20,900 encoded bytes for this fixture, excluding SQLite
page, index and WAL overhead. Dismissal writes do not need the monotonic read
required for existing read marks, while reads add one prefix scan to account
projections, including conversation snapshots. This fixture measures an account
with 200 saved dismissals; it does not model long-term accumulation or estimate
production response times.

Compatibility: dismissals use the new `dismissed:<account>:task:<id>` namespace.
Existing read marks and their JSON encoding stay unchanged. Previous releases
ignore the new keys, and the new release accepts installations without them.
The additive outcome/requester fields remain optional; the compatibility epoch
therefore stays unchanged.

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

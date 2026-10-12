# Local measurements for readable slugs

Measured on 2026-10-12 with
[`scripts/measure-readable-slugs.py`](../scripts/measure-readable-slugs.py).
The reference omits the slug field and its unique index. Both cases use 1,000
records, 5,000 warm reads and 100 individually committed writes. SQLite writes
update conversations; the slug variant reads and preserves the stored slug.
Postgres writes create installations using the actual migration's generator
and unique index. No production accounts or installation data are used.

Reproduce against a disposable local Postgres database:

```sh
CAIRN_SLUG_MEASUREMENT_DATABASE_URL='postgres://<local-test-database>' \
  uv run scripts/measure-readable-slugs.py
```

The script removes its isolated Postgres schema and temporary SQLite files.
Its pinned measurement dependency does not change the application dependencies.

## Host and limits

Linux 6.12.109 x86_64, glibc 2.36, 12 visible CPUs; SQLite 3.53.1,
Postgres 15.19, TCP loopback. SQLite uses WAL with FULL synchronization;
Postgres uses ordinary WAL-logged tables with `fsync=on` and
`synchronous_commit=on`. The test databases are on `/tmp` **tmpfs**, so committed
write times describe this VM's memory-backed filesystem, not physical disks.
Two Rust compilation workers were running concurrently. These are local
comparisons, not production network, relay, disk or end-to-end application latency.

“First read” is a fresh SQLite connection (20 samples, including opening the
connection) or the first Postgres lookup (one sample). It includes statement/cache
initialization; OS buffers remain warm. Physical cold-cache reads were not
measured. Warm SQLite slug reads include the alias-to-UUID lookup followed by
the existing UUID read. Postgres reads still use UUIDs, selecting the added
slug as the account session does. Postgres's first read excludes connecting.

## Results

Latencies are medians, in microseconds.

| Measurement | Reference | With slugs |
| --- | ---: | ---: |
| SQLite encoded conversation JSON, bytes | 101 | 131 |
| SQLite database + indexes, bytes after checkpoint | 221,184 | 286,720 |
| SQLite first read, µs | 54.96 | 62.75 |
| SQLite warm read, µs | 1.57 | 3.10 |
| SQLite update + commit, µs | 5.72 | 10.14 |
| Postgres encoded installation row, bytes | 82 | 101 |
| Postgres first read, µs | 154.18 | 211.47 |
| Postgres warm read, µs | 21.00 | 21.32 |
| Postgres insert + commit, µs | 23.68 | 28.19 |

SQLite CPU time is the measurement process's user + system time, including
Python, JSON encoding and database operations. Postgres CPU time is its backend's
user + system time, read from `/proc`, with 10 ms resolution.

| CPU total for the workload | Reference | With slugs |
| --- | ---: | ---: |
| SQLite 20 first reads, ms | 1.16 | 1.36 |
| SQLite 5,000 warm reads, ms | 8.91 | 17.35 |
| SQLite 100 updates, ms | 0.72 | 1.20 |
| Postgres 5,000 warm reads, ms | 40 | 40 |
| Postgres 100 inserts | below 10 ms resolution | below 10 ms resolution |

The fixed example `quiet-cedar-00000000` adds 30 bytes to a compact conversation
JSON record. Actual names vary slightly in length; Postgres row size also varies
with alignment. The additional SQLite lookup and index maintenance have measurable
local cost, while existing UUID reads retain their existing lookup. These small,
memory-backed measurements cannot establish production latency or throughput.

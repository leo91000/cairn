# Local measurements for readable slugs

Measured on 2026-10-12 with
a standalone probe (since retired).
The reference omits the slug field and its unique index. Both cases use 1,000
records, 5,000 warm reads and 100 individually committed writes. SQLite writes
update conversations; the slug variant reads and preserves the stored slug.
Postgres writes create installations using the actual migration's generator
and unique index. No production accounts or installation data are used.

Reproduce against a disposable local Postgres database:

The standalone measurement probe has been retired.

The script removes its isolated Postgres schema and temporary SQLite files.
Its pinned measurement dependency does not change the application dependencies.

## Host and limits

Linux 6.12.109 x86_64, glibc 2.36, 12 visible CPUs; SQLite 3.53.1,
Postgres 15.19, TCP loopback. SQLite uses WAL with FULL synchronization;
Postgres uses ordinary WAL-logged tables with `fsync=on` and
`synchronous_commit=on`. The test databases are on `/tmp` **tmpfs**, so committed
write times describe this VM's memory-backed filesystem, not physical disks.
No compilation workers were running during this measurement; the local browser test harness was starting concurrently. These are local
comparisons, not production network, relay, disk or end-to-end application latency.

“First read” is a fresh SQLite connection (20 samples, including opening the
connection) or the first Postgres lookup (one sample). It includes statement/cache
initialization; OS buffers remain warm. Physical cold-cache reads were not
measured. Warm SQLite slug reads include the alias-to-UUID lookup followed by
the existing UUID read. Postgres account reads still use UUIDs, selecting the added
slug as the account session does. The additional hostname-resolution query uses
the unique slug index before the existing installation API queries. Postgres's first read excludes connecting.

## Results

Latencies are medians, in microseconds.

| Measurement | Reference | With slugs |
| --- | ---: | ---: |
| SQLite encoded conversation JSON, bytes | 101 | 131 |
| SQLite database + indexes, bytes after checkpoint | 221,184 | 286,720 |
| SQLite first read, µs | 45.55 | 61.18 |
| SQLite warm read, µs | 1.57 | 3.06 |
| SQLite update + commit, µs | 5.58 | 10.48 |
| Postgres encoded installation row, bytes | 82 | 103 |
| Postgres first read, µs | 149.78 | 157.59 |
| Postgres warm read, µs | 21.19 | 21.37 |
| Postgres warm hostname resolution, µs | — | 21.02 |
| Postgres insert + commit, µs | 22.90 | 28.04 |

SQLite CPU time is the measurement process's user + system time, including
Python, JSON encoding and database operations. Postgres CPU time is its backend's
user + system time, read from `/proc`, with 10 ms resolution.

| CPU total for the workload | Reference | With slugs |
| --- | ---: | ---: |
| SQLite 20 first reads, ms | 1.05 | 1.44 |
| SQLite 5,000 warm reads, ms | 8.81 | 17.03 |
| SQLite 100 updates, ms | 0.66 | 1.25 |
| Postgres 5,000 warm reads, ms | 50 | 50 |
| Postgres 5,000 hostname resolutions, ms | — | 40 |
| Postgres 100 inserts | below 10 ms resolution | below 10 ms resolution |

The fixed example `quiet-cedar-00000000` adds 30 bytes to a compact conversation
JSON record. Actual names vary slightly in length; Postgres row size also varies
with alignment. The additional SQLite lookup and index maintenance have measurable
local cost, while existing UUID reads retain their existing lookup. These small,
memory-backed measurements cannot establish production latency or throughput.

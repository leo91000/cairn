# Unattributed question retirement: storage shape

Measured locally on 2026-10-20. This is a one-time compatibility operation before
native/plugin workers start. No question content or answer is added to its notice
or audit. Closed and attributed questions remain unchanged. The ordinary startup
path subsequently reads only its marker; it does not rescan question history.

Reproduce with Python's standard library:

```sh
python3 scripts/bench-question-retirement.py --context 'Describe concurrent host work'
```

The [raw observation](question-turn-migration.json) uses 128 synthetic pending
questions across eight conversations. The reference reads the same prefix and
leaves every record unchanged. The candidate writes the cancelled/nonblocking
records with their retirement reason, eight content-free notices, eight audits
and one marker. It retains the original question fields. Both use WAL, synchronous
FULL and 4 KiB pages. Each transaction sample uses an independently seeded database;
seed and connection creation are outside its timer. There are 30 transaction,
1,000 warm-marker and 50 new-connection marker samples per variant. New connections
retain the OS cache; these are not physical cold reads.

| Measurement | Reference | Candidate |
| --- | ---: | ---: |
| KV encoded JSON bytes | 44416 | 50308 |
| Event text bytes | 0 | 1352 |
| Audit JSON bytes | 0 | 528 |
| SQLite bytes after checkpoint | 98304 | 110592 |
| One-time transaction p50 / p95, ms | 1.658 / 2.000 | 7.487 / 8.993 |
| One-time transaction CPU total, ms | 50.402 | 220.695 |
| Warm marker read p50 / p95, ms | 0.012 / 0.019 | 0.015 / 0.030 |
| Warm marker read CPU total, ms | 12.270 | 15.807 |
| New-connection marker read p50 / p95, ms | 0.230 / 0.331 | 0.336 / 0.415 |
| New-connection marker read CPU total, ms | 12.226 | 16.412 |

This Python/SQLite shape benchmark excludes Rust, the database actor, other startup
migrations, chat/run lookup, queued-answer cancellation, full production indexes
and foreign keys, authentication, relay and provider I/O. Encoded payload counts
exclude key strings, table metadata and SQLite overhead; file bytes are reported
separately. The reference's empty marker lookup is compared with the candidate's
present marker. Reference runs first, so cache and shared-host scheduling can
change the timings. This is neither a startup latency promise nor a claimed
speedup. Full prior-production-data and release qualification remains required.

Host: Linux 6.12.109, Python 3.14.7, SQLite 3.53.1, 7 CPUs. MemTotal:       19462272 kB; MemAvailable:   15675988 kB. Regression qualification pipeline active (formatting then native test build); no browser, Gradle, emulator or Postgres.

# Local SQLite compatibility-marker observation

The Slack candidate reserves epoch 14 / SQLite 13 after Main adopted epoch 13 /
SQLite 12 for owner-reviewed native administrator mode. No automatic transition
is declared. This observation isolates the marker write, without measuring
`Store::open`, `Service::new`, native mode, encryption, provider traffic or recovery.

Reproduce from the repository root:

```sh
python3 scripts/measure-plugin-compatibility-fence.py --output docs/performance/PLUGIN-COMPATIBILITY-FENCE.local.json
```

The fixture uses 100 synthetic resource JSON records in a local SQLite `records`
table. The reference commits version 12 to an existing version-12 file; the
candidate commits version 13 after resetting to 12 outside each timed interval.
Each shape executes 100 measured writes, 100 reset writes, 1,000 warm reads and
100 reads through new connections. WAL and synchronous FULL are explicit.
Record equality is checked after all writes, before reading the marker.

| Observation | Reference marker 12 | Candidate marker 13 |
| --- | ---: | ---: |
| Encoded record JSON bytes | 15990 | 15990 |
| Database bytes after checkpoint | 32768 | 32768 |
| WAL bytes before checkpoint | 824032 | 824032 |
| Write median / p95 ms | 5.3242 / 5.7578 | 5.1939 / 9.4331 |
| Warm read median / p95 ms | 0.001956 / 0.002091 | 0.003221 / 0.003393 |
| New-connection read median / p95 ms | 0.04390 / 0.04790 | 0.11546 / 0.13706 |
| Write/reset CPU seconds | 0.019122 | 0.019374 |
| Warm-read CPU seconds | 0.001988 | 0.003421 |
| New-connection CPU seconds | 0.004515 | 0.009836 |

Linux x86_64 on this native run's managed workspace filesystem; Python
3.14.8, its SQLite 3.54.0, host load average
7.65, 8.70, 12.29. No host caches or services
were changed. New connections give cold SQLite connection state, with OS cache
uncontrolled; warm reads retain one connection. CPU includes Python overhead and
reset transactions. The reference runs before the candidate, with no randomization
or isolation from other host work. Differences are observations, not evidence of
an intrinsic speedup or regression. This is the Python SQLite driver, not a
benchmark of the Rust bundled driver or production/S3 network latency.

The native `block_codec_migration` suite separately observes the actual Store
preserving native review, resource, route and opaque KV fixtures while moving
from version 12 to 13 and refusing a future version. That proves neither complete
service startup nor encrypted OAuth restoration. Whole-PR forward transition,
recovery, image qualification and independent compatibility review remain gates.

# External OAuth event receipts

A native event that has already started personal consent must not replace a newer
request when retried. Its receipt contains the pending-consent key and requested
scopes, without a token, code or raw state. Creation shares the pending-consent
transaction and verifies that the exact admitted native event still exists.
Successful, exhausted or obsolete event removal and instance deletion remove its
receipts in the same transaction. The pending consent keeps its independent TTL.

Reproduce the synthetic index-shape comparison with:

```sh
python3 scripts/bench-requester-control.py --context 'Describe concurrent host work'
```

The `oauth_event` section of the [raw observation](oauth-event-replay.json)
compares one existing subject/provider flow index with that index plus one event
receipt. UUIDs, subject digest, pending key and scope are synthetic. Reads compare
one flow lookup with receipt plus flow lookups. Writes upsert the index and,
for the candidate, its receipt in one immediate transaction. The existing KV
schema, expiry-aware lookup and upsert shapes are used. No native host or provider
is called. Encrypted pending consent, authorization lookup, event/source checks,
fences, audit and ordinary chat writes are excluded from these timings.

| Measurement | Reference | Candidate |
| --- | ---: | ---: |
| KV rows | 1 | 2 |
| Encoded JSON bytes | 142 | 264 |
| Key bytes | 137 | 303 |
| SQLite bytes after checkpoint | 12,288 | 12,288 |
| New connection read p50 / p95, ms (50 samples) | 0.597 / 0.929 | 0.550 / 0.756 |
| New connection read CPU total, ms | 32.079 | 27.689 |
| Warm read p50 / p95, ms (1,000 samples) | 0.023 / 0.035 | 0.046 / 0.075 |
| Warm read CPU total, ms | 25.767 | 48.771 |
| Write commit p50 / p95, ms (100 samples) | 0.024 / 0.039 | 0.013 / 0.029 |
| Write commit CPU total, ms | 2.649 | 1.501 |

Measured on 2026-10-19 in the same Linux VM as
[requester control](REQUESTER-CONTROL.md). Raw data records exact Python/SQLite,
memory, load and CPU conditions. Only Rust formatting ran concurrently; no
Android, device, native test or PostgreSQL traffic. WAL, synchronous FULL and
4 KiB pages were used on a temporary filesystem. New-connection reads retain
OS caches; these are not physical cold reads. The reference runs first. These
Python/SQLite shape measurements describe added storage and local overhead;
they establish no native or production-network latency guarantee.

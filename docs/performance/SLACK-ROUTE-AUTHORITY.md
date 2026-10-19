# Route authority record overhead

Measured locally on 2026-10-18. The reference is the route record shape before
per-person authority generations; the candidate adds those generations and the
captured person to event/delivery bindings. Both use synthetic, non-secret data.

Reproduce with Python's standard library:

```sh
python3 scripts/bench-route-authority.py --context 'Describe concurrent host work' > route-authority.json
```

The [raw observation](slack-route-authority.json) records the kernel, Python,
SQLite, memory, load, sample counts, CPU and database pragmas. This is a
Python JSON/SQLite shape microbenchmark using `Store`'s record lookup/upsert and
audit transaction shapes. It does **not** measure Cairn's Rust serializer,
authorization checks, hashing, database actor, provider calls or network latency.

The workload is 256 routes with 100 listed people each, 50 queued events and
20 deliveries. This deliberately exercises the largest per-route generation map:
100 approvals after route creation. Initial grants share the route's generation
and do not require map entries. Removed people leave no generation-map entry;
turnover does not create an accumulating revocation table.

| Measurement | Reference | Candidate |
| --- | ---: | ---: |
| Encoded JSON bytes, all records | 1,532,784 | 2,797,624 |
| First route, bytes | 5,827 | 10,747 |
| First queued event, bytes | 581 | 657 |
| SQLite bytes after checkpoint | 1,687,552 | 3,268,608 |
| New connection read p50 / p95, ms (50 samples) | 0.940 / 1.116 | 0.958 / 1.495 |
| Warm read p50 / p95, ms (1,000 samples) | 0.154 / 0.269 | 0.267 / 0.400 |
| Write + audit commit p50 / p95, ms (100 samples) | 0.701 / 1.274 | 0.427 / 0.936 |
| New connection read CPU total, ms | 45.741 | 47.646 |
| Warm read CPU total, ms | 169.320 | 299.036 |
| Write + audit commit CPU total, ms | 76.453 | 67.821 |

"New connection" is cold only with respect to the SQLite connection; OS page
caches were retained. Writes include JSON encoding, a record upsert, an audit
insert and an immediate transaction commit; fixture copying is outside the
measurement. The VM ran one serial Rust compilation concurrently, with about
970 MiB available at sampling, and no PostgreSQL or emulator. SQLite used WAL,
4 KiB pages and synchronous FULL. The reference ran first, so caching and shared
host scheduling affect the comparison. In particular, the lower candidate write
time is **not** evidence of an improvement. The repeatable cost is the bounded
record growth; these local timings establish no production latency guarantee.

## Format decision for this slice

The published reference was checked at tag `v0.57.0`, commit
`1cd10ca9d14b5cf57efae4cebb9e8ba5095644a4`. It has no `plugins/routes.rs` or typed
resource-route bindings: those records are introduced within this draft PR.
Its existing connection records tolerate unknown fields, and event/delivery data
is read as JSON values. This slice adds a bounded, optional `personRevisions` map
to the new route type and an optional `person` proof inside its new binding type.
Empty maps and unbound transport/owner notifications omit those fields.

Newly admitted personal work always carries the proof. Earlier development
snapshots' unversioned personal bindings are refused before plugin delivery or
effects; they require a fresh source request. History, root instance delegation
and vault entries are not rewritten or removed. No forward data conversion is
needed for these fields; a failed start can be corrected forward while preserving
the installation directory. Reapproval must never revive an old binding.

After integrating main at `d01b519896c62ab3e51295eb702195975a2414ec`, this
slice leaves its inherited compatibility metadata at epoch **4**, with its recorded
legacy tree unchanged. The published reference inspected above is epoch **2**;
the production contract's separate epoch transition review must remain gated before
deployment. These measurements and the additive-field decision are not whole-PR
compatibility or deployment approval. The later authorization guards change no
encoded records. Their native read/write and provider-I/O costs still require
qualification with the complete Slack delivery; the shape benchmark above does
not measure them.

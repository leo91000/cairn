# Owner-answer provenance overhead

The owner can answer a question without taking over its author's personal OAuth.
The host records the exact answer's chat, run and initiating turn separately from
credential suspension. This lets the native worker deliver that answer while
refusing unrelated anonymous inputs. The original author retains control; their
personal credentials stay suspended for the turn, including after wakeup.

The marker uses the existing chat-control KV namespace. An edit or withdrawal
retires it in the same message transaction. Conversation deletion clears the
whole namespace. Retention follows the conversation; there is one marker per
owner-controlled answer, with no separate journal or provider token in it.

Reproduce the storage-shape measurement with Python's standard library:

```sh
python3 scripts/bench-requester-control.py --context 'Describe concurrent host work'
```

The [raw observation](requester-control.json) compares the R16 suspension-only
records with the added exact-answer marker. It uses Store's KV schema, upsert,
expiry-aware read and delete shapes with synthetic UUIDs. The measured read is
an owner-answer check after suspension: one lookup before, three after. The write
updates the suspension flag, deletes the answer's requester proof, and adds the
candidate marker. Unchanged audit and ordinary chat writes are excluded.

| Measurement | Reference | Candidate |
| --- | ---: | ---: |
| Stored KV rows | 2 | 3 |
| Encoded JSON bytes | 104 | 243 |
| Key bytes | 235 | 342 |
| SQLite bytes after checkpoint | 12,288 | 12,288 |
| New connection read p50 / p95, ms (50 samples) | 0.529 / 0.699 | 0.581 / 0.957 |
| Warm read p50 / p95, ms (1,000 samples) | 0.012 / 0.031 | 0.066 / 0.111 |
| Write commit p50 / p95, ms (100 samples) | 0.014 / 0.033 | 0.037 / 0.077 |
| New connection read CPU total, ms | 26.101 | 29.569 |
| Warm read CPU total, ms | 21.676 | 69.126 |
| Write CPU total, ms | 2.236 | 10.090 |

Measured on 2026-10-19 in a Linux VM with 7 CPUs, 19,462,272 KiB memory and about
5,074,964 KiB available. Android APK and native regression compilation ran
concurrently. SQLite used WAL, synchronous FULL and 4 KiB pages on the VM's
temporary filesystem. New connections retain OS caches; they are not physical
cold reads. Reference runs first, so shared-host scheduling and caching affect
timings. This Python/SQLite microbenchmark measures neither Cairn's Rust runtime,
database actor, authentication nor network/provider latency. It establishes no
production latency guarantee or claimed speedup.

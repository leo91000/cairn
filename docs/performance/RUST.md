# Rust backend measurements

The native backend substantially reduces manager memory and improves concurrent
throughput. It is not faster on every request: the single-client read scenarios
below remain slightly slower. These are local measurements, not production SLAs.

## Method

Run `cargo build --locked --release --bin cairn`, then
`node --import tsx scripts/benchmark-backend.mjs`. Set `TMPDIR` to a directory on a
real disk when `/tmp` is tmpfs. The recorded run used ext4, an Intel i9-14900K,
Linux 7.2.3, Node 26.8.1, Rust 1.97.1 and two Tokio worker threads. Production's
Node agent toolkit uses Node 24; this comparison does not establish its performance.

Both servers receive a fresh copy of the same SQLite fixture: 1,000 completed runs
and 10,000 structured events. Requests use a valid session, exercise the real HTTP
routes, and check status and result counts. Compression is disabled for both.
There are 20 warmups, then 240 measured requests, with concurrency 1 or 16. Each
scenario runs three times, alternating server order. Values below are medians of
those three runs. There were no concurrent builds or browser suites during the
final measurement. The client and server share the same host.

The scenarios read a 100-run page, read a 500-event page, or create an agent with
its audit entry. Worker execution is disabled equally; model inference, provider
latency, project builds and long-lived agent memory are outside this measurement.
RSS is the server process after the scenario, not total system memory. Startup
includes the former backend's TypeScript loader, as its production command did.

## Results

| Scenario | Clients | Node req/s | Rust req/s | Node p95 ms | Rust p95 ms | Node RSS MiB | Rust RSS MiB |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Run page | 1 | 337 | 298 | 4.74 | 4.83 | 190.5 | 22.2 |
| Event page | 1 | 286 | 275 | 5.98 | 6.60 | 211.1 | 22.5 |
| Agent + audit write | 1 | 169 | 255 | 7.45 | 5.14 | 172.3 | 22.1 |
| Run page | 16 | 1,816 | 3,890 | 15.39 | 14.35 | 189.5 | 23.4 |
| Event page | 16 | 870 | 1,977 | 30.70 | 14.33 | 226.1 | 24.9 |
| Agent + audit write | 16 | 249 | 631 | 119.81 | 29.57 | 173.1 | 22.5 |

At concurrency 16, throughput is 2.1–2.5 times the reference and manager RSS is
about 88–89% lower. Median startup per scenario is 22–40 ms versus 322–354 ms.
Single-client run/event throughput is about 12%/4% lower; crossing database actor
queues has a cost even though it keeps SQLite work off the HTTP executor.

Raw measurements: [final results](rust-backend.json).

## Optimizations retained

- One bounded SQLite writer and two readers allow concurrent reads while keeping
  writes ordered. WAL, indexed filters and prepared statement caches remain on.
- Run and event pages serialize stored JSON directly on the database actor,
  avoiding a second object tree and work on the HTTP executor.
- Record writes and audit entries share a transaction. Event retention is batched
  instead of deleting old rows on every event.
- A bounded HTTP client pool reuses validated MCP connections. DNS and private
  network policy are still checked before every request.
- Two Tokio worker threads are the default; `CAIRN_HTTP_THREADS` can override it.
  An earlier two-versus-eight-thread trial showed similar concurrent throughput
  (run pages 3,308/3,270; event pages 1,614/1,653; writes 658/651 req/s). Eight
  threads did not provide a consistent benefit. These trials preceded the final
  run-page serialization optimization and should not be compared as isolated
  evidence of that change: [two threads](threads-2-trial.json),
  [eight threads](threads-8-trial.json).

The measurements are short, have three repeats and show normal host variability.
They demonstrate reduced backend overhead under these workloads, not a maximum
possible throughput or a guarantee that model-driven tasks finish faster.

# Disk publication transfer diagnostics

`cairn_performance` emits `disk_publication_transfer` summaries for each transfer
attempt. `publication_attempt` is a fresh UUID per capture, independent of the
runner attempt and the snapshot UUID. The enclosing `disk_publication_attempt`
span correlates existing manager phase events, including queueing and failures
before transfer. Filter by this UUID, then by `side` (`manager` or `node`). Do not
subtract process-wide `s3_totals` or `blockCodec` counters to attribute concurrent
publications. `uploadedBytes` remains plaintext, not encoded S3 traffic.

No disk content, hash, object key, path, endpoint, credential or grant is logged.
Each summary has a fixed set of stages. No per-block events or stored samples.
The node aggregates successive batches under (attempt, run, snapshot), with at
most 64 entries, ten minutes idle expiry checked on subsequent requests, and
oldest-entry eviction. Snapshot discard also closes partial aggregates when the
manager fails between successful batches. It logs at the final batch, on stream failure/drop, at most
once per minute at intermediate batch boundaries, and on expiry/eviction. Partial
entries can remain silently until the next request or process exit. Eviction or
expiry produces a partial summary; later batches can start another segment with
the same identities. These segments must not be added as cumulative snapshots.

The manager sends optional diagnostic JSON only after the node advertises
`publicationDiagnostics` in its transient snapshot response. Old nodes still
serve unchanged requests; only manager timings are then available. The existing
authentication, disk grants, membership checks and fencing remain authoritative.
A node's base-block requests carry two UUID headers. The manager attaches their
S3/codec measurements to an active transfer only when both UUIDs match and the
existing disk grant authorizes that run. Headers grant no access. Coalesced GETs
attribute physical work to the initiating attempt, while each consumer records
its own inclusive `getCoalesced` wait; bytes there are consumer volumes, not
additional S3 traffic.

Offline converted-environment publications use the same attempt span and manager
summary lifecycle, with `source="converted_environment"` on the span. Their existing
serial read/hash/codec/PUT loop remains serial, so no producer upload-capacity wait
or final parallel drain is needed. Failures before transfer still emit the correlated
operation phase; transfer errors and dropped futures retain their partial summary.

## Units and interpretation

All `*Us` fields are **wall-clock microseconds**, including synchronous codec jobs.
They include scheduling/preemption and are not CPU time. Use a process/thread CPU
clock or a profiler for CPU consumption. The overhead qualification separately
measures process CPU. Each stage reports started/completed/failed-or-cancelled
operations, successful `bytes`, summed elapsed wall time, current and maximum
in-flight operations. A failure or dropped sample contributes elapsed time and a
failure count, with zero successful bytes. An unfinished operation's elapsed time
is included in a live/cancelled summary and `inFlight` stays visible. Detached
blocking work can finish after a cancelled manager summary; this is a partial
observation, not proof of completion or a final byte total.
`completed` describes the measured operation, not acceptance of its data. Hash
computations count even when a later integrity comparison rejects the block or
journal record; existing integrity checks and publication outcomes remain the
authority for acceptance.

- `wallUs`: elapsed transfer phase (node: since first request in this segment,
  including gaps between batches).
  On the manager this diagnostic clock starts at collector construction and is
  snapshotted before encoding/emitting the summary. The existing enclosing
  `disk_publication` / `phase_ms` clock also includes diagnostic setup/emission;
  the two clocks therefore need not match exactly. Their difference is not an
  additional measured storage sub-phase.
- `observedUnionUs`: union of all measured intervals on that side.
- `unattributedUs`: `wallUs - observedUnionUs`; unmeasured coordination, allocation,
  iteration, scheduling gaps, and on the node gaps between requests/stream polls.
- `operationWallUsSum`: sum of stage intervals. Stages include nested envelopes
  (`baseRead`, `getCoalesced`, `nodeProduce`) and overlapping producer/upload waits.
- `nestedOrParallelOverlapUs`: sum minus union. This measures duplicated coverage
  from **both nesting and parallel execution**, not just parallel upload time.
  Neither the sum nor this overlap is an additive partition of phase wall time.
  The only partition is observed union plus unattributed time, separately per side.
  Inclusive envelope coverage does not attribute all its internal work to child
  stages; remaining reconstruction, copying and cache work stays in the envelope.

| Stage | Meaning and byte unit |
| --- | --- |
| `inventory` | Discover base manifests and construct missing inventory; bytes zero. |
| `nodeHeaders` | One HTTP batch request from send to response headers, including route/relay startup; bytes zero. Receipt of an error status still counts as headers received. |
| `nodeFirstByte` | Headers-to-first-body-byte wait, once per batch; bytes one. Together with headers gives time to first byte. Nested in `nodeBody`. |
| `nodeBody` | One complete plaintext block read, including batch EOF check; plaintext bytes. Overlaps node production and S3 PUTs. |
| `convertedRead` | One converted local block lookup/seek/read; plaintext bytes. File opening and other unmeasured setup remain residual. |
| `baseRead` | Inclusive manager base-block read, admission through verified decode; plaintext bytes. |
| `recoveryAdmission` | Recovery payload capacity before fetch/decode; bytes zero. |
| `codecAdmission` | Shared codec permit wait; bytes zero. |
| `blockingQueue` | Spawn-to-entry wall delay in blocking pool, separately from worker execution; bytes zero. |
| `compress`, `decompress` | zstd call wall time; plaintext input/output bytes respectively. Raw fallback still pays compression. Raw reads have no decompression call. |
| `encrypt`, `decrypt` | Vault call wall time; codec payload input bytes / ciphertext input bytes. Envelope construction/copying outside the call is residual unless inside an inclusive envelope. |
| `journalHash` | BLAKE3 journal header/payload authentication computation, including cold reader recovery; bytes hashed (headers plus payloads). This is distinct from content identity and can count the same payload again. |
| `nodeIoAdmission`, `nodeOpenRetry` | Existing bounded runtime I/O permit wait and retry sleep while an earlier disk open/close drains; bytes zero. Permit counts, retry cadence and deadlines are unchanged. |
| `hash` | BLAKE3 content identity computation/verification; plaintext bytes. Multiple integrity checks legitimately count the same bytes multiple times. |
| `putSlot`, `getSlot` | Shared S3 admission wait; bytes zero. |
| `s3Client` | SDK client lookup/initialization; bytes zero. |
| `putChecksum` | MD5 transport checksum (distinct from content identity); encoded envelope bytes. |
| `putRequest` | SDK PUT send through response, including SDK retry/backoff; successful encoded envelope bytes once per logical PUT. Not socket wire bytes: retries, TLS and HTTP overhead are not counted. |
| `getHeaders`, `getBody` | Physical SDK GET headers/body; encoded bytes only on completed body. SDK retry/backoff is included in headers. |
| `getCoalesced` | Inclusive consumer GET/coalescing/cleanup wait; successful encoded bytes per consumer. Includes nested physical stages for the initiator. |
| `uploadBackpressure` | Producer blocked joining a PUT at the existing in-flight limit; bytes zero. |
| `uploadDrain` | Final join of all started PUTs, including on error, before lock release; bytes zero. |
| `nodeOpen` | Reader open/load, lease and journal/runtime coordination before headers; bytes zero. |
| `nodeMetadata`, `nodeJournalLookup` | Captured manifest lookup/SQLite/JSON and journal index selection, including their existing locks; bytes zero. These expose producer coordination separately from payload reads/hashing. |
| `nodeLocalRead` | Local snapshot block open/seek/read, or one journal record header/payload read pair, including buffer allocation; successful physical bytes read. Journal headers count here; hashes can count the same payload again. Cold journal recovery also reads headers within `nodeOpen`. |
| `nodeProduce` | Inclusive reconstruction/read per block, with journal/base/cache work and integrity checks; plaintext bytes. |
| `nodeBaseRequest`, `nodeRetryWait` | Each node-to-manager base request (including body/renewal) and retry sleep, separately; successful plaintext bytes / zero bytes. Failed responses count failed operations. |
| `nodeCacheWait` | Wait for the existing same-block source coalescing lock; bytes zero. |
| `nodeBaseRead` | Node remote-base fetch including manager round trip, retries and recovery waits; plaintext bytes. Manager S3/codec stages correlate when supported. |
| `nodeBackpressure` | Yielded block to next producer poll, excluding the last block; downstream transport backpressure plus scheduling. Bytes zero; not an isolated network latency. |

`referencedBlocks`, `reusedBlocks` and `plannedPlaintextBytes` are the manager's
reserved inventory, including reused objects. The planned bytes count only missing
objects. Compare successful `nodeBody` and `baseRead` bytes to the planned volume,
then `compress` input to successful `putRequest` envelope bytes. On failure these
need not balance: bytes may have been read/encoded without a completed PUT.
Node summaries leave these inventory fields zero; use `nodeProduce` counts/bytes.

## Example and diagnosis

Abbreviated illustrative fields (all other stages remain in actual JSON):

```json
{ "operation": "disk_publication_transfer", "publication_attempt": "<uuid>", "run_id": "<uuid>", "snapshot_id": "<uuid>", "side": "manager", "outcome": "success", "metrics": { "wallUs": 1200000, "observedUnionUs": 1100000, "unattributedUs": 100000, "operationWallUsSum": 2600000, "nestedOrParallelOverlapUs": 1500000, "referencedBlocks": 3, "reusedBlocks": 1, "plannedPlaintextBytes": 8388608, "stages": { "nodeBody": { "completed": 2, "bytes": 8388608, "wallUsSum": 600000 }, "compress": { "completed": 2, "bytes": 8388608, "wallUsSum": 180000 }, "putRequest": { "completed": 2, "bytes": 2000000, "wallUsSum": 1300000 }, "uploadDrain": { "completed": 1, "bytes": 0, "wallUsSum": 400000 } } } }
```

This took 1.2 s, not 2.6 s. Two PUTs consumed 1.3 s of cumulative SDK time; some
of that overlapped reads/encoding and the final drain. It does not establish CPU
usage or isolate WAN transfer from SDK retries. Check matching node summaries:
large `nodeProduce`/`nodeBaseRead` indicates producer/recovery cost; fast production
with large manager body waits and node backpressure points toward transport or
consumer pacing. Compare codec execution with its admission/pool queue, PUT slots
with SDK time, producer backpressure with final drain, and residual coordination.
These observations narrow a bottleneck; they do not reconstruct a critical path,
measure one-way link latency, or exclude concurrent foreground cache/coalescing
work. Summaries are bounded diagnostics, not a distributed profiler.

`success` means transfer finished on the manager, or all requested plaintext was
produced on the node; node success does not confirm network delivery or manifest
commit. `error`, `cancelled`, `error_or_cancelled`, `progress`, `expired_partial`
`evicted_partial` and `snapshot_discarded_partial` preserve partial evidence.
No summary survives process death.
The owned capture task still outlives an HTTP caller's cancellation, and started
PUTs still drain on ordinary errors. No concurrency, cadence, timeout, format,
durability or integrity policy changes.

This instrumentation becomes available in production through the normal delivery
process. It cannot retrospectively decompose publications completed before delivery.

## Local overhead qualification

Run the opt-in fixture alone, with the isolated backend launcher:

```sh
pnpm test:backend -p cairn-installation --lib \
  publication_transfer_overhead_measurement -- --ignored --nocapture --test-threads=1
```

The fixture writes sixteen distinct 4 MiB journal blocks: eight repeated-byte
blocks and eight deterministic PRNG blocks (seed 42), each with its block index
in the first eight bytes. It measures reopening the journal, reading through a
retained reader, and the shipped manager transfer over HTTP to the real node
snapshot producer and a loopback S3 PUT receiver. The transfer enables manager
and node diagnostics, asserts that the correlation JSON reaches the producer,
and formats summaries with the production default filter into a sink. It includes
summary construction/formatting, but excludes stderr and log-collector I/O.
Setup, deterministic input generation and final volume assertions are outside timed
regions; transfer integrity and diagnostic-correlation checks remain in the path.
Two warmups precede six samples; rows report process CPU separately from wall
time and conserve plaintext/encoded PUT volumes. The process CPU includes the
manager, producer and simulated S3 receiver in this one-process fixture.

Compare with an unchanged production-source reference using the same test-only
fixture. On a reference without this instrumentation, omit the
`Publication.publication_attempt` initializer and its local UUID, and replace the
node callback's diagnostic assertion/parsing and `served_publication_observed`
call with `served_publication(&captured, hashes)`. Expose the reference's existing
`#[cfg(test)]` module `object_storage::tests` and its unchanged `fixture_storage`
helper as `pub(crate)`. These adaptations and the measurement module declaration
are test-only; keep the reference's production functions unchanged.
Keep both compiled binaries and alternate their
execution order to limit drift; record the reference SHA, profile and host load
with results in the PR.

"Cold" here means a reopened reader/journal, while "warm" retains its volume.
Neither drops the OS page cache or simulates a provider cold read. Record whether
the temporary directory uses tmpfs or a physical disk. This loopback workload
does not qualify WAN, TLS, remote base recovery, production CPU contention or
logging transport. Do not turn local timing differences into CI thresholds or a
claim about production transfer throughput.

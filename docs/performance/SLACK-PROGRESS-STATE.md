# Delivery-owned progress state

Local native observation on 2026-10-23. Run alone through the isolated backend
launcher, with the repository's current manager and worker binaries:

```sh
CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 \
  mise exec pnpm@12.3.4 -- pnpm test:backend -p cairn-installation --test slack -- \
  state::measure_progress_state_storage --ignored --exact --nocapture
```

The [raw samples](slack-progress-state-storage.json) compare the same synthetic,
confirmed progress value stored as ordinary instance state and as delivery-owned
state. It contains a delivery/conversation reference, source/person/message
binding, wall deadline and a short native step label. The fixture seeds its
pending delivery; it does not measure a complete model or Slack lifecycle.
Both instances have 999 unrelated rows, each with a 1,800-byte string, before the
observed key is inserted. Native writes include actual capability authorization
and store actors; retirement uses the actual native delivery-removal transaction.

Ownership adds the existing host-derived `deliveryId` and `chatId` fields (+100
serialized JSON bytes). No table, index, envelope or native capability is added.
Reference retirement retains the value; candidate retirement removes it. Later
reference writes update that retained key, whereas candidate writes insert again
and perform the existing 1,000-key capacity scan. This lifetime difference is part
of the measured workload, and explains the candidate's higher repeated-write cost.

| Metric | Ordinary reference | Delivery-owned progress |
| --- | ---: | ---: |
| Observed row JSON bytes | 621 | 721 |
| First reopened-reader read wall / CPU, ms | 0.491 / 0.691 | 0.441 / 0.505 |
| Five warm reads median wall / CPU, ms | 0.027 / 0.028 | 0.016 / 0.016 |
| Six writes median wall / CPU, ms | 2.030 / 0.573 | 9.283 / 7.691 |
| Six retirements median wall / CPU, ms | 4.029 / 2.312 | 4.099 / 2.394 |
| Rows remaining after retirement | 1000 | 999 |

Six reads share one reopened reader: the first is connection-cold, five are warm.
OS caches are retained; this is not cold physical I/O. Reads bypass capability
authorization. Six write/retirement pairs include native checks and actor
scheduling, but exclude prepared schema/auth, unrelated rows and re-insertion of
the pending delivery. CPU is process CPU. Every sample is retained; six values do
not establish a tail percentile. Initial posting intent and uncertain-edit intent
have different small value shapes; their entire lifecycle is not timed here.

Host: native Linux 7.2.9-arch1-1 x86-64, Intel Core i9-14900K, 32 logical CPUs
reported by `lscpu`, 12 available to the Rust process. The sampled cgroup
`cpu.max`/`memory.max` paths are unavailable. Temporary stores use `/tmp`, whose
filesystem reports `ext2/ext3`; filesystem and OS caches are not flushed. The
reference runs first, then the candidate. No other test/build was launched by
this agent during measurement; unrelated host processes and scheduling remain
uncontrolled.

These are row JSON bytes, excluding database/WAL/index pages, and local store
latencies. They do not establish live Slack/model latency, physical durability or
production network/object-storage performance. Actual-worker progress/cancel,
cooldown, uncertain-write, restart and revocation journeys are separate correctness
evidence. Whole-PR compatibility and final qualification remain open.

# Verified workspace app identity

Local native observation on 2026-10-23, run alone through the current isolated
backend launcher and its rebuilt manager/worker:

```sh
CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 \
  mise exec pnpm@12.3.4 -- pnpm test:backend -p cairn-installation --test slack -- \
  state::measure_workspace_app_binding_storage --ignored --exact --nocapture
```

The [raw samples](slack-workspace-app-binding-storage.json) compare the same
synthetic verified workspace resource without its new `settings.appId` field
(reference) and with the provider-verified value (candidate). Each has 63
unrelated resource rows containing 1,800-byte labels. The field adds 16 JSON
bytes for this synthetic app ID; real IDs can have different lengths. No table,
index or encryption envelope is added.

| Metric | Previous reference | Verified app binding |
| --- | ---: | ---: |
| Resource row JSON bytes | 372 | 388 |
| First reopened-reader read wall / CPU, ms | 0.509 / 0.647 | 0.567 / 0.646 |
| Five warm reads median wall / CPU, ms | 0.025 / 0.026 | 0.025 / 0.027 |
| Six writes median wall / CPU, ms | 4.109 / 0.342 | 3.793 / 0.266 |

Six reads share one reopened reader: the first is connection-cold, five are warm.
OS caches are retained; this is not cold physical I/O. Reads exclude the store
actor queue; writes include it. CPU is process CPU. Setup, schema creation,
provider verification and initial row writes are excluded. The reference runs
first. All six raw samples are retained; they do not establish tail percentiles
or a meaningful latency improvement.

Host: native Linux 7.2.9-arch1-1 x86-64, Intel Core i9-14900K, 32 logical CPUs
reported by the host, 12 available to the Rust process. Temporary stores use
`/tmp`, whose filesystem reports `ext2/ext3`. Caches were not flushed. This
agent ran no other build, test or emulator during measurement. Unrelated host
load remained uncontrolled: sampled I/O pressure averaged 32.13% some / 24.38%
full over 10 seconds; memory pressure averaged 0.00%. Cgroup CPU/memory limit
paths were unavailable.

These are row JSON bytes, excluding vault, database, WAL and index pages, and
local store timings. The operational guard also obtains the resource through a
native plugin RPC before provider I/O; that RPC, approval and authorization
checks, actor scheduling outside these samples, and provider/network latency
are not measured here. The admission tests separately prove refusal before
token-bearing I/O after root replacement, both before and after activation, and
owner revalidation of a legacy resource. Production recovery and whole-contract
qualification remain open.

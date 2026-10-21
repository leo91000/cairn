# Resource references before an agent merge

The merge guard refuses to delete an agent while a resource route or granular
resource-access rule still names it. It asks SQLite for existence instead of
materializing every configuration and people directory. Both alternatives below
compute that same predicate; the old implementation had no such resource guard.
This is a comparison of implementations of the added check, not an A/B of whole
agent merges or production network latency.

## Reproduce

```sh
CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=1 \
CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 \
mise exec -- pnpm test:backend -p cairn-installation --lib \
  agent_merge::measurement::measure_resource_merge_precondition -- \
  --ignored --exact --nocapture --test-threads=1
```

The opt-in test calls the production SQL predicate. The reference uses `Db::list`
for both record kinds, scans `agentId`, and serializes the materialized records.
There are 2,000 synthetic inactive reference rows, alternating routes and grants,
with 32 channel IDs and 32 people per payload. They exercise scan size, not full
provider-authority validation or a representative production distribution. One
case names the requested agent in one row; the other has no reference.

Each implementation opens a fresh Store; sample 0 has cold reader and statement
caches, followed by five warm samples. OS filesystem caches remain warm. The
present reference may stop earlier in SQL index order than a complete scan.
These local samples are not isolated hardware or a statistical latency guarantee.
Other qualification jobs had finished before the measurement; the normal agent
processes remained. Compilation time is excluded.

## Observed locally on 2026-10-21

Host: Linux x86_64, kernel `6.12.109`, 7 available CPU threads, 19,462,272 KiB reported memory. Cgroup quota files were unavailable (`null` in the raw data); no quota is inferred. Storage is the VM's local overlay filesystem.

The seeded input JSON totals **4,744,000 bytes**, unchanged between alternatives. This is logical JSON, not physical SQLite/compressed disk usage. `materializedJsonBytes` below measures the complete reference array or the Boolean result encoding; it is not an HTTP response size or a claim that the results contain equivalent datasets. The Boolean check returns the same yes/no answer.

| Agent reference | Implementation | Materialized JSON bytes | First read ms | Warm median ms | Warm median process CPU ms |
| --- | --- | ---: | ---: | ---: | ---: |
| present | complete-record-scan | 4,746,001 | 1073.025 | 980.459 | 980.510 |
| present | existence-projection | 4 | 0.844 | 0.735 | 0.703 |
| absent | complete-record-scan | 4,746,001 | 971.746 | 1009.368 | 1009.449 |
| absent | existence-projection | 5 | 33.812 | 39.978 | 40.018 |

The existing same-value write path is unchanged: first write **0.896 ms**, five warm writes median **0.523 ms**, warm process CPU median **0.543 ms**. These write samples do not measure a complete merge transaction.

[Raw samples and exact source hashes](AGENT-MERGE-RESOURCE-READS.local.json) retain every read/write value and host observation. The ordinary owner-API regression separately verifies refusal without mutations for both record kinds, including inactive references, and success after explicit owner resolution. No source record, route or access right is copied or widened by the check.

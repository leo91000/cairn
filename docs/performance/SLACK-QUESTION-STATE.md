# Delivery-owned question state

Local native observation on 2026-10-22. Reproduce with the isolated backend
launcher and no other local test/build running:

```sh
CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 \
  mise exec -- pnpm test:backend -p cairn-installation --test slack \
  state::measure_question_state_storage -- --ignored --exact --nocapture
```

The [raw samples](slack-question-state-storage.json) compare the same small,
synthetic nonsecret question value in an instance containing 999 unrelated rows
(each with a 1,800-byte string). The reference uses ordinary persistent instance
state; the candidate ties that value to a pending native delivery. Both use the
actual Rust storage capability, authorization and store actors for writes, and
`work::finish` for retirement. No completed model turn or Slack response is seeded.
The synthetic pending delivery and question are explicitly fixture data.

The candidate adds two host-derived UUID fields, `deliveryId` and `chatId`, to
the existing record (+100 JSON bytes). No table, index or storage envelope is
added. Retirement deletes owned records in the same transaction as the delivery.
Reference retirement leaves the question value behind; candidate retirement
returns the instance to 999 keys. Thus later candidate writes insert a new key
and check the 1,000-key limit, while reference writes update the retained key.
That scan and its JSON decoding explain the candidate's higher repeated-write
cost; this comparison deliberately includes this lifetime difference.

| Metric | Reference | Delivery-owned candidate |
| --- | ---: | ---: |
| Observed row JSON bytes | 296 | 396 |
| First reopened-reader read wall / CPU, ms | 1.019 / 1.227 | 0.852 / 1.008 |
| Five warm reads median wall / CPU, ms | 0.069 / 0.073 | 0.065 / 0.069 |
| Six writes median wall / CPU, ms | 1.347 / 1.367 | 32.185 / 32.258 |
| Six retirements median wall / CPU, ms | 9.699 / 9.775 | 7.695 / 7.748 |
| Rows remaining after retirement | 1000 | 999 |

Six reads share one reopened reader: the first is connection-cold, five are
warm. They use the actual store reader but bypass capability authorization.
Six write/retirement pairs include native checks and actor scheduling; fixture
setup, re-insertion of the pending delivery and unrelated rows are excluded.
The raw samples preserve every value rather than inferring a tail percentile
from six observations. CPU is process CPU, not just database-thread time.

Host: Linux 6.12.109 x86-64, Intel Xeon @2.80 GHz, seven available logical CPUs.
The VM exposes no readable `cpu.max`/`memory.max` at the sampled cgroup paths.
Temporary databases use `/tmp` tmpfs; OS caches are retained. The reference ran first,
then the candidate, without another local compiler, test or emulator running;
the disposable PostgreSQL fixture was idle. Shared-host scheduling is uncontrolled.

The 296/396-byte rows are **not** the 16,000-byte supported Slack field-schema maximum
or the generic 64-KiB value maximum. These are serialized row bytes, excluding
SQLite pages/WAL/indexes. This observation does not establish cold physical I/O,
a full question lifecycle, live Slack, physical VM or production network/S3
latency. Actual-worker answer/cancel, cooldown, revocation and purge scenarios
are separate correctness evidence. Whole-PR compatibility and final qualification
remain required.

# Owner package and instance read contracts

Measured locally on 2026-10-21 against synthetic packages and saved instances.
The reference is the complete `/api/plugins` response in the same binary. The
candidates are the **first** compact package page, **first** selected-package
instance page, **first** compact saved-access page, and one exact instance read.
These return different information and datasets, not equivalent complete results
or an A/B comparison of releases.

Reproduce alone from the repository root:

```sh
PATH="$PATH:/usr/sbin" CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 mise exec -- pnpm test:backend -p cairn-installation --test plugin_catalogue measure_catalogue -- --ignored --nocapture --test-threads=1
```

The [raw observation](plugin-catalogue-reads.json) includes every sample, source
and binary hashes, build profile and host conditions. The fixture copies 40
packages containing 79 valid versions, each with six 10,000-character TOML
defaults. Each manifest stays below 65,536 bytes; escaping its literal quotes
roughly doubles its JSON size. Two tiny Luau files accompany each version. Forty
paused instances of one package store matching six-field settings and no
credentials or grants. Fixtures are created before timing. Instance and access
pages each contain 32 instances; the exact read contains one. Generated UUIDs can
slightly change first-page membership and encoded size between repetitions.

| Local measurement | Complete response | First package page | First instance page | First access page | Exact instance |
| --- | ---: | ---: | ---: | ---: | ---: |
| Encoded response bytes | 19,226,147 | 20,618 | 3,857,677 | 5,592 | 120,548 |
| First API read wall time, ms | 6067.727 | 2365.128 | 2290.503 | 38.242 | 1107.746 |
| First API read process CPU, ms | 6050.178 | 2367.750 | 2288.721 | 37.881 | 1107.010 |
| Five warm reads, median wall time, ms | 4188.725 | 1942.437 | 2067.360 | 22.507 | 1025.168 |
| Five warm reads, median process CPU, ms | 4192.522 | 1945.113 | 2067.440 | 22.579 | 1025.327 |

Six observations of the existing registry approval write path have median wall
time **74.667 ms** and process CPU **73.139 ms**. All reads share that
unchanged write path and stored record format. No summary, index, token or
manifest is newly persisted by these APIs. Earlier four-purpose observations
remain available in [Git](https://github.com/leo91000/cairn/blob/fea34810952ee6d6d0fc5c58b2ee4617cb50e5ca/docs/performance/plugin-catalogue-reads.json);
no cross-revision performance claim is made.

The first request is cold only with respect to its API. OS page caches retain
fixture creation and earlier reads. The complete response is measured first,
then cards, instance page, access page and exact instance. Warm rounds reverse
that order on alternate samples. Timings include routing, file validation and
hashing, database actor work, serialization and response collection. They exclude
client parsing and network/Beacon latency. Process CPU includes all threads of
the test process. Approval timings include validation and its transaction, rather
than an isolated SQL upsert.

Conditions: Linux-6.12.109-x86_64-with-glibc2.36, 7 virtual CPUs,
MemTotal:       19462272 kB; load average at launch
`1.45 4.03 4.76 1/181 15086`. Other test suites, compilers, Node builds,
PostgreSQL and the managed emulator had stopped before this isolated observation.
The coding agent and shared VM scheduler remained active. These timings establish
no production latency guarantee.

All paged candidates stay within half the 8,000,000-byte relay limit; the complete
reference exceeds it. Functional cases separately follow **all** package, version,
instance and saved-access cursors and enforce count/byte bounds. A valid
eight-field instance fixture forces a page before the 32-item count limit. Saved
access tests also cover resource memberships, deduplication, paused instances,
missing source and owner-only access. Exact reads retain full current root IDs;
changed module bytes still reject stale approval digests.

Remaining CPU work is visible: cards validate every installed version of each
selected package. Full instance reads inspect that package for worker/readiness
semantics; the dense package here has 40 versions. The access projection avoids
materializing settings in Rust or loading credential/history records, but SQLite
still reads the underlying instance JSON and correlates resource grants. CPU is
not bounded solely by response bytes or item count. Web/Android cards, detail and
review screens now use these separate contracts; agent/grant selectors still
require their own migration.

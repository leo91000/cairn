# Owner package read contracts

Measured locally on 2026-10-21 against synthetic packages. The reference is the
complete `/api/plugins` response in the same binary, retaining its existing data
contract. The candidate is the **first** `/api/plugins/packages` page: at most
32 compact cards. This is a comparison of response purposes, not an A/B benchmark
of two releases or equivalent complete datasets.

Reproduce alone from the repository root:

```sh
PATH="$PATH:/usr/sbin" CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 mise exec -- pnpm test:backend -p cairn-installation --test plugin_catalogue measure_catalogue -- --ignored --nocapture --test-threads=1
```

The [raw observation](plugin-catalogue-reads.json) includes every sample and host
conditions. The fixture copies 40 packages containing 79 valid versions, each
with six 10,000-character TOML defaults. The input manifest stays below 65,536
bytes; escaping its literal quotes roughly doubles its JSON representation.
Source is two tiny Luau files per version. Fixtures are outside measured reads.

| Local measurement | Complete response | First compact page |
| --- | ---: | ---: |
| Encoded response bytes | 14,404,158 | 17,174 |
| First API read wall time, ms | 3762.934 | 2239.057 |
| First API read process CPU, ms | 3765.388 | 2239.502 |
| Five warm reads, median wall time, ms | 3927.364 | 2066.962 |
| Five warm reads, median process CPU, ms | 3930.136 | 2066.143 |

Six observations of the existing registry approval write path have median wall
time **72.895 ms** and process CPU
**71.197 ms**. Both read contracts
use that same unchanged approval write and persisted record format. No catalogue
summary, index, token or manifest is newly persisted by these reads.

The first API request is cold only with respect to that endpoint; OS page caches
are retained, and the complete response is measured first. Later samples alternate
order. Reads include native routing, file validation/hashing, database actor work,
JSON serialization and collecting the response body; they exclude client parsing
and network/Beacon latency. Process CPU includes all threads of the test process.
Approval measurements include its existing validation and database transaction,
rather than just an isolated SQL upsert.

Conditions: Linux 6.12.109 x86_64/glibc 2.36, seven virtual CPUs, about 18.6 GiB RAM;
load average at launch 1.95/2.76/2.18. No other test suite, compiler, PostgreSQL or
emulator ran during the observations. The coding agent and shared VM scheduler
were active. These timings establish no production latency guarantee.

The compact first page is below the 8,000,000-byte relay limit; the complete
reference exceeds it. Functional tests additionally follow **all** package and
version cursors, enforce the 32-item and half-relay encoded-byte budgets on each
page, and retain exact selected-version review. They include an extra invalid
version, so their byte counts differ slightly from this measurement. The compact
read still validates all installed versions of each selected package to derive
approval status: its work and CPU are not bounded solely by 32 cards. Client
migration and purpose-specific instance/grant reads remain required before the
owner catalogue no longer uses the complete response.

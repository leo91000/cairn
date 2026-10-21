# Complete agent selector reads

The web and Android agent selectors use compact saved metadata and load a package's
full resource/profile/operation schema only when the owner chooses its instance.
This changes reads, with no new persisted record format. Saved-rule counts are
presentation, never execution authority.

Reproduce with the credential-isolating backend launcher, on an otherwise idle
Linux host (pinned mise environment, debug symbols and incremental cache disabled):

```sh
CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_BUILD_JOBS=1 \
  mise exec -- pnpm test:backend -p cairn-installation --test plugin_catalogue \
  measure_complete_agent_selector_reads -- --ignored --nocapture
```

The workload has 40 copied packages, 79 copied versions, 40 saved instances with
six 10,000-character defaults each, 40 saved root agent IDs per instance and 2,000
resource rules. It compares the complete aggregate catalogue with **every** package,
instance-access and per-agent count page, including JSON decoding in both paths.
The latter intentionally does not load any selected schema yet. Each workload
alternates order across six samples; the first is API-cold and the remaining five
are warm. OS caches remain populated. SQLite, package filesystem validation and
the local HTTP router run in the same process; these are not production network
latencies. The candidate makes six requests instead of one. The shared VM's CPU
and unknown cgroup-limit observations are in the [raw data](measurements/plugin-agent-selectors/local.json).
No emulator, Postgres or other local build/test ran during the samples.

| Complete read workload | Encoded response bytes | API-cold | Warm median | Warm process CPU median |
| --- | ---: | ---: | ---: | ---: |
| Aggregate catalogue | 19,288,428 | 5,671 ms | 5,055 ms | 5,058 ms |
| Selector directory, all pages | 107,088 | 2,450 ms | 2,649 ms | 2,653 ms |

The unchanged package-approval write path was sampled six times after the reads:
median 66.6 ms wall time and 66.5 ms process CPU. It still updates the existing
package approval/revision record and audit entry; there is no alternative write
format in this comparison. Encoded response bytes above count complete HTTP
response bodies, not on-disk compression or network framing. This observation
makes no timing assertion and does not establish production performance.

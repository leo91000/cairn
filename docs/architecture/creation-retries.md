# Retrying resource creation

Editors keep a saved resource ID as soon as the create response arrives, before
saving a GitHub token or refreshing the workspace. A failed secondary operation
leaves the editor open with a message that distinguishes the committed save from
the failed token update or reload. The next save updates that ID.

A lost POST response is a separate case. Web and Android editor sessions generate
a random UUID and send it as optional `creationId` on POST `/agents`, `/projects`
and `/tasks`. The server validates it as UUIDv4 and uses it as the normal record
ID. In the write transaction, an existing record returns unchanged; a collision
with another resource kind is rejected. Concurrent requests and process restarts
therefore keep one resource and one mission schedule. Clients without this field
retain the existing server-generated identity contract. PUT uses the URL ID and
ignores `creationId`.

The editor retains the original request after an uncertain response. A retry first
recovers that creation, then applies subsequent form edits with PUT. It stores the
acquired ID before that PUT, so even a failed update keeps future retries on the
same resource. A definitive validation rejection allows correction of the initial
request. GitHub imports retain their existing repository-based idempotency; once
an import response is acquired, subsequent saves update the returned project.

This protects retries within an editor session. Deliberately starting a new editor
session creates a new identity. Deleting the resource ends the lifetime of its
identity; there is no permanent request tombstone or deduplication by mission text.

## Compatibility

No installation persistent schema, record shape, storage codec, or database version changes.
`creationId` is removed by input normalization and is not stored as an extra field.
The resulting UUIDs and records are readable by the previous release, and the new
release continues to read existing records. The compatibility epoch remains
unchanged under `docs/PRODUCTION-CAIRN.md`.

## Local storage reference

The ignored `measure_creation_retries` test compares the legacy API without a
creation identity against the stable identity path: 100 synthetic scheduled
missions, each submitted twice, 20 reads through freshly opened SQLite pools,
and 200 warm reads. Both cases use the candidate binary; the reference omits
`creationId` and exercises the existing server-generated identity path. It reports
encoded JSON record bytes, first-save and replay
latencies, read latencies and process CPU. Connection-cold reads exclude pool
opening time and retain the operating system's file cache. These are local SQLite
measurements, not production or network latency. No disk-object codec changes.

Run with pinned tools and the credential-isolating backend launcher:

```sh
CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 CARGO_INCREMENTAL=0 \
  pnpm test:backend -p cairn-installation --test creation_retries \
  measure_creation_retries -- --ignored --nocapture --test-threads=1
```

The local run on 2026-10-18 used Rust 1.97.1, the unoptimized test profile with
debug information and incremental compilation disabled, on a shared Cairn VM:
Linux 6.12.109 x86-64, glibc 2.36, seven vCPUs (Intel Xeon at 2.80 GHz),
19,968 MiB allocated memory, and an overlay filesystem. Android compilation was
running concurrently. The following wall times therefore include local scheduling
contention; they are a reference for this workload, not a throughput prediction.

| Identity | Mission records | Encoded JSON bytes | Process CPU, µs |
| --- | ---: | ---: | ---: |
| Legacy | 200 | 72,000 | 1,954,162 |
| Stable | 100 | 36,000 | 1,275,011 |

| Identity | First write, µs p50 / p95 | Retry, µs p50 / p95 | Connection-cold read, µs p50 / p95 | Warm read, µs p50 / p95 |
| --- | ---: | ---: | ---: | ---: |
| Legacy | 4,937 / 19,149 | 4,679 / 14,531 | 4,171 / 17,308 | 416 / 2,965 |
| Stable | 3,965 / 25,492 | 1,652 / 14,719 | 1,364 / 3,172 | 395 / 3,421 |

Bytes are the sum of UTF-8 JSON mission records, excluding SQLite pages, indexes,
audit rows and WAL overhead. CPU is process user plus system time for the complete
measured workload, including opening the read pools and the final record listing;
it excludes initial service setup and the separate Android compiler. Pool opening
is outside each connection-cold read's wall-time interval. The filesystem cache is
not dropped. The deterministic result is one stored mission per stable identity;
the noisy latency percentiles do not establish a performance guarantee.

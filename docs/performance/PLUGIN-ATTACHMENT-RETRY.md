# Attachment staging across authority retries

An accepted plugin event can lose native authority temporarily after an upload
commits and before conversation submission. Allocating a new attachment ID on
every retry duplicated the committed files and could exhaust the chat's 200 MiB
quota before the accepted message was submitted.

Submission now derives an opaque UUID from the instance, the host's durable
admission ID and the file position. The existing attachment store reuses an ID
only when the sanitized name and digest match, before checking remaining quota.
Changed source bytes remain a conflict; a distinct admission gets a distinct
file. No new stored field, key family or table is added. This remains inside the
unqualified epoch-14 / SQLite-13 boundary, without an automatic migration.

The actual-worker regression first reproduced duplication after an authority
outage (0.45s), then loss at the exact quota (3.77s). The corrected three-case
suite passes, including a worker restart, the exact 200 MiB quota, refusing changed
bytes and two distinct admissions in the same conversation. Owner HTTP downloads
verify both files' contents. Its latest run took 45.060s including compilation;
source hashes remained unchanged. These are signed-ingress/native-submission
fixtures, not final provider answers, Slack file support or a live workspace test.

## Reproduce the storage observation

```sh
pnpm test:backend -p cairn-installation --test plugin_routes \
  authority_outages::measure_authority_retry_attachment_storage \
  -- --ignored --exact --nocapture --test-threads=1
```

Use pinned pnpm 12.3.4 and Rust 1.97.1. The native observation passed in 1.259s
including the launcher, with unchanged source. It calls the actual
`Service::store_attachment` six times with identical synthetic 1 KiB bytes. The
reference allocates a fresh UUID each time; the second case reuses one UUID.
UUID generation and the plugin's deterministic ID derivation are excluded.
[Raw samples](PLUGIN-ATTACHMENT-RETRY.local.json) retain all timings and host data.

| Six calls | Metadata records | JSON bytes | Key bytes | Logical file bytes |
| --- | ---: | ---: | ---: | ---: |
| Fresh IDs | 6 | 1,698 | 534 | 6,144 |
| Reused ID | 1 | 283 | 89 | 1,024 |

Each case opens a new Store connection after the writes. One read actor performs
the connection-cold metadata read and five warm reads; the OS cache is retained.
The following values are milliseconds; warm reads and subsequent writes are
medians of five samples. CPU uses the process clock.

| Case | Cold read wall / CPU | Warm read wall / CPU | First write wall / CPU | Subsequent writes wall / CPU |
| --- | ---: | ---: | ---: | ---: |
| Fresh IDs | 0.529550 / 0.620116 | 0.010518 / 0.010868 | 11.538041 / 1.572219 | 11.514072 / 1.335407 |
| Reused ID | 0.551626 / 0.876614 | 0.020868 / 0.022034 | 10.129644 / 1.123740 | 0.130938 / 0.137454 |

Host: Linux 7.2.9-arch1-1, glibc 2.44, Intel Core i9-14900K, 12 available threads.
Load was 17.37/12.12/10.67; 10s I/O pressure was some 33.79% / full 25.32%.
Cgroup CPU/memory limit files were unavailable. No task-owned emulator, browser
or other build/test was active; unrelated host load was uncontrolled.

Logical bytes exclude filesystem allocation, SQLite pages, WAL and indexes.
Reads exclude actor scheduling; writes include it. Fixed case order and short
samples provide no production latency bound or causal speedup measurement.
These observations cover native metadata/file storage primitives, excluding
plugin dispatch, file downloads, authority calls, ID derivation, recovery and
whole-PR forward migration. Older unqualified staged attachments and production
recovery still require the broader compatibility qualification.

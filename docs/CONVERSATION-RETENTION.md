# Automatic conversation deletion

The installation owner can set **Days of inactivity** under web **Settings →
Installation → Automatic conversation deletion**, or **Jours d’inactivité** in
Android **Paramètres → Suppression automatique des conversations**. The setting
applies to every conversation in the installation and is saved server-side.
The default is **7 days**. Choose a whole number from 0 to 36500; **0 disables
future automatic deletion**. Both apps ask for confirmation before saving and
explain that existing conversations are included.

Inactivity starts at the last conversation activity (`lastActivityAt`, falling
back to `updatedAt`, then `createdAt` for older records), or the last run's
`finishedAt`, whichever is later. Merely reading a conversation does not extend
its life. A queued/running agent, pending message, open question (including a
nonblocking question), scheduled wakeup, run preparation or worker finalization
protects the conversation. Pausing a conversation does not exempt it once its
pending work is resolved.

The worker checks every 15 seconds and removes at most one eligible conversation
per pass. It rechecks the current policy, activity and pending work atomically
before claiming deletion; a concurrent message or disabling the policy wins if
it commits first. Existing process, storage, preparation and worker locks prevent
cleanup from racing run setup or finalization.

Deletion uses the existing permanent deletion path: immediately hide the
conversation, revoke public artifact links and grants, then remove its history,
attachments, artifacts, workspaces and published disks. Unavailable nodes/storage
leave durable cleanup state and retry after one minute. Logical deletion is
irreversible; physical erasure can finish later. Disabling or increasing the
delay cannot recover a conversation already claimed for deletion. Existing trash
keeps its promised recovery deadline.

`GET /api/settings/conversations` returns `{ "inactivityDays": 7 }` by default.
`PUT` at the same owner-only endpoint accepts the same shape and records an audit
entry. The additive `conversation-expiration` KV record deliberately differs
from the retired archive policy (`conversation-retention`), which stays ignored.
No database schema, disk encoding or object format changes are required; the
existing installation data compatibility version is unchanged. Rolling the code
back stops future expiration but cannot restore deleted conversations.

## Validation

Backend lifecycle tests cover default/configured/disabled retention, invalid
values, owner permissions, policy persistence, the legacy activity fallback,
recent run completion, pending work, finalization, concurrency and physical
cleanup including public links. The settings browser journey covers confirmation,
cancellation, persistence and disabling; shared Android Compose cases cover the
same actions, input validation and failed-save retry on JVM and device.

## Local storage measurements

Run the ignored benchmark against the actual `Service` / `Store` interfaces:

```sh
pnpm test:backend -p cairn-installation --test conversation_lifecycle \
  measure_retention_storage -- --ignored --nocapture --test-threads=1
```

Reference: retention disabled (`0`), followed by enabled (`7`), using the same
100 synthetic conversations, all 8 days old, with no runs, attachments or remote
storage. Each setting measures 50 warm reads, 50 first reads through newly opened
SQLite connections, 50 writes of the same unchanged chat document, and 20 cleanup
passes. CPU is process user + system time across those operations, including
connection creation. Setup and compilation are excluded. Cold connection reads
exclude opening the store itself; they **do not** mean a cold host page cache.

Local sample, 2026-10-17: Linux 6.12.109 x86-64 KVM VM, 12 Intel Xeon vCPUs,
30 GiB RAM, local container overlay filesystem, Rust 1.97.1 unoptimized test
profile. The targeted Android JVM rerun ended about five seconds before the
sample; no exclusive CPU reservation was held. These are descriptive samples,
not a latency guarantee. The Android emulator was stopped. No production
conversation data or network I/O is involved.

| Measurement | Disabled reference | 7-day retention |
| --- | ---: | ---: |
| Encoded chat JSON bytes | 269 | 269 |
| Warm read median | 0.061 ms | 0.062 ms |
| Cold connection read median | 0.280 ms | 0.279 ms |
| Write median | 0.170 ms | 0.166 ms |
| Cleanup pass median | 0.678 ms | 1.163 ms |
| Process CPU for the workload | 0.074 s | 0.088 s |
| Conversations remaining after 20 passes | 100 | 80 |

Read/write representation stays unchanged. The enabled workload additionally
claims and removes one conversation per pass. Node disk deletion and object-store
publication cleanup still use the existing retry path; this local measurement
must not be interpreted as production network or remote erasure latency.

The full local Android JVM run completed 424 tests with two initial failures
(`CairnAccountJourneyTest.installationSelectionAndMemberRole` and
`ConnectionLostTest.theNoticeClearsItselfAndReloadsTheWorkspaceOnceTheInstallationAnswers`).
A targeted rerun of those two and both new retention tests passed 4/4. Both new
retention tests also passed on the API 36 AOSP emulator with KVM. Android lint and
Spotless passed. The Chromium settings journey passed against real local Beacon
and installation processes. Backend checks passed 19 lifecycle integration tests,
388 unit tests, the storage lock test and workspace Clippy; `pnpm check` passed
536 tests, lint, typechecking and production build.

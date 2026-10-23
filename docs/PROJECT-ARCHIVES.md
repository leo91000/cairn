# Prepared project archives

Native conversations can initialize their private ext4 filesystem from a cached
ZIP rather than transferring JSON/base64 file frames and syncing every file.
The manager refreshes remote project seeds every minute. Nodes learn about
complete, authorized versions through their existing heartbeat and preload them
over their authenticated outbound connection. This is background distribution,
not a new inbound port or an agent-visible filesystem share.

Each version identifies the registered project configuration, source commit,
BLAKE3 archive checksum, encoded bytes and logical bytes. New foreground checkouts
still resolve the remote branch with the current granted Git credentials; a
network/authentication failure never falls back to an older commit. Local project
snapshots keep their existing behavior. Normal, nonisolated Git clones keep their
existing contract too.

Only repository seeds are shared. Credentials, generated homes, installed skills
and files edited in a conversation are excluded. The seed retains Git history,
file modes and symlinks. Extraction rejects traversal, duplicate paths, symlink
ancestors, unsupported entries and entries exceeding the remaining disk budget.
Downloads verify both encoded length and BLAKE3 before atomic publication. A
foreground cache hit on either the master or node is rehashed before it becomes
a conversation seed, including ZIP names and Unix permissions outside payload CRCs.

The master and each node keep at most 2 GiB of encoded cache objects. A project
set that exceeds this capacity keeps the established file-transfer path. The node
also reclaims unpinned old objects against actual pool pressure, reserving room
for missing downloads and the final extracted filesystem before admission. Initial
home/configuration, legacy project and inbox transfers can reclaim unused archives
as file headers exceed their initial space budget; protected current project
archives remain available. Native admission also reclaims cache for legacy disk
initialization. Native
pool accounting counts shared archive inodes once, including temporary run pins.
Pins protect extraction against concurrent eviction and are removed after the
private filesystem is durably initialized. Cache files are private to Cairn;
repository code receives only its independent writable filesystem.

Background downloads hold the storage mutation lock to preserve budget admission,
but yield within the 10 ms priority polling interval when any foreground waiter
appears. The priority lock is visible across the connector and controller
processes. Cancellation discards the incomplete download before releasing the
mutation lock. Heartbeat and lease renewal do not await downloads.

## Compatibility and recovery

The installation data epoch stays unchanged. Conversation filesystems, journals,
database records, published manifests and storage grants retain their format.
The new ZIPs and JSON descriptors are disposable caches, not a new backup format;
this does not reopen the historical conversation-archive behavior in ADR-0008.

The manager adds optional archive descriptors to the prepare response. Old
connectors ignore them and request complete legacy staging trees. A new connector
requires the controller's `projectArchives: 1` health capability before omitting
project files. An old controller therefore receives the original complete tree,
including during a mixed-version update. The archive fields exist only in the
node's local, attempt-scoped runner plan, not the manager's authoritative plan.

Controller startup fences interrupted attempts rather than replaying their local
plans. After interruption, the connector regenerates staging from the manager's
plan: an older connector restores the legacy tree. Before the first successful
initialization, an incomplete `.cairn-initial-seed` is discarded and rebuilt; no
workload has been admitted at this point. After initialization, the existing disk
is authoritative and its completion marker bypasses extraction. Neither a new
commit nor a cache eviction overwrites an existing conversation's work.

Cache failure or an oversized archive falls back to legacy preparation. A damaged
master cache is discarded and rebuilt from the currently authorized remote. Node
archive staging errors remove partial pins and request a complete legacy seed;
authorization failures remain failures. An unavailable archive catalogue never
invalidates node leases. Invalid or interrupted downloads never become cache hits. Archive distribution checks
current agent project/node grants, backend, node admission and trusted native
people; run-scoped downloads also retain the existing attempt/provenance checks.

## Reproducible local measurements

Run the production archive/file-frame implementations through the isolated
backend launcher:

```sh
mkdir -p "$HOME/.cache/cairn-archive-measurement"
TMPDIR="$HOME/.cache/cairn-archive-measurement" pnpm test:backend \
  -p cairn-installation --lib measure_project_archive_transfer -- --ignored --nocapture
```

Workload: 4,096 files of 4 KiB repeated text plus 8 MiB pseudorandom bytes
(StdRng seed 42), 24 MiB total, three alternating old/new installations. The old
path receives existing JSON/base64 file frames with per-file `sync_all`; the new
path extracts ZIP and finishes with filesystem synchronization. Archive encoding
and frame encoding are timed separately. The command prints process CPU time,
encoded bytes, installation latency, first/warm archive read latency and host load.

On 2026-10-23, an i9-14900K, Linux 7.2.9, local ext4/NVMe, 94 GiB RAM, with
concurrent builds/tests, produced these samples (host load at the end: 18.29 / 17.56 / 13.95):

| Metric | File frames | Prepared ZIP |
| --- | ---: | ---: |
| Encoded bytes | 33,854,028 | 8,924,901 |
| First installation | 48,818 ms | 6,047 ms |
| Second installation | 41,313 ms | 2,738 ms |
| Third installation | 41,229 ms | 2,874 ms |
| First installation CPU | 2.152 s | 0.347 s |
| Second installation CPU | 2.043 s | 0.341 s |
| Third installation CPU | 2.136 s | 0.330 s |
| Encoding | 1,032 ms | 861 ms |
| First / warm archive read | Not applicable | 3.85 / 2.65 ms |

The ZIP reduces transferred bytes by 73.6%; these local installation samples are
8.1–15.1 times faster. The extraction target is a regular local filesystem, not
the production ublk journal, so this is not a production startup forecast.
First/warm reads are page-cache eligible; a genuinely cold device read and the
production network path are not measured by this harness. An earlier tmpfs run
gave median 483 ms versus 305 ms installation, demonstrating why tmpfs results
must not be presented as physical disk measurements.

Native nodes can also prepare [local disk bases](NATIVE-BASES.md) from verified
ZIPs to avoid expanding the same project for each new conversation.

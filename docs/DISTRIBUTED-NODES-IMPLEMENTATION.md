# Distributed nodes — implementation progress

> Historical implementation notes. [ADR-0009](adr/0009-s3-backed-disks-required-on-every-node.md) replaces local disk transfers and dm-era; PR status must be checked on GitHub.

The approved target is [DISTRIBUTED-NODES-SPEC.md](DISTRIBUTED-NODES-SPEC.md).
The branch implements the approved CPU-only scope. GPU passthrough remains deferred.
The pull request remains draft until its final CI checks have passed; this work
has not been merged, deployed or released.

## Implemented

- Single-use registration, node identity/revocation, detected capabilities and
  tags, per-agent grants including Main, and web/Android management.
- Outbound controller transport, private workspace staging, project/artifact
  routing (artifact responses preserve the channel’s snapshot `length` in
  `X-Cairn-Artifact-Size` independently of HTTP framing; no node upgrade is needed), chat/inbox streaming, scoped native-auth relays and the run's MCP channel
  (`cairn_workspace` and gateway connections), forwarded over the node session.
  Provider account refresh and 1Password access stay under master control.
- Atomic slot admission, shared CPU/RAM/disk budgets including retained stale disks,
  resource pressure, preferred/strict placement, bounded capacity waits and
  immediate request failure without ending the conversation.
  Automatic placement picks the authorized node with the largest free-slot fraction
  (a preferred node wins, ties keep the master runner). Agents discover their
  authorized nodes, tags, shared budgets and free slots with `list_nodes` before calling
  `move_to_node` with a destination. Agents never request resource allocations.
- Agent access can be granted from each node's card; a newly connected node
  asks for it right after enrollment and reports why it cannot take work.
  Conversations only show execution details once remote nodes are relevant.
- Owner alerts when a conversation waits for its node, fails over, fails to move
  or cannot save a recovery point: a message in the conversation, web push and
  an Android notification, repeated at most hourly per conversation and kind.
- Old disks left on a node (conversations now running elsewhere, or copies set
  aside by a restore) are reported per node and freed only on explicit request,
  since a disk abandoned by failover can hold changes newer than its recovery point.
- Each move is announced in the conversation. Slot counts and shared budgets are
  configured per node; there are no per-conversation resource reservations.
- Local and remote execution leases, fencing, durable movement requests, repeated
  stop attempts, full environment transfer and automatic recovery on compatible
  authorized nodes. Idle movements stay idle, explicit cancellation is preserved,
  and an unusable newest backup falls back to an older complete point.
- Coherent guest filesystem capture, content-addressed 4 MiB disk blocks,
  encrypted master/S3 recovery points, dependency retention, quota checks,
  periodic/final captures, authenticated restoration and archive purge.
  Incremental publication reuses immutable blocks authenticated by retained
  manifests without rereading their payload. New blocks and unpublished cache
  entries are verified before publication; restoration always verifies again.
  There is no scheduled full-content audit. If restoration discovers damaged
  cached ciphertext, it removes that block. If no valid copy can be read, it
  invalidates the incremental baseline so the next full capture can repair
  current blocks.
  An unavailable or corrupt S3 copy also loses its upload receipt. Damage can
  remain undetected until a restore needs the block. Publication, restoration,
  moves and purge serialize per conversation. Two publications can progress
  concurrently, while cache admission and eviction retain their shared guards.
  The node's separate dm-era tracking survives reboots but expires a baseline
  after 24 hours, forcing a full local disk copy and index. Previously received
  blocks are still reused by the master, so this does not resend the whole disk.
  Copying can keep the VM paused when the filesystem cannot reflink. A failed
  capture also clears the master's baseline; an offline node is not guaranteed
  an incremental local copy on its next capture.
  New blocks use a versioned binary AES-GCM format: 8 header bytes, a 12-byte
  nonce and a 16-byte tag, with no base64. Legacy double-base64 blocks remain
  readable and reusable without a forced scan or rewrite of existing backups.
  S3 keeps a second copy and verifies each new upload by downloading it once.
- Bulk snapshot/archive responses use a negotiated continuous binary HTTP upload,
  with eight buffered 64 KiB frames, cancellation and explicit completion. Old
  nodes/masters continue using acknowledged JSON frames. Control and log messages
  retain that protocol. Each bulk upload is bounded to 120 seconds; stalled input
  and readers fail explicitly, including responses without a declared length.
- Recovery settings, visible capture age/errors and dated restoration on web and
  Android. Chat remains current even when restoring an older disk state.
- Assisted Linux installation, a root-owned systemd supervisor, immutable master
  image selection, bounded drain/stop, health rollback, retried acknowledgements
  and retained compatible runtimes. Offline nodes catch up on reconnect. Older
  guest images without active-capture support keep executing and report the
  limitation; their stopped disks can still be captured.

## Validation

The repeatable checks live beside the implementation:

- `pnpm test:backend`: registration, grants, concurrent admission, quotas,
  encrypted block publication, retention, failover fencing, transfer cancellation,
  lost stop retries, current grants on worker recovery, native-auth refresh and
  remote run-scoped MCP.
  The outbound idle-movement test copies an environment between two separate
  controller directories, moves it back without a destination execution history,
  and checks both failed transfer and cancellation during capture.
  An inotify regression verifies that unchanged publication does not read cached
  blocks. Restoration detects corruption and prepares the next capture to repair
  it. The backup fixture checks the stored size and restores mixed legacy/binary
  blocks, including after removing the master cache in the S3 fixture. Binary
  vault tests reject tampering, truncation and a different owner/purpose; the
  existing Node-generated credential fixture remains compatible.
  Transport tests cover ownership, truncation with/without a length, bounded backpressure,
  consumer cancellation and legacy JSON responses.
- `crates/installation/tests/node_performance.rs`: opt-in benchmarks for initial, unchanged
  and 4 MiB delta publication; real outbound relay with
  0/50 ms injected request latency. Build with `CARGO_PROFILE_RELEASE_LTO=false
  CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16 cargo test --locked --release --test
  node_performance --no-run`, then run the emitted executable alone with
  `--ignored --nocapture --test-threads=1`. Fixtures use tmpfs and loopback: results
  measure CPU/logical I/O/protocol overhead, not a physical disk or real WAN.
  `NODE_STORAGE` reports the actual encrypted block bytes after initial capture.
  Conversation archival and restoration retain a sequential 256 KiB outer
  protocol, so their gains differ from the 4 MiB snapshot fixture. Normal node
  movements use recovery-point blocks, not this archive transfer loop.
- `python3 scripts/tests/guest_projects_test.py`: real mount-namespace regression for
  atomic read-only project publication, retry and reboot restoration. An
  unprivileged observer must never gain write access while mounts are prepared.
- `python3 scripts/tests/node_supervisor_test.py`: four shipped-supervisor CLI scenarios
  covering update, rollback, lost acknowledgement and retained-runtime download.
  Docker and master responses are fixtures.
- `uv run --with 'moto[server]==5.2.3' python scripts/tests/node_s3_test.py`: actual AWS CLI
  against a loopback S3-compatible fixture with synthetic credentials. Covers
  remote retrieval after cache removal, integrity, retention, quota exhaustion,
  older-point fallback and complete purge. This is not a live AWS account test.
- `pnpm exec playwright test --project=journeys-nodes --workers=1`: enrollment,
  configuration, revocation, recovery settings, placement and backup age.
- Android `NodePlacementTest` / `NodePlacementDeviceTest`: placement and node
  management through the real UI with a fixture HTTP backend. The device class is
  included in Android CI; JVM results alone are not device evidence.
- `node tests/runner-smoke.mjs <image>` also verifies active capture and block
  integrity on the exact image built by CI.
- `node tests/node-recovery-smoke.mjs <assets> <cairn-binary>`: real KVM cold restore
  between separately stopped/started controllers. Untracked files, an installed
  tool, a Docker volume and real native Codex/Claude sessions survive. The native
  CLIs use a local model-response fixture and synthetic credentials; both session
  identities and initial conversation history are checked after restore. The test
  also captures a stopped destination disk before any execution and verifies
  lease-expiry shutdown with recoverable exit code 75.

The latest local KVM measurement used a 1 GiB writable disk: 71,303,168 bytes for
its base, 25,165,824 bytes for its next increment, a 341 ms second capture pause
and 17,584 ms of local indexing. This is a small fixture under shared test load,
not a throughput or recovery-lag guarantee for a full workstation. Local indexing
skips holes with `SEEK_DATA` but still reads every allocated block; only changed
nonzero blocks cross the network. Continuous and end-of-turn recovery points are
taken only for conversations on remote nodes: the master runner fails together
with the master, so its conversations are captured when they move. The guest's
dm-era target records written 4 MiB blocks, so captures that continue from the last
published point copy only those blocks; any doubt falls back to a full copy (see
[ADR 0005](adr/0005-guest-dm-era-write-tracking.md) and
[INCREMENTAL-VM-BACKUP-WRITE-TRACKING.md](INCREMENTAL-VM-BACKUP-WRITE-TRACKING.md)).

The VM fixture used published image layers with verified digests and this
branch's guest binary. It was not a production deployment or two physical hosts.
Live provider sign-in, physical shutdown and WAN performance remain operational
acceptance checks on the user's machines, rather than claims made by these tests.

## Delivery gate

Final lint/typechecking, complete crates/installation/web suites, Android checks and PR CI
must pass on the delivered revision. The review found no remaining documented
standard violations or definite movement/recovery defects after corrections;
one optional repeated-connection-argument maintainability observation remains.

## Try the connector from a matching build

Create an enrollment code in Atelier → Nodes. On a trusted Linux x86-64 machine
with the matching `cairn` binary, use Bash to read the code without putting it in
shell history or process arguments:

```bash
read -rs -p 'Enrollment code: ' CAIRN_ENROLLMENT_CODE
printf '%s' "$CAIRN_ENROLLMENT_CODE" | cairn node-enroll https://your-master.example "$HOME/.local/state/cairn-node"
unset CAIRN_ENROLLMENT_CODE
cairn node-connect "$HOME/.local/state/cairn-node"
```

The state directory is private and the identity file uses mode 0600. Enrollment
refuses to replace an existing identity. A revoked connector exits and must be
registered again using a new private state directory. Production connections
require HTTPS, do not follow redirects, and make only outbound requests. HTTP
is accepted only for loopback testing. The installation's own runner is the one
exception: it reads disks from the exact origin its manager records in their
shared `DATA_DIR` (`http://manager:4310` in the generated Compose file). Only the
manager writes that record, from its own configuration; a node or a request
never supplies it. No root privileges, host reformatting or systemd changes are
performed by these commands.

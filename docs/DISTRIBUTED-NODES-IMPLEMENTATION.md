# Distributed nodes — implementation progress

The approved target is [DISTRIBUTED-NODES-SPEC.md](DISTRIBUTED-NODES-SPEC.md).
The branch implements the approved CPU-only scope. GPU passthrough remains deferred.
The pull request remains draft until its final CI checks have passed; this work
has not been merged, deployed or released.

## Implemented

- Single-use registration, node identity/revocation, detected capabilities and
  tags, per-agent grants including Main, and web/Android management.
- Outbound controller transport, private workspace staging, project/artifact
  routing, chat/inbox streaming and scoped native-auth relays. Provider account
  refresh and 1Password access stay under master control.
- CPU/RAM admission, persistent-disk accounting including retained stale disks,
  resource requests, preferred/strict placement, bounded capacity waits and
  immediate request failure without ending the conversation.
  Automatic placement picks the authorized node with the most free CPU/RAM
  (a preferred node wins, ties keep the master runner). Agents discover their
  authorized nodes, tags and free capacity with `list_nodes` before calling
  `request_capacity`.
- Agent access can be granted from each node's card; a newly connected node
  asks for it right after enrollment and reports why it cannot take work.
  Conversations only show execution details once remote nodes are relevant.
- Local and remote execution leases, fencing, durable movement requests, repeated
  stop attempts, full environment transfer and automatic recovery on compatible
  authorized nodes. Idle movements stay idle, explicit cancellation is preserved,
  and an unusable newest backup falls back to an older complete point.
- Coherent guest filesystem capture, content-addressed 4 MiB disk blocks,
  encrypted master/S3 recovery points, dependency retention, quota checks,
  periodic/final captures, authenticated restoration and archive purge.
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
  lost stop retries, current grants on worker recovery and native-auth refresh.
  The outbound idle-movement test copies an environment between two separate
  controller directories, moves it back without a destination execution history,
  and checks both failed transfer and cancellation during capture.
- `python3 tests/guest_projects_test.py`: real mount-namespace regression for
  atomic read-only project publication, retry and reboot restoration. An
  unprivileged observer must never gain write access while mounts are prepared.
- `python3 tests/node_supervisor_test.py`: four shipped-supervisor CLI scenarios
  covering update, rollback, lost acknowledgement and retained-runtime download.
  Docker and master responses are fixtures.
- `uv run --with 'moto[server]==5.2.3' python tests/node_s3_test.py`: actual AWS CLI
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
- `node tests/node-recovery-smoke.mjs <assets> <leo-binary>`: real KVM cold restore
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
still reads the entire disk; only changed nonzero blocks cross the network.

The VM fixture used published image layers with verified digests and this
branch's guest binary. It was not a production deployment or two physical hosts.
Live provider sign-in, physical shutdown and WAN performance remain operational
acceptance checks on the user's machines, rather than claims made by these tests.

## Delivery gate

Final lint/typechecking, complete backend/web suites, Android checks and PR CI
must pass on the delivered revision. The review found no remaining documented
standard violations or definite movement/recovery defects after corrections;
one optional repeated-connection-argument maintainability observation remains.

## Try the connector from a matching build

Create an enrollment code in Atelier → Nodes. On a trusted Linux x86-64 machine
with the matching `leo` binary, use Bash to read the code without putting it in
shell history or process arguments:

```bash
read -rs -p 'Enrollment code: ' LEO_ENROLLMENT_CODE
printf '%s' "$LEO_ENROLLMENT_CODE" | leo node-enroll https://your-master.example "$HOME/.local/state/leo-node"
unset LEO_ENROLLMENT_CODE
leo node-connect "$HOME/.local/state/leo-node"
```

The state directory is private and the identity file uses mode 0600. Enrollment
refuses to replace an existing identity. A revoked connector exits and must be
registered again using a new private state directory. Production connections
require HTTPS, do not follow redirects, and make only outbound requests. HTTP
is accepted only for loopback testing. No root privileges, host reformatting or
systemd changes are performed by these commands.

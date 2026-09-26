# Distributed nodes — implementation progress

The approved target is [DISTRIBUTED-NODES-SPEC.md](DISTRIBUTED-NODES-SPEC.md).
This remains a draft implementation. Do not merge or deploy it as a completed
node system: final integration, review and acceptance tests remain.

## Implemented in the working branch

- Single-use registration, node identity/revocation, inventory and per-agent grants
  including Main; owner management on web and Android.
- Outbound controller transport, private workspace staging, project/artifact
  routing, result and inbox streaming, scoped native-auth relays.
- CPU/RAM admission and separate persistent-disk allocation, execution leases,
  controller fencing, resource requests and pause/restore orchestration.
- Coherent guest filesystem capture, content-addressed 4 MiB disk blocks,
  encrypted master/S3 recovery points, dependency retention, quota checks,
  periodic/final captures and authenticated restore grants.
- Recovery settings (destination, interval, retention and master budget), and
  web/Android conversation controls for preference, strict pinning and movement,
  capture age, errors and dated restoration.
- Assisted Linux installation, a root-owned systemd supervisor, immutable master
  image selection, bounded drain/stop, health rollback, retried acknowledgements
  and an approved retained-runtime package catalogue.
- Archive transfer across separate filesystems, current-grant checks for archive
  placement, and disk allocation retained until confirmed deletion.
- Configurable disconnect, maintenance and capacity-wait limits; detected system
  tags, cgroup memory detection and default host resource margins.

These code paths are at different validation stages. Native provider resumption,
complete outbound execution and real S3/Android scenarios are not yet established.

## Current validation evidence

- Twenty node tests passed, covering registration, grants, admission, placement,
  scoped native authentication and archive transfer between distinct directories.
- A further encrypted-backup integration test passed through the actual outbound
  relay: exact restoration, changed-block reuse and refusal to publish corrupt
  data while retaining the previous recovery point. This uses a fixture controller.
- Three shipped-supervisor CLI tests pass: update, rollback and lost acknowledgement
  retry without restarting the healthy container. Docker and the master are fixtures.
- Android Kotlin compilation passed. Device and final merged-branch checks remain.
- The recovery-settings API test was observed failing (404), then passing; invalid
  configuration preserves the last good settings.
- Two block/capture tests pass, including exact reconstruction, corruption
  rejection, unchanged-block reuse and cleanup after a lost pause acknowledgement.
- A real Firecracker test cold-restored on a second, separately stopped/started
  controller: untracked files, installed tool, synthetic native-session file bytes
  and a Docker volume survived. On a 1 GiB fixture disk, measured guest pause was
  336 ms, indexing 20,248 ms, and 10 blocks / 41,943,040 bytes transferred.
  This proves filesystem continuity, not native Codex/Claude session resumption.
- The local VM fixture uses published image layers with verified digests and the
  current branch's guest binary; it is not a production image or two physical hosts.
- Rust all-target checking and web typechecking passed at intermediate revisions;
  final checks must run on the delivered commit. No Android device test yet.

## Remaining implementation order

1. Integrate current main's shared provider accounts and S3-compatible storage;
   preserve the distributed execution changes and recheck authentication protocols.
2. Finish archive/stale-disk and backup cleanup, quota/retention/S3 scenarios,
   placement races, partition/fencing/cancellation and update/runtime-cache tests.
3. Extend the real KVM scenario with incremental captures and lease expiry;
   complete web and Android device journeys and native-provider validation.
4. Full repository checks, two-axis review, commit/push the draft PR and verify CI.
   Main already contains fixes for the earlier Android gallery CI failure.

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

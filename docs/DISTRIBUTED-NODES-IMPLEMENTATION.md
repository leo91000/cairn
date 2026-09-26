# Distributed nodes — implementation progress

The approved target is [DISTRIBUTED-NODES-SPEC.md](DISTRIBUTED-NODES-SPEC.md).
This branch is a draft implementation, **not the completed distributed execution
feature**. Registration does not make a machine eligible to run a conversation.
Do not deploy this as a complete node system or merge it on that basis.

## Implemented

- Owner-authenticated enrollment codes, expiring after ten minutes and consumable
  once, including concurrent requests. Only token hashes are retained by the master.
- Revocable node identities, authenticated outbound presence, and public inventory
  without node credentials. A heartbeat indicates connectivity, not a VM lease.
- Linux x86-64 connector detection of CPU, RAM, disk capacity and accessible KVM.
  The connector does not start a VM or receive provider credentials.
- Node names, user tags, admission preference and CPU/RAM/disk ceilings. These
  preferences are saved for the forthcoming scheduler, not currently enforced
  against remote VMs (there are no remote VMs yet).
- Explicit node access per agent, including Main; existing agents default to the
  current runner. Older clients preserve the node axis when saving other fields.
  Fresh local executions cannot bypass node restrictions.
- Web and Android management screens and agent node selectors. Both screens state
  that remote execution is under development.

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

## Still required before the approved feature is complete

- One execution interface for the existing runner and remote runner, outbound
  streaming transport, file imports/exports, chat input and scoped credential relay.
- Resource reservation/admission, agent capacity requests and bounded waiting,
  node preference versus strict pinning, and user-visible conversation states.
- Full environment movement with compatible runtime retention, coherent local
  capture, encrypted incremental master/S3 backups, manifests, retention and quotas.
- Durable attempt ownership, local watchdog fencing, automatic restore/failover,
  stale-node reconciliation and preservation of explicitly cancelled conversations.
- Assisted installation, systemd shutdown preparation and master-approved automatic
  updates with verification, pause/resume and rollback.
- Complete web/Android journeys for execution, migration, backup and recovery.
- Two-controller KVM integration with files, Docker and native provider sessions;
  partition/shutdown/update failure scenarios; S3 restore; Android device tests.

## Evidence collected during this implementation

- The enrollment integration test initially failed with HTTP 404; the implemented
  route passes single-use registration and immediate revocation checks.
- The connector CLI initially failed as an unknown command; the implemented command
  registers through a real loopback HTTP server and writes a private identity without
  printing its token.
- KVM API version 12 is accessible in this development VM. This is not a migration test.
- A local 8 MiB FICLONE probe returned EOPNOTSUPP. No instant snapshot latency claim
  is made for this filesystem; no active VM disk was copied by the probe.
- The installed Android SDK tools cannot find `platforms;android-37`, required by
  the repository. No SDK or Gradle version pins were changed to work around this.
  Android source changes are not evidence of a successful Android build or device run.

## Review — Standards

The independent standards review found a revocation/edit dead end. Revocation now
removes explicit agent grants atomically; grant validation and agent saving share
the same writer transaction, preventing a concurrent editor from restoring a
revoked grant. The reviewer confirmed both corrections. No remaining standards
findings were reported.

## Review — Spec

The independent specification review identified the same defect (corrected), a
missing local-runner inventory entry, incomplete available-resource/version
reporting, and the remaining distributed execution feature listed above. Both
screens now display detected numeric capacities and runtime identity, but this
does not establish schedulable capacity or update compatibility. No scope creep
was reported. The draft is not a completed implementation of the approved spec.

Review summary: Standards — 0 unresolved findings; Spec — 3 grouped incomplete
areas, the largest being distributed execution, backup and recovery.

## Validation of the implemented slice

- Nine native node tests pass: single-use and concurrent enrollment, authentication
  scope, revocation, resource bounds, independent Main permissions, legacy updates,
  real HTTP connector/private identity, and MCP update defaults.
- Native suite coverage was collected across the full-suite attempt and subsequent
  targeted runs, rather than one uninterrupted green run. One worker test failed
  when invoked without the repository's credential-isolating launcher, then passed
  with `pnpm test:backend`.
- The full JavaScript run had 249/258 passing tests initially: one expected-policy
  assertion required the new default; eight timing-sensitive failures occurred
  while other builds were running. After the assertion fix, all five affected
  files pass with one worker (56 tests). The new contract test and MCP tests also
  pass (10 tests). No application behavior was changed to mask timing failures.
- Typechecking and the web production build pass. The Chromium owner journey
  registers, configures and revokes a node successfully.
- Repository lint/format checks, `cargo check --locked --workspace --all-targets`
  and Clippy across all workspace targets with warnings denied pass.
- Android setup succeeds, but SDK package installation rejects platform 37. The
  Gradle attempt was stopped before source validation after several minutes of
  configuration; neither Android compilation nor an emulator run is claimed.
- No two-controller KVM migration, S3 recovery, provider credential migration or
  automatic-update test has been implemented or claimed for this draft.

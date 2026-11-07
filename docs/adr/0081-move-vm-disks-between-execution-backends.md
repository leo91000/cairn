# Explicit backend moves reuse VM disks

Date: 2026-11-07.

An explicit node move may change a conversation's execution backend. A disk
created in a VM retains its flat ext4 layout and engine session when mounted by
a native controller, and can return to a VM without copying its files. This
updates ADR-0050 and ADR-0054 while preserving ADR-0073's automatic placement,
ordered priorities, fixed profiles and concrete backend retention after initial
admission. Ordinary retries never choose a different backend implicitly.

Execution backend and disk format are separate facts. A prepared descriptor
without an explicit format retains the meaning of its original backend. The
native runtime records VM disk provenance separately from its current component
version; changing tools must not pin the conversation to an obsolete controller.
Native destinations advertise `vmDisks`; admission and restore refuse VM disks
when that capability is absent. Only the current flat layout is supported;
retired paired layouts are not reintroduced.

We deliberately allow the native host kernel to parse guest-written ext4. The
conversation must meet the same live human trust, parent/plugin provenance and
destination-specific administrator approval rules as other native executions.
An explicit owner's move may adopt historical work; an agent moving itself keeps
its recorded authority. The accepted trusted-native risk includes this extra
kernel parser exposure. This does not permit following guest-written symlinks in
the privileged controller: validate guest-derived bind and runtime paths before
writing them. No FUSE layer or whole-filesystem reconstruction is added.

An idmapped mount presents disk UID/GID 1000 as the conversation's current engine
identity, so its writes remain 1000 on disk. Other guest identities map to the
conversation's subordinate ranges. Guest root remains host root for controller
preparation and is unmapped inside the protected enclave. Its namespace
supervisor/runtime state uses the separate on-disk identity 65537. Administrator
execution uses its reviewed host engine identity. VM disks never enter the native
recursive ownership reconciliation; remounting translates identities without
rewriting files. Native-born disks retain the existing reconciliation, growth,
independent backend namespace and verified physical-close behavior.

Source-only disk hydration is enabled only after the source has been fenced.
It remains bound to the pending move, current destination grants, retained
non-placement access and native authority. It grants no source workload lease
and disappears when the move completes or fails.

A disk created natively still requires one physical conversion before a VM can
run it. Keep the source until publication and destination restore succeed.
Preserve the existing refusal when rootless Docker state requires export or
cleanup. Conversion supports missions without chat records: active missions
continue with visible history in a fresh engine session, including after a
failed conversion is retried; chats wait for their next message. A self-directed
conversion retains a bounded operation proof after its engine stops, and checks
cancellation and current grants before subsequent effects. Active conversions
prevent agent merges; destination administrator approvals are never inherited.

Web, Android, public MCP and the installation tool expose the same operation.
`migrate_chat_backend` remains a confirmed compatibility entry point; callers may
identify a chat or a mission run. Expired environments retain their separate
fresh-environment flow and do not pretend to recover deleted files.

Database layout 28 stores the advertised capability, initially false until a
fresh report. Startup upgrades layout 27 transactionally. Older managers reject
layout 28; a rollback needs a pre-upgrade database backup, or recovery using this
version. Léo accepted incompatible rollback on 2026-10-24. No claim that older
strict typed readers accept newly written move state is retained.

Qualification must cover real protected native ext4/idmap/ublk, native writes
stored as guest UID 1000, publication, eviction and S3 restore, with comparative
read/write/CPU/encoded-byte measurements in the PR. Offline ownership checks do
not establish a successful Firecracker guest boot after returning from native;
that qualification limit remains explicit. Measurement artifacts stay out of
the committed source tree.

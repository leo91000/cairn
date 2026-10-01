# Initialize empty Codex schemas in the image

Date: 2026-10-01.

## Problem

New conversation homes repeat Codex's SQLite migrations on the durable FUSE
disk. A native turn through a local synthetic Responses endpoint writes
6.35–6.43 MB in 1,226–1,252 callbacks. The same workload with preinitialized
schemas writes 3.62–3.66 MB in 714–719 callbacks. This does not explain every
production write: fresh production runs write approximately 30 MB, and their
remaining finalization delay still needs separate attribution.

The comparison runs the real Codex 0.159.3 binary in disposable Firecracker VMs
with new 32 GiB disks. It needs no account or remote model. Its raw metadata is in
[the comparison](../benchmarks/codex-state-initialization-2026-10-01.json).
The prototype builds templates on tmpfs during each seeded run; that work is
included in its total time. These are local measurements, not deployment proof.

## Decision

The image builder initializes the exact installed Codex with an isolated empty
home, a local provider with no listener and networking disabled. It starts a
thread but never starts a model turn. Each database retains its native migration
checksums and schema; every runtime table is emptied. Secure deletion, vacuum,
row-count checks, integrity validation and SQLite backup produce self-contained
templates without WAL dependencies or deleted runtime payloads. Unsupported
virtual tables or absent migration metadata fail the build rather than silently
producing an unsuitable template. CLI update images regenerate the templates.

A Codex chat installs templates only when its home already exists and contains
no SQLite state, including WAL remnants. Existing databases, partial prior
installations, symlinked homes and custom SQLite files remain untouched. The
native migration path handles those homes normally. A missing template directory
also keeps compatibility with older retained guest images.

Each new file is written privately, owned by the agent, synced, published without
replacement and followed by a directory sync before Codex starts. An interrupted
installation leaves complete templates and falls back to normal native migration
on restart. No conversation state or account is shared between homes. Templates
reduce redundant initialization; all subsequent writes still use the durable
journal and published conversation snapshots.

## Finalization

The adapter measures `codex_shutdown`. The guest records `guest_finalize` phases
for waiting for the process, reading the result, flushing the disk and sending
the exit event. These contain identifiers and timing fields, never file contents.

The final flush targets the filesystem containing the persistent overlay upper
directories using `sync --file-system`, rather than global `sync`. A failed flush
does not emit a successful exit event. Journal acknowledgements, crash recovery,
controller protection and publication receipts remain required.

## Validation

Tests cover migration retention, removal of runtime data including deleted
payloads, template integrity, preservation of existing and partial state,
private permissions, invalid paths and propagation of a failed final flush.
The image smoke test verifies that embedded native templates contain no runtime
rows. Release qualification must still exercise actual guest writes and reopen,
repeated publications under load and nested Android on the exact release image.
Production startup and finalization measurements remain a separate release gate.

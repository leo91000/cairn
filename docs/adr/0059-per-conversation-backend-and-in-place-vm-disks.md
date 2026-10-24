# Conversations keep their backend; native nodes mount VM disks in place

Date: 2026-10-24.

Agents no longer select an execution backend. An agent's node grants are its only
permission: it may hold Firecracker nodes, native nodes and the current runner
together. Each conversation records the backend its environment was created on
(`run.executionBackend`, then `checkpoint.prepared.backend`). A new environment
starts in a VM whenever the agent may use a VM node; it starts natively only when
the agent is granted native nodes only, or when a native node is explicitly pinned
or preferred before its first start. Pinning an existing environment to a node of
the other backend is refused: switching backend is an explicit move.

Native authority is unchanged. A native node must enable native execution and
trust every person directing the conversation; provenance, delegation ceilings and
fresh Beacon membership still apply. Every conversation records its provenance,
including VM ones, so a later native move can be checked. An owner moving a
conversation to the other backend explicitly adopts it, as an owner migration did
([ADR 0054](0054-explicit-conversation-backend-migration.md)). An agent moving
itself with `move_to_node` keeps its conversation's provenance; the native
destination must allow it.

## Moving between backends

A move to a node of the other backend is an ordinary node move: execution pauses,
the source publishes its point, the destination restores it, and the conversation
resumes with its engine session. The move records the destination backend and
updates both the run and its checkpoint when the destination resumes.

A native node mounts a VM disk without copying it. Its controller attaches each
filesystem of a flat or paired VM layout on ublk and mounts it with an idmapped
mount: disk root stays root, and the VM agent identity (UID/GID 1000) appears as
the native workload account, whose writes are stored as 1000 again. The workload
binds the guest paths from the overlay upper layer and, for a paired disk, from the
workspace filesystem, including the home directories the guest redirects there.
Every bound component is checked without following links before the workload
starts, since the guest wrote the disk. The disk keeps its VM layout and its VM
image identity (`runtime.disk = "vm"`, `runtime.vmRuntimeId`); checkpoints of such
a disk carry no `native-ext4-v1` kind, and a later move back boots the same disk in
a VM with that image. System packages installed in one backend and running
processes do not follow.

This deliberately lets a native host kernel mount a filesystem written inside a
VM. The trust model of native nodes already accepts that the agent runs code
directly on the host kernel; the native authority checks above therefore gate
this path exactly like native execution itself. Only native nodes reporting the
`vmDisks` capability (a root controller able to create user namespaces for
idmapped mounts) receive VM disks.

A disk created natively (`native-ext4-v1`) cannot boot in a VM. Moving it to a VM
node runs the existing conversion once: the active turn stops, the chat is paused
until the converted point is restored. The next chat message starts a fresh engine
session; a running mission resumes automatically with its visible history in a
fresh session, and waits on that mission remain pending during conversion. The converted disk is a VM disk, so later moves are ordinary.

## Interfaces

Web and Android list VM and native nodes together on agent pages, show each
destination's backend when moving a conversation and explain the consequence of
switching backend, including the one-time conversion. `list_nodes` reports each
node's backend to agents. `get_chat_backend_migration` describes a conversation's
backend, disk format and other-backend destinations;
`migrate_chat_backend` moves a conversation to one of them through the same path.

## Compatibility

Stored agent profiles may still carry the removed `access.backend` field; it is
ignored and dropped on the next save. Runs, checkpoints and manifests gain only
additive fields (`diskFormat`, `diskRuntimeId`, `runtime.disk`,
`runtime.vmRuntimeId`). Existing runs and checkpoints already record their
backend, so no migration is needed, and the installation data epoch is unchanged:
the previous release reads everything this one writes. After a rollback, that
release again infers a profile's backend (Firecracker once the field is dropped):
a conversation moved to a native node, or one using an agent granted both kinds of
node, then waits with the previous release's "execution backend changed" error
until it is moved again with this release. Files and recovery points are never
rewritten by a rollback. Native nodes without the updated controller do not report
`vmDisks` and keep receiving only natively created disks; an older native
controller also refuses to mount a VM disk because its runtime identity differs.

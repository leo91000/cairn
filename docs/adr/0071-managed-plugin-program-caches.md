# ADR-0071: Managed plugin program caches

- Status: Accepted
- Date: 2026-10-26
- Extends: [ADR-0045](0045-declarative-plugin-contributions.md)

## Context

Every `exec.run` used a new temporary home. The 1Password CLI therefore could not
reuse its official encrypted in-memory cache between authorized conversations,
increasing repeated upstream requests against a shared service-account quota.
Saving decrypted secret values in Cairn would introduce another secret store and
make credential rotation and permission changes harder to enforce.

## Decision

A declared program may opt into a host-managed foreground daemon through static
`daemonArgs` and a reserved socket environment variable. Cairn starts the same
approved executable without credentials, checks readiness through its private Unix
socket, and gives clients that socket and home. On Linux, socket peer credentials
must identify the foreground child. The daemon is killed when its host exits;
on Linux 6.5 and later, cleanup also handles a vendor client's unexpected daemon
replacement through `SO_PEERPIDFD` on the private socket. This identifies the
actual socket peer without PID reuse or reading the non-dumpable CLI's `/proc`
files. Older kernels still kill the owned foreground child and remove the private
socket/home, but cannot reap an independently restarted vendor daemon through this
mechanism; that encrypted process cache expires under the vendor's timeout.

The pool belongs to one installation and keys by connection/program. Its identity
also includes the saved revision, approved package digest, executable and a digest
of the resolved client environment. Concurrent callers initialize one daemon.
Existing authorization and post-I/O checks apply on every call. Rotating or
changing an instance, disabling/deleting/disconnecting it, replacing its approved
package, or shutdown invalidates its daemon and cancels active calls. Draft and
disabled-instance tests use transient daemons rather than entering the pool.

Idle expiry is ten minutes. The installation permits 64 resident daemons and
evicts only entries without active leases, oldest first. If all are busy, a new
connection gets a retryable capacity error. Non-opted-in programs retain their
fresh temporary home. Daemon files are removed when the last active lease drops.

The 1Password package opts in using `op daemon --timeout 0`, `OP_SOCK` and
`OP_CACHE=true`. Cairn stores no decrypted item cache or command output; the
official CLI owns encrypted memory contents and refresh semantics. Agents must
still pipe plaintext output directly into the consuming operation.

## Consequences

Authorized agents on one connection can reuse official CLI caches without sharing
them with other connections or installations. Restart and eviction make the next
read cold. This does not reset or increase upstream quotas, and API savings depend
on vendor cache behavior. Connect support, quota-aware conversation scheduling and
any cross-connection cache sharing are separate work.

Tests cover concurrent sharing, isolation, permissions, credential-free daemon
startup, revocation during reads, revision rotation, disabled-owner tests,
shutdown, idle expiry, resource pressure and failed startup. An opt-in local test
exercises official CLI foreground startup and warm lease reuse without credentials
or network. These timings measure process management, not secret reads, freshness
or production quota savings; those require an available service-account budget.

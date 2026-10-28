# Automatic placement with per-agent node priorities

Date: 2026-10-27.

This extends the fixed agent backend in ADR-0050 while retaining its native
authority checks and ADR-0054's explicit environment migration.

## Decision

An agent may use `access.backend=auto` and authorize both VM and native nodes.
Its ordered `nodePriorities` are preferences within those live grants. Explicit
conversation placement wins; otherwise the first eligible priority with capacity
wins, followed by other eligible nodes using the existing headroom comparison.
Priorities never grant access or an administrator review.

The first admission atomically reserves capacity and replaces the run's automatic
backend with the selected node's concrete backend before workspace preparation.
Subsequent attempts and ordinary moves keep that backend. A stale admission
request cannot rebind a conversation. Preparation renews the reservation until
materialization hands it to the ordinary execution lease.

Automatic profiles record native provenance even when it is insufficient for a
native node: such conversations can use an authorized VM. Native execution still
requires current trusted people, bounded delegation, the node's applied mode and
its owner's explicit administrator review. Existing unknown mission provenance
never acquires native authority through automatic placement.
An unavailable native-authority service excludes native placement and preserves
VM admission. Concrete native conversations still require fresh host authority;
concrete VM conversations do not depend on that native-only endpoint.

Existing profiles remain fixed until edited. New app drafts start automatic with
only the current VM runner authorized. Changing a profile never converts existing
environments. Explicit migration for an automatic profile offers the other backend.
This conversion is optional. After success, both apps confirm completion before
offering a new explicit conversion; they do not claim the agent requires moving back.

## Schema deployment

Database layout 11 adds ordered agent node priorities and permits `auto` in agent
permissions. Stop the installation, back up its database and apply
[`0011-mixed-node-agents.sql`](../migrations/0011-mixed-node-agents.sql) using
`sqlite3 -bail`. Verify `PRAGMA integrity_check` returns `ok`,
`PRAGMA foreign_key_check` returns no rows and `PRAGMA user_version` is 11 before
starting the new installation. The migration refuses layouts other than 10,
preserves existing profiles, grants and run snapshots, and changes no runtime
configuration. For rollback, restore the offline backup and previous binaries;
layout 11 with automatic profiles is not readable by the previous release.

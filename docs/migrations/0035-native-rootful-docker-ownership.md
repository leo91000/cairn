# 0035 — Native rootful Docker ownership fence

Native runtime metadata advances from `ublk-rootless-v1` to
`ublk-rootless-v2` before mounting or reconciling identity. This changes no SQLite
schema, journal encoding, block receipt, physical lease or pending identity entry.
Readers accept historical `ublk` and `ublk-rootless-v1` metadata. Upgrading writes
the new runtime marker atomically without rewriting retained file data.

The reserved filesystem-root directory `docker-administrator` contains raw host
UID/GID values, ACLs and capabilities belonging to rootful Docker containers. The
identity reconciler excludes that entire subtree. Protected `docker` metadata and
ordinary work files retain their existing translation and replay contracts.
Mode changes preserve both stores and never open either as the other.

Earlier controllers reject the new storage marker before mounting/reconciling;
they cannot safely translate this raw store. Do not remove or downgrade the marker
to force a rollback. Use the matching controller to recover the environment or
restore a compatible offline environment together with its metadata. Existing
agent-installed Docker stores are not implicitly adopted into this new store.

Validation covers both historical native markers, future-marker rejection,
payload preservation, opaque ownership across both mode changes, and the real
native Docker/ublk runtime qualification. See [ADR-0085](../adr/0085-managed-administrator-docker-lifecycle.md).

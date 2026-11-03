# Installation database 23 → 24

`Store::open` adds explicit plugin contribution grants and credential suppliers
after the version-22 → 23 typed placement migration. Versions 11–21 first use
their existing sequential conversions to reach version 22. The entire grants
upgrade runs in one immediate transaction before the installation serves
requests.

The migration preserves the old per-agent, per-instance full-access grants and
the enabled, name-ordered legacy credential supplier for each package. It does
not infer a replacement supplier when a later access reduction removes the
selected instance. New grants made through setup can make an explicit supplier
choice when no supplier remains; routine profile and settings saves preserve the
empty tombstone.

Contribution grants, resource restrictions and supplier selections are stored
in separate tables. Foreign keys bind each selection to the existing agent,
instance and package. An unavailable package retains its existing full grant and
credential supplier so an owner can still reduce its access without losing
state. No package-wide permission or credential fallback is introduced.

The migration creates only the new grant tables, copies the existing full-grant
and supplier state, and advances `user_version` to 24 in the same transaction.
Failed migration leaves version 23 intact. Reopening version 24 performs no
additional migration. Downgrading a migrated database is unsupported.

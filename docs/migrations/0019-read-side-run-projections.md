# Installation database 18 → 19

The startup run-projection upgrade removes the generated `runs.data` and
`runs.summary_data` columns and exposes the same JSON through `run_views`.
Mutable lifecycle updates retain the physical columns, frozen snapshots, indexes,
identities and references without rebuilding the full wire JSON on each write.

The immediate transaction verifies both legacy columns, drops only those virtual
columns, creates the version-19 view, and advances `user_version` only after success.
A schema conflict or unexpected legacy layout rolls back every change and leaves
the version-18 input intact. Reopening version 19 performs no additional conversion.
The historical view definition is frozen in
`crates/installation/src/runs/projections/v19.sql`; later placement columns are
introduced by [version 20](0020-typed-placement.md).

Tests compare both exact wire projections and all retained physical columns,
exercise rollback/retry and simultaneous startups, and reopen the current Store.
See [storage performance](../STORAGE-PERFORMANCE.md) for the write-path rationale.

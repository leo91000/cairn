# Installation database 18 → 19

`Store::open` performs this migration before serving requests. In one immediate
SQLite transaction it verifies that `runs.data` and `runs.summary_data` are the
expected generated columns, removes those columns and creates the equivalent
`run_views` projection. It then sets `user_version` to 19 and commits. The
underlying run rows, identities, relationships and stored fields are retained;
the JSON projections remain available from the view.

An unexpected schema, conflicting view or other error rolls back the transaction,
including both column removals and the version update. The version-18 database
remains available for repair and retry. Reopening version 19 or a newer version
performs no further work in this migration. Keep a consistent database/WAL backup
before upgrading; there is no downgrade after a successful commit.

The tests in `crates/installation/src/runs/projections/tests.rs` upgrade a real
version-18 database, compare the original and new JSON projections, verify
integrity and foreign keys, exercise concurrent startups, and check rollback on
an unexpected schema or a conflicting view.

The historical version-19 view definition is frozen in
`crates/installation/src/runs/projections/v19.sql`: the projection upgrade must
not reference placement columns introduced by [version 23](0023-typed-placement.md).

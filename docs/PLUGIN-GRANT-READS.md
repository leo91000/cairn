# Agent resource-rule directory reads

The owner directory selects one agent's records after the exact UUID cursor in
SQL, with the management page size plus one lookahead record, before decoding
rules in Rust or fetching their resource and instance names. The existing byte
limit still decides the actual last item and continuation cursor. Catalogue
counts aggregate agent IDs in SQLite without decoding every rule in Rust.
Neither change adds an index, changes saved records, nor changes grant authority.

The reference is the all-record implementation in review tree
`89f0ead4f366ee7668891b2e6ea51e6df8f2846e`: load every installation grant, decode
it, enrich each matching rule, then apply the response bounds. The comparison
reproduces those query/projection boundaries and the bounded implementation.

Run from the repository root:

```sh
python3 scripts/measure-plugin-grants.py --output /tmp/plugin-grant-reads.json
```

[Recorded measurements](measurements/plugin-grants/sqlite-local.json) use Linux
x86-64, Python 3.14.7 and SQLite 3.53.1 on a host reporting seven logical CPUs.
Android lint and browser qualification jobs shared the host. Each workload has
512 saved rules on eight resources (64 each), split between two agents, with one
instance and either short channel limits or eight scopes of 100 long values.
The fixture writes 521 records. The script verifies that both versions return the
same page and counts, measures the fixture write and encoded/database bytes, and
records one fresh-connection read and five warm reads with wall and process CPU
time. Fresh connections retain the OS file cache; this is not a disk-cache flush.

The page budget is 32 items and 4,000,000 bytes, with 256 bytes reserved
for the envelope, matching native `paging::MAX_BYTES` (`MAX_BODY / 2`). The
record captures both the returned item count and response bytes, the continuation
cursor and hashes of the harness and native bound definitions. An earlier
measurement incorrectly used 4,194,304 bytes in both compared variants; its
reference/candidate equality check missed that native-boundary discrepancy.
These refreshed results use the corrected bound; the large fixture now returns
27 rules rather than 28. The native grant enforcement itself is unchanged.

| Large-scope workload | Reference | Bounded |
| --- | ---: | ---: |
| Rules returned in the first page | 27 | 27 |
| Encoded page response bytes | 3,945,654 | 3,945,654 |
| Rows decoded for one directory page | 512 | 33 |
| Encoded rule bytes returned from SQLite | 74,778,624 | 4,819,716 |
| Metadata lookups for that page | 512 | 66 |
| Warm page wall time, median | 571.7 ms | 466.7 ms |
| Warm page CPU time, median | 571.7 ms | 460.0 ms |
| Rows decoded for catalogue counts | 512 | 2 |
| Encoded count-query result bytes | 74,778,624 | 124 |
| Warm counts wall time, median | 519.6 ms | 161.6 ms |
| Warm counts CPU time, median | 432.3 ms | 158.5 ms |

These are local Python SQLite/JSON boundary measurements, not Rust HTTP latency,
native memory measurements or production network latency. The directory still
scans stored JSON in SQLite to select the agent, and catalogue aggregation still
visits all rules. The correction bounds rule materialization and metadata reads;
it does not claim constant-time indexed lookup. Recorded write times describe
synthetic fixture seeding; the production write path and encoded record format
are unchanged.

Native HTTP regression tests separately check record-bound pagination, byte-bound
pagination of large saved rules without gaps, agent isolation, exact revocation,
role checks, and discoverability after a package becomes unavailable.

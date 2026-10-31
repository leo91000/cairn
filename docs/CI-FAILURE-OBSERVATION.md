# Cold Rust CI preparation and native admission observations

## Cold shared Rust builds

The shared build, Clippy, backend and node-qualification jobs compile Rust without
restoring or publishing a Cargo target-tree cache. There is no v3 or legacy
`target/` bootstrap and no optional warm mode. Existing shared caches are left
untouched; the workflow simply no longer reads or writes them.

This removes the optional large-archive download/extraction contribution to a
reader's disk use. It does **not** guarantee that the runner cannot run out of
space: compilation, package installation and unrelated writers still allocate
resources. The historical ENOSPC runner/archive is not replayed or claimed fixed.
Measured cold jobs completed within their existing deadlines on one earlier
source/image; this is feasibility evidence, not a paired warm/cold comparison or a
performance guarantee for later source. Deadlines and concurrency are unchanged.

Small registry/Git reuse may be considered separately with its own resource
contract. This patch implements no new cache format, quota, threshold or pruning.
The exact native-reference **binary** cache remains separate: its immutable key
binds the reference commit, image, Rust version, profile and features; embedded
metadata and binary checksums are verified before use. Same-run build artifacts,
container-image caching and the specialized Firecracker qualification are distinct
from the removed shared target cache.

## Read-only build capacity observations

The four shared Rust jobs upload small capacity observations before and after
build/test work. The early upload can survive a later worker failure. Fixed
filesystem locations report available bytes and inodes; fixed Rust directories
report allocated bytes, apparent bytes and inode counts. Unknown measurements are
reported as unavailable, never zero or success.

Each observation has one total 15-second child-process budget. It emits neither
filenames nor arbitrary environment/error text. Image, source and run identifiers
use explicit technical-field validation. The command refuses non-CI invocation;
offline tests use injected synthetic filesystem/child-process dependencies rather
than probing the shared development host. Observation failures do not replace the
job's outcome, and missing artifacts mean missing evidence.

Directory aggregates are not a hard-link-aware combined allocation bound, exact
archive contents, or peak disk-use measurement. They do not reconstruct the
historical failed runner's capacity. Incremental output is measured separately;
compilation still runs with its existing incremental/debug settings.

## Fixture preparation without unnecessary package acquisition

Browser shards use the test-only `beacon_sql_fixture` example built with their
existing backend artifact instead of installing `psql` at runtime. The synchronous
TypeScript helper sends its connection URL and raw SQL batch on stdin. It keeps
schema/options and simple-query transaction behavior while exposing only fixed
failure codes. The example is not included in production binaries or images.
Each shard first qualifies the actual helper and artifact against disposable
Postgres 17; see [the fixture contract](../tests/e2e/README.md).

Network and Android jobs run `prepare-network-tools.mjs` before their existing
qualification. Its CLI refuses invocation outside GitHub CI. It checks actual
executable tools under the privileged PATH,
including cleanup's external `kill`, and records bounded numeric versions,
iptables backend and the noninteractive sudo/namespace-list checks. Complete
hosts make no APT calls. Missing tools select only packages from the fixed map;
installation errors and unsuccessful final probes stop preparation. Before,
APT-stage and ready records are uploaded with the existing evidence artifacts.
The tool-version probes have a bounded ten-second read; qualification and package
installation keep the existing job deadlines without added retries.

All network groups still require `conntrack`: their unchanged shared test suite
uses its real CLI even when selected NAT scenarios do not. Android's selected
scenarios do not require that CLI. The checks do not claim to prove namespace
creation, packet routing or cleanup; the subsequent real qualification does that.

This avoids a needless package-manager dependency on already complete hosts. It
does not repair the mirror or identify the silent historical APT subphase. Missing
packages still need acquisition, and network jobs retain Playwright's separate
`install --with-deps chromium` path. The workflow is not universally APT-free.

## Native admission diagnostics

The complete reference and candidate qualifications retain their original order,
unchanged binaries and exact archived protocol fixtures. Each temporary fixture
receives the same two observation statements after the API has read a failing
HTTP response. Removing those statements must restore the complete original AST
and exact original text. An incompatible or repeated overlay is refused. Actual
fixture hashes are logged to prove provenance.

Only `POST /runs/{UUID}` with HTTP 409 can emit an observation. A response of at most
4096 bytes containing exactly one `error` string is compared to fixed
source-defined literals. Output contains only reference/candidate role, route
label, synthetic attempt UUID, status, fixed reason code and response length.
Unknown, oversized or malformed data emits `unrecognized`, never raw response,
request, header, plan or environment data. A closed output stream cannot replace
the original HTTP exception. No request, assertion, phase, Docker flag, retry or
policy is changed. The existing CI reference selector supplies the actual base
commit; no historical reference is imposed permanently.

Guard tests intentionally produce some 409 responses. Their presence alone does
not establish a failed admission. A cause must be correlated with an unhandled
request and failing phase. A passing observation of the historical fixture does
not prove a repair; non-reproduction is recorded as such. Local API/privacy tests
and full native qualification remain separate evidence.

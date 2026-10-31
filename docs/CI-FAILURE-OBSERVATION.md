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

## Native admission diagnostics

The complete reference and candidate qualifications retain their original order,
unchanged binaries and exact archived protocol fixtures. Each temporary fixture
receives the same two observation statements after the API has read a failing
HTTP response. Removing those statements must restore the complete original AST
and exact original text. An incompatible or repeated overlay is refused. Actual
fixture hashes are logged to prove provenance.

Only `POST /runs/{UUID}` with HTTP409 can emit an observation. A response of at most
4096 bytes containing exactly one `error` string is compared to fixed
source-defined literals. Output contains only reference/candidate role, route
label, synthetic attempt UUID, status, fixed reason code and response length.
Unknown, oversized or malformed data emits `unrecognized`, never raw response,
request, header, plan or environment data. A closed output stream cannot replace
the original HTTP exception. No request, assertion, phase, Docker flag, retry or
policy is changed. The existing CI reference selector supplies the actual base
commit; no historical reference is imposed permanently.

Guard tests intentionally produce some409 responses. Their presence alone does
not establish a failed admission. A cause must be correlated with an unhandled
request and failing phase. A passing observation of the historical fixture does
not prove a repair; non-reproduction is recorded as such. Local API/privacy tests
and full native qualification remain separate evidence.

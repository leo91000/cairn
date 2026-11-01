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

[PR #662](https://github.com/leo91000/cairn/pull/662) moved compiled-cache
publication from the backend to the fixture build, keeping its save outside the
backend qualification deadline. A fresh BUILD is materially smaller: on the same
cold source and image in [run 41265275077](https://github.com/leo91000/cairn/actions/runs/41265275077),
`target/debug` occupied 3,793,760,256 allocated bytes (3,784,102,154 apparent;
6,992 inodes), versus 23,023,460,352 bytes for BACKEND (23,163,191,090 apparent;
9,603 inodes). This does not show that the BUILD writer itself is unsafe.
However, the unchanged v3/legacy restoration chain still admitted earlier backend
archives; the observed main cache object (ID 9817498082) was 4,601,796,718
compressed bytes, with its expanded contents and peak unmeasured. No historical archive was replayed and
no new-writer ENOSPC was established. The cold policy preserves #662's deadline
goal by eliminating the backend save altogether.

A separate performance follow-up can retain a BUILD-only publisher under a fresh
cache generation with bounded publication and a qualified warm restore, excluding
all v3 and legacy restoration paths. It must measure the actual published and
restored data before selecting a resource bound. That contract is not implemented
here; the cold measurements neither prohibit future caching nor guarantee its
size or speed. Registry/Git reuse can also be considered separately with its own
resource contract. This patch implements no new cache format, quota, threshold
or pruning.
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

Network and Android jobs run `prepare-network-tools.mjs` before qualification.
Its CLI refuses invocation outside GitHub CI. The supported host is Ubuntu 24.04
amd64 with the six core tools already installed: `ip`, `tc`, `iptables`,
`setpriv`, `sysctl` and external `kill`. Actual privileged PATH/version probes,
iptables backend, noninteractive sudo and namespace-list checks remain required.
An unexpected missing core tool fails closed instead of refreshing all configured
APT repositories. All network groups additionally require `conntrack`: their
unchanged shared suite uses its real CLI even for selected NAT scenarios. Android
does not require it. Complete hosts make no package installation calls.

For missing conntrack, [the fixed manifest](../scripts/ci-network-packages.json)
contains four original Ubuntu Noble amd64 packages, totaling 110,156 bytes.
The dependency closure is conntrack, libmnl, libnetfilter-conntrack and
libnfnetlink; libc and its loader remain host prerequisites, never replaced.
Readelf inspection found only those SONAMEs and glibc symbols through GLIBC_2.34.
The conntrack package has no maintainer scripts; the libraries only declare
standard ldconfig triggers, which normal dpkg installation preserves.

The manifest hashes were verified through Ubuntu's signed Noble InRelease and
Packages index using the documented archive signing key fingerprint
`F6ECB3762474EDA9D21B7022871920D1991BC93C`. The
[archive verification contract](https://documentation.ubuntu.com/security/software-integrity/archive-verification/)
and [documented signing key](https://ubuntu.com/chisel/docs/v1.3.0/reference/security/)
explain that provenance. `ci-network-package-closure.mjs acquire` downloads each
fixed official Snapshot object once in the existing build job, checks its size
and SHA256, then uploads a one-day same-workflow artifact. Network consumers
verify that artifact against the committed manifest, including original package
control identity, and use only local `dpkg --install` paths. Already configured
libraries satisfying the dependency versions remain untouched; incompatible or
unconfigured packages, wrong ABI, corrupted/missing archives or failed post-install
probes stop the job. No solver, mirror fallback, downgrade, libc replacement or
network acquisition runs in the consumer. Real loader verification and the
unchanged common netlink tests qualify the executable on the disposable host.

Refresh is deliberate source maintenance: verify the new Ubuntu InRelease
signature and Packages digest, then exact package versions, hashes, dependency
and ELF closure, scripts, licenses and supported host ABI before reviewing the
manifest change. Check all same-workflow consumers on the new pinned objects;
never silently refresh or fall back during qualification. The
[Snapshot service](https://snapshot.ubuntu.com/) intends at least two years of
availability, so the dated manifest also requires maintenance before aging out.

The observed historical 876-second wait was in broad APT metadata refresh;
its internal acquire-versus-lock cause remains unknown. This removes that broad
prerequisite and package acquisition from network qualification consumers, but
four finite HTTPS requests remain in build. Source outages still fail build.
Unrelated libclang APT and Playwright's `install --with-deps chromium` remain;
the whole workflow is not APT-free. No retry, concurrency or job deadline changes
were made. Phase/version/package evidence stays bounded and accompanies the
existing qualification artifacts. Probes alone do not prove routing or cleanup;
the subsequent real qualification remains responsible for those behaviors.

Diagram geometry assertions resolve the persistent app root and look up current
SVG descendants inside one synchronous callback. Font loading can legitimately
replace an SVG between a browser locator's resolution and evaluation; detached
frames have zero screen bounds. A real-font transition regression acquires the
target before redraw and keeps the exact two-frame, strict ordering assertion.
This changes test measurement lifetime, without changing renderer behavior,
timeouts or test retries.

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

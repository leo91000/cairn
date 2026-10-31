# Observe native admission and Rust cache failures

This change collects evidence for two unresolved CI failures. It changes no
installation behavior, admission decision, cache key/path/publication policy,
request sequence, retry, timeout or qualification assertion.

## Immutable comparison

The diagnostic branch deliberately starts at historical main
`1bf0a0551a23bccaf0724394091cb5f49edb74b4`, where reference admission previously
returned HTTP 409. Remote main had subsequently advanced to `ecf323e4` (SQLite
observation). One diagnostic commit retains `1bf0a055` as its first parent, which
the existing manual-workflow reference selector uses. The Rust product source is
identical in reference and candidate; this run does not qualify PR #649, PR #651
or later main. Before any durable integration, update the base and repeat the
appropriate independent review and exact-head checks.

## Native admission

The existing reference and candidate qualification run in their original order,
against their unchanged binaries and exact archived protocol fixtures. Each
temporary fixture receives the same two observation statements after the API has
read a failing HTTP response. The overlay verifies that removing those statements
restores the complete original AST and exact original text. An unexpected shape
or repeated overlay is refused. The log records both fixture hashes.

Only `POST /runs/{UUID}` with HTTP 409 can produce an observation. A response of
at most 4096 bytes containing exactly one `error` string is compared to fixed
source-defined literals. Output contains only reference/candidate role, route
label, synthetic attempt UUID, status, fixed reason code and response length.
Unknown, oversized or malformed data produces `unrecognized`, never raw payload,
request, header, plan or environment data. A closed output stream must not replace
the original HTTP exception. All existing assertions, phases and Docker flags
remain in place; no new requests or retry delays are introduced.

## Rust cache capacity

The cache action records filesystem bytes and inodes before restore, and uploads
that small artifact **before** invoking any cache restore. A runner that dies
during restore can therefore leave the initial capacity evidence. Further
artifacts record after-restore directory aggregates and the backend writer's
expanded directories before post-save. Diagnostic failures do not change the
cache action's outcome or suppress its existing failure; missing artifacts mean
missing evidence.

Each observation measures only fixed cache directories with one total 15-second
child-process budget. It reports allocated bytes, apparent bytes and inodes,
including the incremental directory separately because the current cache excludes
it. It reports neither filenames nor arbitrary environment/error text. Image,
source and run identifiers use explicit technical-field validation. The command
refuses non-CI invocation; offline tests use injected synthetic filesystem and
child-process dependencies instead of probing the shared development host.

Directory aggregates do not directly measure archive contents, hard-link-aware
combined size, extraction peak, or the original failed runner's missing capacity.
The writer observation precedes action cleanup/post steps; it is not proof a cache
was saved. The original failing archive is no longer visible in the repository's
cache inventory. A new successful restore is useful qualification, not proof of a
causal cache repair. No cache is removed, pruned or assigned a new key by these
observations.

## Validation and interpretation

Offline tests compare the actual fixture API before and after insertion, including
privacy boundaries, HTTP exceptions and output failure, plus strict overlay
rejection. Node tests cover synthetic bytes/inodes, invalid/error measurements,
the single total budget and refusal to probe outside CI. Workflow contract tests
check evidence ordering and unchanged cache/native qualification contracts.

The complete disposable native job remains necessary: a shortened case has not
been shown to preserve the history preceding the 409. A failure remains a failure;
non-reproduction remains non-reproduction. These observations establish no
production cause, recovery or speedup. A confirmed independent product defect
must be handed to a different implementer before its own review and validation.

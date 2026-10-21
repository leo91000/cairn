# CI performance

The target is a five-minute feedback loop. Measure elapsed time from workflow
creation to completion, including dependency waits and runner queues. Runner
minutes are a separate cost metric: parallel jobs shorten elapsed time without
necessarily reducing compute consumption.

## Reference before parallelization

On 2026-10-21, successful PR runs of `Quality and container` took 28m35s,
30m34s and 32m57s. The most recent main run took 23m25s despite reusing validated
images. The last PR's `quality` job took 28m46s, the installation image job
27m43s, and the network job 11m34s. Android took 12m11s and the storage
compression reference 17m52s in separate workflows.

Reference run: https://github.com/leo91000/cairn/actions/runs/39551334642

```sh
node scripts/ci-timings.mjs 39551334642 --budget=300
```

The existing timing script exits unsuccessfully when the completed workflow
exceeds the budget. Replace the run ID to compare the new PR and main runs.
Compare cold cache initialization separately from subsequent warm runs.

## Execution and coverage

- Web checks and generated schemas, Clippy, backend tests, and node/S3 recovery
  run independently. Image validation still requires every group to succeed.
- Backend tests use pinned cargo-nextest with four concurrent tests across
  binaries. The existing launcher strips live agent credentials and builds the
  code worker with the same feature flags. Normal local `pnpm test:backend`
  continues to use cargo test. Doctests run separately because nextest does not
  execute them.
- Network topologies run on five separate runners. Namespaces and firewall rules
  remain isolated by host; all twelve scenarios run exactly once.
- Browser journeys and WebKit checks each use four shards with two workers per
  shard. Chromium layout coverage is unchanged.
- The installation image starts immediately after release resolution. Before
  smoke testing it waits for this workflow run's fixture build and fails if that
  build terminates without its artifacts. Image publication requires that build
  to succeed as well as the image's own validation.
- Android unit tests and lint run independently, and four device shards retain
  the existing PR annotations and full main/tag test selection.
- Storage measurements keep the production release profile and workload. Their
  package-specific invocations avoid building the unrelated MCP code worker.

## Cache constraints

The repository's GitHub Actions cache limit was 10 GiB and was already occupied
before these changes. Only the backend job saves the shared compiled Rust cache;
the other Rust jobs restore it. The key changes with the commit, with fallback
to compatible earlier builds. The Rust cache action removes unused dependency
and incremental artifacts and omits extracted registry sources. Workspace
artifacts are retained to reuse unchanged code. Cache ownership avoids storing
four copies of the same dependency graph.

PR caches remain scoped to their PR; other PRs reuse main's cache. A first run
with the new cache format must compile dependencies. That initialization is not
evidence of warm-run performance. Storage release caches also refresh after
source changes rather than staying fixed until Cargo.lock changes.

## Remaining measurements

Do not infer a five-minute result from the workflow graph. Record actual runs
after merge. Release linking, image/root-filesystem export, APK preparation,
device rendering, individual slow tests and runner concurrency can still define
the critical path. More parallel jobs can also increase queue time and runner
minutes; report those costs alongside elapsed time.

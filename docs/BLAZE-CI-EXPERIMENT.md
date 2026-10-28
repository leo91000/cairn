# Blaze CI experiment

Investigated on 2026-10-27. Blaze is worth measuring for repeated builds in a live
compiler process. Its advertised edit latency does not establish a gain for a fresh
GitHub Actions runner, and the experiment does not replace Cairn's Rust checks or
release builds.

## Upstream version

The available Linux binary predates the Blaze rename. This experiment uses
`cargo ra-jit`, the release's actual command, with these fixed inputs:

| Input | Pin |
| --- | --- |
| Release tag source reference | [`8b15e4b406f010f98eb456ec867e16e0a9ebb0d5`](https://github.com/indexable-inc/blaze/tree/8b15e4b406f010f98eb456ec867e16e0a9ebb0d5) |
| Release asset | [`ra-jit-x86_64-unknown-linux-gnu.tar.gz`, asset ID `659530386`](https://api.github.com/repos/indexable-inc/blaze/releases/assets/659530386) |
| Archive SHA-256 | `c5202bfb3c29920831149db9b7060c58e9876972aba3cbcd73e9e4ccaf818330` |
| Compiler | Rust `1.99.0` with `rust-src` |

The asset was published on 2026-10-20 under the mutable `nightly` tag. The workflow
downloads that tag's archive and checks its fixed digest, failing if upstream
replaces it. The asset ID above identifies the inspected bytes. Preserve all
executables together: the wrapper needs `ra-jit`
and `ra-jit-rustc` beside `cargo-ra-jit`.
[Release metadata](https://api.github.com/repos/indexable-inc/blaze/releases/tags/nightly),
[released installation and commands](https://github.com/indexable-inc/blaze/blob/8b15e4b406f010f98eb456ec867e16e0a9ebb0d5/docs/cargo.md#install).

The release requires exactly Rust 1.99.0 and the standard library source; using
Cairn's pinned Rust 1.97.1 would cause fallback. This follows the actual
[toolchain check](https://github.com/indexable-inc/blaze/blob/8b15e4b406f010f98eb456ec867e16e0a9ebb0d5/crates/ra-jit/src/cargo/state.rs#L883),
not today's installer defaults.

A local smoke run reported daemon identity
`0.0.0-4513540-18e04244584b5a00`. The trailing fields are hexadecimal executable
length and modification time, **not a source commit**. They can change when the
file metadata changes. Use the archive digest for binary reproducibility and keep
the release tag's source reference distinct from the runtime identity.
[Daemon identity implementation](https://github.com/indexable-inc/blaze/blob/8b15e4b406f010f98eb456ec867e16e0a9ebb0d5/crates/ra-jit/src/cargo/daemon.rs#L601).

Upstream has changed substantially since this binary. Main at
[`855f6159139b63f0abc384f3c30b04c15396eda2`](https://github.com/indexable-inc/blaze/tree/855f6159139b63f0abc384f3c30b04c15396eda2)
uses its own `blaze-2026-10-27` toolchain and describes compiling whole programs
without Cargo/rustc fallback. Those capabilities are outside this experiment.
[Current toolchain](https://github.com/indexable-inc/blaze/blob/855f6159139b63f0abc384f3c30b04c15396eda2/docs/toolchain.md),
[current build behavior](https://github.com/indexable-inc/blaze/blob/855f6159139b63f0abc384f3c30b04c15396eda2/docs/cargo.md#what-a-build-does).

## What the workflow measures

`.github/workflows/blaze-benchmark.yaml` runs
`scripts/benchmark-blaze.py` independently of the existing CI. It compares three
arms sequentially on the same runner:

1. Cargo on Cairn's Rust 1.97.1.
2. Cargo on Rust 1.99.0, to separate compiler-version effects.
3. `cargo ra-jit` on Rust 1.99.0, to compare the wrapper with the same compiler.

All arms build the real `cairn` binary with the same locked dependencies,
`CARGO_PROFILE_DEV_DEBUG=0` and `CARGO_INCREMENTAL=0`, matching the current debug CI
settings. Each has an isolated target directory. Registry downloads happen before
the measurements. Installation and dependency download costs remain separate from
build latency; no persistent target cache is restored or saved.

Each arm records an initial build, an unchanged warm build and three function-body
edits. The edit adds a unique suffix to the existing `--version` output in
`crates/installation/src/main.rs`. Executing the rebuilt binary must return the
expected new value each time, so a stale executable cannot count as a fast build.
The source is restored after the experiment.

For ra-jit, background loading after the first build is timed separately and must
finish before warm measurements start. A further edit after stopping the daemon,
with the target directory retained, measures restart behavior. This approximates
a fresh process with already available build files. It excludes cache transfer,
checkout and runner startup costs, so it is an optimistic reference for an
otherwise fresh CI runner.

The first hosted run was still loading after three minutes, so the loading budget
is fifteen minutes. During loading, the experiment records the daemon and worker's
process state, cumulative CPU time and RSS every thirty seconds to distinguish
ongoing computation from a stopped or stuck worker. These samples are diagnostics,
not total-engine resource measurements. A timeout is inconclusive for warm-build
speed; it is not reported as an engine speedup.

The workflow retains machine-readable measurements, command logs, daemon status
and a readable comparison in its Actions artifacts and job summary. Wall time is
measured per build. GNU time CPU and peak RSS describe the foreground command and
its children; a detached compiler daemon is excluded, so those values are not
whole-system resource comparisons. Host CPU, memory, OS and tool versions are
recorded alongside the workload.

## Reading the results

The released first build runs Cargo and starts background loading without waiting.
Later invocations can still fall back to Cargo. `[ra-jit]` in the completion line
means the daemon served the build. Status also reports served/fallback counts and
reasons. Keep this evidence with timings and distinguish fallback timings from
compiler-engine gains.
[Released build and fallback behavior](https://github.com/indexable-inc/blaze/blob/8b15e4b406f010f98eb456ec867e16e0a9ebb0d5/docs/cargo.md#what-a-build-does),
[status implementation](https://github.com/indexable-inc/blaze/blob/8b15e4b406f010f98eb456ec867e16e0a9ebb0d5/crates/ra-jit/src/cargo/mod.rs#L363).

A warm edit speedup answers whether a persistent daemon could help this binary.
The initial and restarted-process results address the more immediate CI question.
They do not measure Clippy, tests, release optimization, the whole multi-binary CI,
or a cache shared across workflow runs. Repeat runs to assess runner noise before
adopting a build strategy.

Upstream's warm-VM CI runner document is explicitly a design with nothing built
as of its writing. Its proposal preserves the live daemon through VM snapshots;
a disk cache alone does not preserve a running compiler's memory.
[CI runner design at the inspected main commit](https://github.com/indexable-inc/blaze/blob/855f6159139b63f0abc384f3c30b04c15396eda2/docs/ci-runner.md).

The pinned release also documents semantic gaps, including the absence of a borrow
checker. Its experimental binaries must not become release or production artifacts.
Cairn's existing toolchain, Clippy and backend tests remain the correctness gates.
[Released known problems](https://github.com/indexable-inc/blaze/blob/8b15e4b406f010f98eb456ec867e16e0a9ebb0d5/docs/cargo.md#known-problems).

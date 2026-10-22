# Rust release build performance

Cairn keeps the pinned stable Rust compiler and ThinLTO. Its workspace packages
use four release codegen units and cap LLVM's normal, hinted and locally hot
inlining thresholds at 75. The global release profile stays at one codegen unit
so third-party dependencies retain their existing settings.

The named package overrides in `Cargo.toml` select the workspace codegen units.
When adding a workspace package, add its release override too. Cargo's workspace
compiler wrapper applies the inlining defaults only when the effective compiler
arguments select optimization level 3 and four codegen units. Development builds,
compiler queries and build scripts at optimization level 0 pass through.

The wrapper needs only POSIX `sh`, which is available in the Rust-only Docker
build stages. Those stages copy it after compiling the dependency layer. Both
installation and Beacon release builds use the same configuration.

Explicit `-C llvm-args=...` opts out of the wrapper's inlining defaults. Compiler
response files (`@path`) also pass through, since they can override those options.
To disable the wrapper entirely for a diagnostic build, set `RUSTC_WORKSPACE_WRAPPER=`.
Keep its pathname versioned: changing compiler arguments requires a new wrapper
filename and updates to `.cargo/config.toml` and both Docker copies, so Cargo
cannot reuse workspace artifacts compiled with an older wrapper policy.

## Measured workload

The initial search used Cairn commit
`7e549399d4da047cd77440bb02ac23999bb2d906`, Rust 1.97.1 / LLVM 22.1.6,
O3, ThinLTO and `--features ublk`. Complete application rebuilds ran on the same
four physical P cores of an i9-14900K host with cached third-party dependencies.
Other work shared the host's memory and power budget. Controls and candidates
were interleaved; each row below has three successful builds.

| Workspace configuration | Median build | Executable size |
| --- | ---: | ---: |
| 16 codegen units, default inlining | 170.04 s | 94,822,408 bytes |
| 4 codegen units, inline thresholds 75 | 122.36 s | 77,022,800 bytes |

This is a 28.04% reduction against the intermediate 16-unit control. The original
repository profile had one codegen unit; its single screening observation was
287.56 s and is not part of this repeated comparison. These are local warm
dependency rebuilds, not cold builds or total GitHub Actions durations.

The research harness also set optimization level 3 on workspace build scripts.
The checked-in configuration preserves their existing optimization level. The
production integration therefore needs its own checks in addition to the
initial search results.

Both measured configurations passed 31 focused release integration tests
(durability, HTTP, notifications and TLS) and the native codec fixture. Three
additional codec processes per configuration preserved the input hashes,
encoded bytes and round trips. Across the shipped v2 codec workloads, observed
write/read CPU median changes were between -1.7% and +2.7%; wall changes were
approximately within 4%. These fixtures do not establish equivalent performance
for every application workload or production storage/network latency.

## Reproducing the release build

```sh
pnpm install --frozen-lockfile
cargo build --locked --release --bin cairn --features ublk --timings
```

For repeated builds, preserve the dependency cache and invalidate the workspace
source roots between observations. Use the same CPU affinity, toolchain and
feature set for both configurations. Record the source commit, effective
compiler arguments, wall time and executable hash/size. Keep warmups separate
and compare repeated controls, rather than selecting the fastest isolated run.

Use `scripts/test-backend.mjs` for release tests to isolate live agent credentials.
The durability scheduler comparison also needs the pinned Node dependencies.

## LLVM source experiment

The GlobalOpt cold-call-scan patch remains an archived research candidate. Its
direct pass replay improved by 16.09%, but a complete expanded prelink pipeline
improved by only 0.57% across three observations per variant. Complete Cairn builds
using the same source-built Rust frontend and each native LLVM variant had medians
of 248.795 s and 248.410 s across three builds each: a 0.15% difference, with
identical executable bytes. These measurements do not justify adopting a custom
LLVM/Rust compiler for Cairn. The release configuration uses the standard pinned
toolchain.

Cargo's [package profile overrides](https://doc.rust-lang.org/cargo/reference/profiles.html#overrides)
and [workspace wrapper configuration](https://doc.rust-lang.org/cargo/reference/config.html#buildrustc-workspace-wrapper)
define the scope of these settings.

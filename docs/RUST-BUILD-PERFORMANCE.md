# Rust release build performance

The custom wrapper described below has been retired; release builds now use
the standard ThinLTO profile with one codegen unit and stripped debug info.

The historical experiment kept the pinned stable Rust compiler and ThinLTO. Its workspace packages
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
filename and updates to its Cargo configuration and both Docker copies, so Cargo
cannot reuse workspace artifacts compiled with an older wrapper policy.

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

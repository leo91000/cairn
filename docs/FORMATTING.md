# Formatting and readability

Formatting runs locally and in GitHub Actions. Checks fail on differences; CI
does not push formatting commits or require a bot token.

## Setup and commands

Install the repository's pinned Node/pnpm and stable Rust toolchain, then run:

```sh
pnpm install --frozen-lockfile
```

`scripts/rust-format.mjs` runs the stable rustfmt pinned in `rust-toolchain.toml`,
then `scripts/rust-spacing.mjs` separates declarations using the Rust syntax tree.
Comments and attributes stay attached to their declarations, and compact groups
of imports, module declarations, and single-line constants stay together. It
does not edit function statements or macro bodies. The parser dependencies are
pinned in `package.json` and `pnpm-lock.yaml`.

The unstable rustfmt option `blank_lines_lower_bound = 1` is deliberately unused:
it also adds spacing inside blocks and between attributes and their declarations.

| Scope | Apply fixes | Check only |
| --- | --- | --- |
| Rust + web + scripts/tests | `pnpm lint:fix` | `pnpm lint` |
| Rust only | `pnpm format:rust` | `node scripts/rust-format.mjs --check` |
| Kotlin + Gradle Kotlin scripts | `pnpm format:android` | `pnpm lint:android` |

Android formatting uses Spotless and ktfmt, pinned in `apps/android/build.gradle.kts`.
Use the JDK from `apps/android/mise.toml` and the Gradle wrapper. With mise, run
`mise exec -- ./gradlew spotlessApply --no-daemon` from `apps/android/`.

ESLint adds blank lines around functions and types, expands larger objects,
limits statements per line, and splits Vue tags with many attributes. These
rules extend the existing Antfu configuration. `.editorconfig` supplies basic
indentation and newline settings to editors.

Vendored third-party sources under `vendor/` retain their upstream formatting;
ESLint and the Rust declaration-spacing pass exclude them. Vendored packages are
also excluded from the Cargo workspace so `cargo fmt --all` formats Cairn's code.
Local patches keep the upstream style; regression tests use Cairn's conventions.

## Commit and CI checks

The existing pre-commit hook runs `pnpm lint:fix`. When staged Kotlin or Gradle
Kotlin files change, it also runs `pnpm format:android`. If fixes change tracked
files, the commit stops so you can review and stage the changes. The hook never
stages changes for you. It then checks Rust compilation and Clippy.

The quality workflow runs the same lint and Rust formatting checks through
`pnpm check`. The Android workflow runs `spotlessCheck` before its existing
tests, lint, builds, and device checks. Both remain publication prerequisites.

## What still needs review

Formatters do not decide where a function changes logical steps or whether an
expression needs a descriptive name. Add that spacing when editing code.
In particular, inspect `json!` and `tokio::select!` manually: their macro syntax
can prevent rustfmt from formatting their contents. Keep literals, evaluation
order, and control flow unchanged during a formatting pass.

Keep formatting changes in dedicated commits where practical. Review whitespace
changes separately from refactors, and use existing tests to check behavior.

## Release builds

The standard release profile uses ThinLTO, one codegen unit and stripped debug
information. Build with the pinned tools:

```sh
cargo build --locked --release --bin cairn --features ublk --timings
```

Compare the same source, toolchain, features and CPU conditions, and keep cold
and warm cache observations separate.

## Native test coverage

Native process recovery is covered by `crates/installation/tests/worker.rs`;
runner fencing is tested in `crates/installation/src/runner/broker.rs`.
`tests/e2e/task-focus.spec.ts` checks browser reconnection and manual resume.
See the [native browser fixture guide](../tests/e2e/README.md) for isolated setup.

## Android test readiness

Compose journeys wait for the control they use: an enabled editable field before
typing, and the expected pager page count before swiping. Fixtures unmount the
composition and clear their owned ViewModelStore before closing the server.
The device launcher waits for stable boot/configuration, system processes and a
quiet CPU window before instrumentation; inspect its `readiness.json` on failure.
See [Android validation](../apps/android/VALIDATION.md) for device commands.

## CI evidence

Compare completed workflow and job durations from their GitHub Actions timestamps.
Elapsed time includes queues and dependency waits; aggregate runner time measures
a different cost. Separate cold cache initialization from warm runs and record
both before claiming an improvement. PR caches cannot populate another PR’s cache;
only the backend job saves the shared compiled Rust cache.

Image reuse requires successful validation of the exact Git tree, paired Beacon
and installation digests, matching freshly resolved CLI versions, served engine
releases matching what production serves, and schema-3 provenance from this
repository. Fork and manual runs cannot provide release
proof. Missing or stale evidence requires full validation. See the
[release runbook](PRODUCTION-CAIRN.md) and
[Android publication guards](../apps/android/docs/UPDATES.md#fast-tag-publication).

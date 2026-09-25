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

Android formatting uses Spotless and ktfmt, pinned in `android/build.gradle.kts`.
Use the JDK from `android/mise.toml` and the Gradle wrapper. With mise, run
`mise exec -- ./gradlew spotlessApply --no-daemon` from `android/`.

ESLint adds blank lines around functions and types, expands larger objects,
limits statements per line, and splits Vue tags with many attributes. These
rules extend the existing Antfu configuration. `.editorconfig` supplies basic
indentation and newline settings to editors.

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

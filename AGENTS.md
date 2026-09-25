# Working in this repository

- Keep changes focused on the requested behavior. Preserve existing contracts and
  use the tests around the affected code to check them.
- Separate logical steps with blank lines. Give complex expressions descriptive
  intermediate names when that makes the flow easier to follow. Avoid arbitrary
  function-size limits or extracting helpers solely to reduce line counts.
- Format `json!` and `tokio::select!` bodies explicitly: rustfmt may leave them
  untouched. Keep object fields and select branches easy to scan.
- Run `pnpm lint:fix` for Rust and web code; run `pnpm format:android` for Kotlin.
  See [formatting setup](docs/FORMATTING.md) for the pinned tools.
- Validate Rust/web changes with `pnpm check`, `pnpm test:backend`, and
  `cargo clippy --locked --workspace --all-targets -- -D warnings` as applicable.
  Use the backend test launcher to isolate tests from live agent credentials.
  For Android, run `./gradlew spotlessCheck testDebugUnitTest lintDebug` from
  `android/`; UI behavior changes also need the relevant browser or device tests.

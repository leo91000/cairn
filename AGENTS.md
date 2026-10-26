# Working in this repository

- Keep changes focused on the requested behavior. Preserve existing contracts and
  use the tests around the affected code to check them.
- For storage behavior changes, include reproducible read/write measurements in
  the PR description against a reference: encoded bytes, cold/warm read latency,
  write latency, CPU, workload and host conditions. Commit no measurement artifacts.
  See [storage measurement guidance](docs/STORAGE-COMPRESSION.md#qualification).
  Distinguish local measurements from production network latency.
- Keep the web and Android apps at feature parity: a user-facing feature added or changed in
  `apps/web` ships in `apps/android` in the same change, and vice versa. Use native
  presentation, and record the feature and any platform difference in
  [web parity](apps/android/docs/WEB-PARITY.md).
- Every capability available through the web or Android UI must also be available
  through the public Cairn MCP and the built-in `cairn_installation` tool for agents
  granted installation access. When adding or changing a UI capability, update
  both tool interfaces in the same change, preserve their authorization checks,
  and verify equivalent behavior across the UI, public MCP and agent tool.
  Beacon-managed Cairn account actions (sessions, sign-in methods, invitations,
  members, MCP authorizations and notification devices) are UI-only: keep web/Android
  parity, but exclude them from public MCP and `cairn_installation`. They act on a
  person's Cairn account, whose authority an installation grant does not confer.
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
  `apps/android/`; UI behavior changes also need the relevant browser or device tests.

## Agent skills

### Issue tracker

Issues and specs live in GitHub Issues for `leo91000/cairn`. See `docs/agents/issue-tracker.md`.

### Domain docs

Single-context: `CONTEXT.md` and `docs/adr/` at the repo root. See `docs/agents/domain.md`.

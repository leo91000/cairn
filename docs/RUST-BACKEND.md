# Native Rust backend

Version 0.12 replaces the Node backend with the `leo` binary. HTTP, authentication,
SQLite, scheduling, execution supervision, chat RPC, account management, MCP
client/server/OAuth, Web Push, and the Docker runner broker/client are native Rust.
Vue, shared frontend contracts and build tooling remain TypeScript. Node stays in
the agent toolkit so agents can work on JavaScript projects and use the Codex CLI.
There is no Node backend process or application `node_modules` in the runtime image.

## Runtime and storage

Axum handles HTTP on Tokio. SQLite operations run on dedicated bounded database
queues: one writer and two readers, with WAL, busy timeouts and prepared statement
caches. Filesystem and process I/O are asynchronous. Password derivation runs in a
bounded blocking pool. Event pages preserve stored JSON payload bytes instead of
allocating an object tree to serialize again. Record saves and their audit entry
commit together. The release profile uses thin LTO and one code generation unit.

The public API, schema version 4, password hashes, session cookies, OAuth grants,
encrypted credential records, chat messages and execution checkpoints retain their
existing formats. Startup does not rewrite existing agents or change their order.
Bootstrap tokens are generated privately when `SETUP_TOKEN` is absent, as before.

The old implementation is frozen under `tests/legacy/server` solely as a migration
oracle, fixture seeder and benchmark reference. It is excluded from the container.
Browser fixtures serve every request and run every worker through Rust; direct
legacy service access in tests only seeds or inspects persisted fixtures.

## Execution boundaries

The worker checkpoints a supervisor identity before allowing it to launch Codex.
An inherited control socket closes when the manager dies; the supervisor stops the
process group. Recovery checks Linux PID/start-time/boot identity before fencing an
old process, then resumes the saved thread, workspace and remaining timeout. An
integration test performs this handoff from an actual Node worker to Rust.

Isolated runs use the native Docker broker and per-attempt durable stop markers.
Only selected mounts and credentials enter the container. Real container tests
cover YOLO, read-only, workspace-write, Codex sandbox enforcement, toolkit access,
framed log streaming, cancellation and broker restart. Keepalive frames are emitted
only between complete Docker log frames. They cannot split command output.

MCP supports current stateless discovery and legacy initialization, HTTP/SSE and
stdio, session-aware legacy clients, scoped per-run gateways, PKCE, token rotation,
provider discovery, private-network policy and instance-metadata blocking. Tests
use the official JavaScript SDK and an OAuth provider across the language boundary.
Web Push subscriptions and VAPID keys retain their encrypted storage format.

## Development and verification

```sh
pnpm install --frozen-lockfile
pnpm dev
pnpm test:backend
cargo clippy --locked --workspace --all-targets -- -D warnings
node scripts/rust-format.mjs --check
pnpm check
cargo build --bin leo
pnpm test:e2e
```

Regenerate `backend/schemas/inputs.json` with `node --import tsx scripts/backend-schemas.mjs`
after changing shared input contracts. CI rejects schema drift. MCP's tool catalog
lives in `backend/schemas/mcp-tools.json`; keep it aligned with tool dispatch.

`leo serve` runs the manager; `leo runner-broker`, `runner-client`, `runner-entry`,
`supervise` and `chat` implement the execution boundary. `toolkit-env` and
`prepare-execution` are local administrative commands used by the container checks;
their output can contain private environment values and must not be logged publicly.
`LEO_HTTP_THREADS` can override the bounded HTTP executor size. It does not change
the separately configured agent concurrency (any positive integer, default 4).

## Deployment and rollback

Keep one manager per data volume. Back up the SQLite database using its online
backup facility together with the encryption key and persistent agent home; copying
only `manager.db` while WAL writes are active is insufficient.

The tag deployment updates Coolify's stored runner entrypoint to
`/usr/local/bin/leo runner-broker` while preserving volume declarations and secret
expressions. CI builds dependency layers separately, tests the exact container,
and promotes its digest only after native, frontend and browser checks pass. A tag
reuses the validated main image.

Data formats remain readable by 0.11. If rolling back across the Rust migration,
restore the runner entrypoint to `[node, --import, tsx, /app/server/runner-broker.ts]`
alongside the old image. Later Rust-to-Rust rollbacks retain the native entrypoint.
Always verify the deployed commit, both services' health and recovered conversations.

Performance measurements and methodology are in [the benchmark report](performance/RUST.md).
They measure manager overhead, not model inference, network providers or project builds.

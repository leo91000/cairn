# Native browser fixtures

Browser journeys run the real `cairn` and `cairn-beacon` binaries. Normal browser
and API actions use their authenticated HTTP routes. Disposable Postgres schemas,
homes and SQLite directories keep the journeys separate from live credentials.

Build the fixture binaries before running Playwright:

```sh
cargo build --locked --bin cairn --bin cairn-beacon --example worker_fixture --example browser_fixture --example beacon_sql_fixture
pnpm build
pnpm test:e2e
```

Set `CAIRN_BEACON_TEST_DATABASE_URL` to a disposable Postgres database.

`native-fixture.ts` invokes `target/debug/examples/browser_fixture` with JSON on
stdin and reads its JSON response. Set `CAIRN_BROWSER_FIXTURE_BINARY` to override
that path. The helper is a test-only Rust example, excluded from production
images and installation commands. It calls the current Rust Store, Vault and
Service; it contains no independent schema, encryption, scheduling or execution
implementation. CI builds, uploads and makes it executable alongside the browser
backend binaries, including the network qualification jobs.

The helper exists because presentation tests need states that the public API
cannot create: synthetic provider events, interrupted runs, expired leases and
persisted waits. Reads and writes use native Store transactions. Service actions
create valid agents, projects, skills and task snapshots without starting another
worker. `cairn` still handles every HTTP request and actual execution. Synthetic
account credentials are encrypted by the current native Vault. Do not add test
routes or fixture commands to the production server.

`executeBeaconSql` synchronously invokes the test-only Beacon example
`target/debug/examples/beacon_sql_fixture`. It sends the connection URL and raw SQL
batch on JSON stdin; neither is put in arguments or credential environment fields.
The same simple-query batch semantics preserve schema/search_path options,
stop-on-error and implicit rollback, while child failures expose only static codes.
It does not need a runtime `psql` installation. The example is built once with the
existing backend fixture artifacts and is excluded from production images.

CI qualifies the real TypeScript helper and distributed example against its
disposable Postgres 17 service before the browser shard, covering raw multi-query
batches, options, rollback, explicit commits and private failure output. Run
`pnpm exec tsx scripts/qualify-beacon-sql-fixture.ts` against a disposable database
after building the fixtures to exercise that same contract locally.

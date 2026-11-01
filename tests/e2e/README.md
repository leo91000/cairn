# Native browser fixtures

Browser journeys run the real `cairn` and `cairn-beacon` binaries. Normal browser
and API actions use their authenticated HTTP routes. Disposable Postgres schemas,
homes and SQLite directories keep the journeys separate from live credentials.

Build the fixture binaries before running Playwright:

```sh
cargo build --locked --bin cairn --bin cairn-beacon --example worker_fixture --example browser_fixture
pnpm build
pnpm test:e2e
```

Set `CAIRN_BEACON_TEST_DATABASE_URL` to a disposable Postgres database.

The relay/NAT browser scenarios write a bounded `*.phases.json` journal beside
their reports. `testPhase` identifies the last entered operation; `cleanupPhase` and
browser lifecycle events are recorded separately. An entered phase does not
prove that operation completed. These fixed labels and elapsed times survive
Playwright timeout cleanup, which can remove all Pages before transport evidence
is collected. `node --test tests/network-phase-diagnostics.test.mjs` exercises that
cleanup with a real browser and an intentionally failing child Playwright test;
the enclosing test requires the timeout failure and retained phase evidence.

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

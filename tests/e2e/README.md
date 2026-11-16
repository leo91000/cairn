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

## Rules in the native Android app

`rules.spec.ts` and `rules-editor.spec.ts` exercise the visual editor, history,
creator controls and member visibility against the real services on Chromium and
WebKit. The `journeys-rules`, `layout-webkit-rules`, `journeys-rule-editor` and
`layout-webkit-rule-editor` projects each own their installation and its production
rate limiter. Splitting the list and editor journeys, and running typed-operand and author editor cases in a
fresh Playwright process, keeps each fixture account within that limiter;
the production limits are unchanged. The opt-in
`rules-android` project drives the installed native app on a disposable Android 16
emulator at 720 × 1280, density 320. It signs in through the actual email-code flow,
edits a rule, uses its lifecycle controls and saves an integer wait. The services
execute a local test plugin and use fixture mail delivery. The UI, execution
records and captures come from the running applications. Captures go to the test
output directory.

With the normal browser binaries and disposable database ready:

```sh
CAIRN_BEACON_ORIGIN=http://localhost:4422 apps/android/gradlew -p apps/android :app:assembleDebug
adb -s emulator-5580 install -r apps/android/app/build/outputs/apk/debug/app-debug.apk
ANDROID_SERIAL=emulator-5580 CAIRN_RULES_ANDROID=1 CAIRN_RULES_ANDROID_RESET=1 pnpm test:e2e --project=rules-android --workers=1
```

The native journey pauses animations on that emulator for stable input and restores their original settings when its worker exits. Update prompts are postponed; no update is downloaded or installed. Native tap XML and coordinates are retained with the test artifacts.

Use a disposable emulator: `CAIRN_RULES_ANDROID_RESET=1` clears its Cairn app data
before this journey. Keep `ANDROID_HOME` set to the SDK. The default browser port
base is 4322, making the Beacon origin `http://localhost:4422`; an overridden port
base needs the corresponding APK origin. Component interactions remain covered
by the shared `RulesEditorCases` on Robolectric and Android instrumentation.

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

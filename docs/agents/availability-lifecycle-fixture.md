# Additional physical availability lifecycle coverage

This fixture has a deliberately narrower scope than the authenticated installation
application. It serves the **unchanged shipped Beacon build** on `/claim`, with
synthetic session/options/invitations and installation status responses. There is
one listed installation and no selected installation. The real session watcher
does not open it on the claim page; `InstallationTransport.start()` refuses to
start without an installation ID. Neither fetch nor RTC nor the caller is mocked.

`BeaconApp.vue` runs its actual `refreshAvailability`, success/failure publication,
timers, suspend/resume and session watchers. Its imports of `readAvailability` and
`document-lifetime` are unchanged. The real DOM displays the listed status.
The server serves exact `dist/beacon.html` and asset bytes, not a rewritten entry
or an extracted copy of the polling implementation.

## Synthetic policy and strict boundaries

Only this isolated HTTP fixture uses `Cache-Control: no-cache`. It permits HTTP
storage/revalidation without a `no-store` response and requires a real request for
each status refresh. These responses contain only fixed synthetic data, with no
real account authentication or secret. No Beacon/manager header, authorization,
production cache policy, transport code or runtime source changes are involved.
Unexpected APIs, methods, queries, missing assets and path escapes fail the fixture.

The test uses the existing native response gate and availability probe. It does
not use Playwright routing, dispatch synthetic lifecycle events, accelerate the
clock, or replace fetch/JSON results. Existing SDK, project profiles and assertion
deadlines remain unchanged. An eligible design is not proof of native admission.

Each engine's one case contains:

1. An inert document round trip on the same server/profile: trusted persisted
   pagehide and pageshow and the same in-memory document identity are mandatory.
2. The real Beacon claim screen: a genuinely held periodic read, strict first
   owned withdrawal, its original rejected terminal and HTTP cancellation before
   releasing body bytes; native persisted departure and return of the same
   document; then a fresh real request, exact 200 JSON and the expected DOM status.
3. A second cycle with a different expected status, proving rearming without a
   second polling implementation or altered production cadence.

A reload, persisted=false, only the inert control passing, incorrect body,
incomplete/orphan/duplicate terminal, unexpected HTTP or JS rejection, pageerror,
unhandled rejection, deadline, duplicate poll or unknown closure cannot pass.
Every owner is retired independently within the existing seven-second bound;
audits run after retirement. First workload and cleanup errors remain preserved.

## Accepted crossed evidence, not a replacement claim

The permanent source witness in `availability-publication.test.ts` calls the real
functions with a generation retired before the same TypeError and a live parent.
The controlled comparison with BeaconApp9da is RED (Offline/counter/retry) while
the candidate is GREEN. That is conditional source evidence, not proof of the
native ordering in the original Page.goto and not a guaranteed BASEbrowserRED.
The additional physical fixture supplies a different boundary. It does not
correlate native application generation and publication in that original journey.

It does **not** qualify BFCache for the authenticated installation Direct/no-store
path, real authentication, network topologies or a future source commit. Original
`availability-bfcache.spec.ts` and its strict persisted=true assertion are intact.
Its two first physical failures and all diagnostic evidence remain preserved.

## Proposed disposition of the two installation BF failures

| Case | Acquired fact | Still due; no reclassification as PASS |
| --- | --- | --- |
| Chromium authenticated Direct | Native non-admission, persisted=false; explicit partial no-store/fetch/RTC exclusions plus masked; first history return loaded another document | Original strict restoration test stays RED. Any separately admitted cache-miss/reload recovery check must require the new document, exact fresh response, DOM/Direct recovery and original terminal/closure, retain the original failure, and claim only reload recovery. |
| WebKit authenticated Direct | Native non-admission, persisted=false before goBack; no Chromium reason may be transferred | Original strict restoration test stays RED. A separately admitted reload recovery check must establish its own native history/new-document evidence, exact response and DOM/Direct recovery. It is not implemented or qualified by this fixture. |

ROOT must decide the disposition of these unresolved app-path eligibility failures.
Neither this table nor a successful additional fixture waives or repairs them.

## Commands and evidence limits

Light real HTTP/controller checks, without a browser or build:

```sh
node --test tests/e2e/availability-lifecycle-fixture.node.mjs
```

Selection only, using the official config and existing two engine projects:

```sh
pnpm test:e2e --config=playwright.config.ts --project=journeys-mcps --project=layout-webkit-mcps tests/e2e/availability-lifecycle.spec.ts --list --reporter=list
```

Expected selection is exactly two cases, one per project. It must be recollected
on the exact candidate/image; this inventory is not a claimed collection result.
After a separately authorized real build, the proposed native command is the same
selection with `--workers=1 --retries=0 --fail-on-flaky-tests --trace=on` in place
of list/reporter options. No native execution is authorized by these instructions.

The initial implementation is source-only. Current original primary evidence is
48/48 PASS on immutable b186, dependants27, saved resume4 and waits7404; it does not
transfer to this new source. ACCOUNT and OAuth401, final source identity and
separately admitted networking remain due, together with disposition of app BF
failures and exact final qualification of the original MCP fallback/pageerror need.
P1 remains open. No fixture success by itself establishes product delivery.

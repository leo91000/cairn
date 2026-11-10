# Availability navigation repair: evidence boundaries

The observable requirement is to avoid an obsolete availability read publishing
Offline, a transport fallback or another retry after its document/session/read
has retired. An active document, including one retained by a download, must still
publish genuine failures and recover on the existing cadence. The original fetch
and JSON terminals must be preserved.

The caller captures the actual document generation and session. A genuine read
failure queues one cancellable publication task. That task checks the captured
ownership before changing availability, the failure counter and retry together.
Retirement cancels that work. Publication before retirement remains a real
failure; another task does not guarantee a universal order relative to pagehide.
The read factory and its earlier TypeError are unchanged by this caller repair.

## Accepted split evidence

The committed browser regression covers the original terminal, the old document's
native lifecycle, HTTP resource closure, and a fresh document's exact availability
body and Direct recovery. The Node regression calls the real refreshAvailability,
document-lifetime and readAvailability functions to cover conditional generation
ownership and publication.

There is **no native app-generation/publication correlation observed in this
Page.goto**. The probe's document ID and pagehide are not the application's
captured generation or a publication receipt. There is **no guaranteed baseline
browser failure**: a baseline publication while its generation is still active is
allowed. The conditional source comparison below must not be reported as a native
ordering proof. These two forms of evidence cover different boundaries; neither a
partial source PASS nor a diagnostic native PASS closes the product defect.

## Minimal committed-case mapping

Only the later read whose JSON body is held through the committed navigation and
its rejection audit change criteria. The first owned withdrawal stays strict.

| Boundary | Previous expectation | Current expectation and reason |
| --- | --- | --- |
| First owned withdrawal | Trusted, exact, in-call signal; original aborted terminal; HTTP cancellation before body release | Unchanged. This withdrawal has native ownership evidence. |
| Later periodic read before commit | Real production cadence; real 200 headers and original JSON start; body and destination held | Unchanged. No replacement producer, early body delivery or new request removes the failure. |
| Later committed terminal | A new private in-call abort and body-rejected.aborted=true | Exactly one original body-rejected terminal. A proven abort still requires trusted/exact/in-call metadata and matching reason or AbortError. Otherwise the recorded event must be TypeError, aborted=false and reasonMatches=false. It remains in the journal unchanged. No manufactured abort is required after a genuine native failure. |
| Later read HTTP receipt | Closed/cancelled before releasing its held body; no completed old body | Unchanged. This receipt proves physical HTTP closure, not JS abort ownership. |
| Old and new documents | New Agents document, Direct route and settled work | Also require one trusted old-document pagehide after that read's JSON start, a distinct new document, and its real exact 200 JSON body plus Online. No application-generation inference follows from pagehide. |
| Non-aborted rejection audit | No non-aborted rejection in any read | Only the exact event already validated as the declared later committed terminal is accounted for separately. Its document/read/sequence and raw fields remain in the saved observation. Every other non-aborted rejection remains a failure. |
| Structural and error audits | Complete read phases; no orphan/duplicate/incomplete terminals; unexpected HTTP/pageerrors/unhandled are red | Unchanged. An unknown terminal, false abort metadata, another read's rejection, JSON parse error or unexpected HTTP failure is not the declared committed outcome. |

The new document's expected response is the existing authenticated reference,
installed for subsequent documents without adding a network request or changing
production timers. It is used by the existing exact-body probe.

Cancelled/download and same-document lifecycle cases, active HTTP/JSON/fetch
failures, backoff/recovery, second departure rearming, account/OAuth 401,
conditional document-return behavior, saved readers and Direct/Relay qualification remain strict.
The original MCP management case keeps its empty pageerror assertion. No new
filter, global allowance, skip, retry or timeout increase is introduced.

## Permanent conditional red/green witness

`apps/web/tests/availability-publication.test.ts` includes
`preserves the same original terminal without publishing after generation retirement while its caller remains alive`.
It starts the real refresh, delivers native Node Response headers, then retires the
actual document-lifetime generation before rejecting the same original JSON
promise with a TypeError. Its parent caller remains alive and unaborted throughout.
It asserts that exact error identity and zero Offline/counter/retry effects.

Replacing only BeaconApp.vue with immutable
`9da5f39aa9170444789708a0c5e6b2183934c303`, using the same test, read factory,
lifetime source, host and adapters, fails the invariant: Offline writes=1,
online=false, failure counter=1 and retry=6000 ms. The candidate expects and
produces 0/true/0/no task. This witness is permanent, rather than only testing a
publication task delayed until pagehide. The separate publication-before-pagehide
case still requires the genuine failure to act.

This is a conditional source regression through real functions and original
Response/JSON promises, using controlled Node I/O/task adapters. It does not prove
that native Page.goto exercises this ordering, nor replace actual Vue/browser
qualification.

## Validation map and pending qualification

| Affected behavior or caller | Required coverage |
| --- | --- |
| Beacon availability publication, session/read/generation ownership | Entire availability-publication suite, including active failures, download retention, both retirement orders, hide/show, signout/redirect/replacement/dispose, paused timer liveness and retry ownership |
| Original fetch/JSON terminal and own withdrawal | Entire availability-read suite; original cause identity and HTTP/JSON/network negatives |
| Native response gate and journal | Existing gate/probe tests and canonical lint/type checks; no new instrumentation framework |
| MCP and skill lifecycle regression | Exact primary command below with zero retries; original MCP assertions preserved |
| API/transport indirect dependants | Direct/Relay, saved readers and network qualifications selected by docs/agents/pr-validation.md |
| Document return and authentication | Authenticated history/reload fresh-document recovery; conditional lifecycle tests; ACCOUNT and OAuth polling 401 cases |
| Source integrity | Canonical pnpm lint:fix without a diff, pnpm check, exact CLI collection, final commit/tree identity |

The primary command remains:

```sh
pnpm test:e2e deliverables.spec.ts skill-bundles.spec.ts beacon-sharing.spec.ts beacon-installations.spec.ts mcps.spec.ts --workers=2 --retries=0 --fail-on-flaky-tests --trace=on
```

The current history/reload correction is source-only and unqualified at runtime.
Earlier exact-source evidence remains scoped to its own head: the primary command
passed 48/48 on b186, while the first primary 47/1 and all historical failures
remain preserved. No result transfers to this corrected source automatically.

## Corrected physical admission criterion

ROOT explicitly retained the original conditional product contract: retire old
work at departure and resume fresh work when the document returns. It does not
promise universal browser BFCache admission. The added physical assertions used
a surface that [Playwright v1.64.1 does not support](https://raw.githubusercontent.com/microsoft/playwright/v1.64.1/docs/src/navigations.md#L140-L146).
Neither removing Chromium's default flag nor changing a navigation wait provides
supported BFCache qualification.

The authenticated physical case and later synthetic fixture have been removed
from the delivered diff by forward commits. Their source history, reports and
runs 42841980306, 42875372366 and 42886703157 remain preserved and RED. There is
no skip, retrospective PASS, cache-miss-as-restoration claim or cache/header/
authorization/runtime change. Physical BFCache remains NON QUALIFIED.

The replacement `availability-history-reload.spec.ts` uses the existing normal
Chromium/WebKit profiles without BFCache reactivation. It requires a real history
return with a network document response and a distinct new Document, then a real
reload and another distinct Document. Both phases require the original owned
withdrawal/JSON terminal and HTTP cancellation before held body release, a new
real availability request/exact 200 body, Online/Direct recovery and complete
closure. An unexpected retained document, missing network response, duplicate
poll, unknown terminal or unexpected error fails this accurately named recovery
case; it does not satisfy a BFCache claim. No synthetic lifecycle event is used
by this recovery test.

The same-document MCP case still labels its events SYNTHETIC and the real-module
Node suites remain conditional source evidence. They preserve generation/read/
session ownership, the SAME earlier TypeError, genuine active errors, retained
navigation/download, retry cadence and rearming. They do not become native
app-generation/publication correlation or physical BFCache evidence.

ACCOUNT and OAuth availability-401 qualification is still due on the final head.
The OAuth journey now explicitly revokes its OAuth browser session while a sibling
is polling, observes the real availability401/login redirect, and requires polling
to remain stopped. This is prepared source, not an executed authentication result.
Final source identity and separately selected networking remain due. No document
or correction of the criterion closes P1 or delivers the product.

This is web document-lifecycle error handling, not a new UI capability. Android,
public MCP and the built-in installation tool retain their existing contracts;
no Rust/backend, Android, mutation replay, grants, history/cursors or storage
behavior is changed by this repair. Incoming main changes are preserved.

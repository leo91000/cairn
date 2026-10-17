# Slack plugin implementation

Q20 authorized the complete contract in `SLACK-PLUGIN-SPEC.md`, including its
dedicated branch and PR. Base: `be40dc4b3a75a788a7678e310f185b9bb1eba12a`.

The agreed test seams are the manifest contract, public installation/Beacon HTTP,
the actual Luau worker against HTTP/WSS fixtures, granted CLI/MCP operations and
the web/Android journeys. Tests assert observable behavior at those interfaces.

## Delivery checklist

- [x] Signed generic HTTP ingress, bounded responses and precise durable admission.
  - Manifest declaration: first red/green test passes.
  - Installation and Beacon paths, signature/replay checks and payload semantics.
  - Installation tests 8/8 and Beacon relay test 1/1 pass against actual HTTP/worker
    interfaces. Includes queue saturation, exact signed bytes, isolated public
    authority, disabled instances, malformed responses and a slow-body deadline.
  - Expiring owner-bound setup drafts now accept only declared signed challenges;
    their owner forms now reuse saved secrets/credentials and expose stable callback
    URLs; OAuth/activation and both client journeys remain below.
- [ ] Generic resources, routes, granular grants and declarative collections UI.
  - Native collection CRUD, encrypted fields, parent references, revision checks,
    cascade cleanup and resource credentials implemented; three worker/HTTP tests pass.
  - Operation/profile/scope grants, request bindings (including resource metadata),
    form encoding and CLI/MCP enforcement implemented; the first end-to-end grant
    journey passes, including revocation and agent deletion. More identity, profile
    and concurrency cases remain, along with both collection interfaces.
  - Host-owned routes select a dedicated agent/project per resource and source
    context. Disabled overrides and equal-specificity conflicts fail closed;
    resource/route/owner/package revisions fence accepted work. Access requests
    and people approvals stay local to their route. External turns retain their
    own subject, and stale or cross-author answer/stop controls are refused.
  - Shared turns may reuse a question ID. Notification deduplication belongs to
    the issuing delivery, so a later author's question remains visible and an
    earlier turn's controls cannot answer it. `newTurn` queues an independent
    request, rechecks its authority after the previous turn stops and starts a
    fresh native session in the same conversation. Owner pause and workspace
    settlement are respected. Explicit native thread reset is implemented below;
    its Slack commands/buttons remain in the package delivery.
- [ ] Generic OAuth, secure token capture/rotation and personal identity binding.
  - Owner-connected resource OAuth: declared scopes/endpoints, confidential and
    PKCE clients, encrypted single-use callback capture, same-owner completion,
    direct/Beacon callback and nested token response mapping implemented.
  - Automatic renewal serializes concurrent callers and persists consumed refresh
    state before I/O; uncertain responses require reconnection. Local disconnect
    invalidates pending exchanges. Resource identity and requested/granted scopes
    remain distinct. Targeted tests cover these paths through actual HTTP/Luau.
  - Personal OAuth management is isolated by authenticated Cairn account and its
    installation-access revision, with a host-captured external subject and verified
    resource identity. Tools/commands use the current authenticated Cairn chat
    author; message-ID replay and cross-author steering cannot transfer authority.
  - OAuth-proved subjects now link unambiguously to their Cairn account within a
    resource/provider. Routed external turns select only that subject's personal
    authorization. Admitted external people can now start and finish their own
    OAuth consent without a Cairn account. The host verifies the exact provider
    subject and stable resource ID, preserves a consent URL across durable replay,
    and never retries an uncertain code exchange. Independent proofs grant no
    Cairn membership; disconnect also removes any credential linked to that same
    proved identity. Configuration drafts, provider revocation/token exchange and
    both client interfaces remain unfinished. This does not complete OAuth or the
    Slack package.
- [ ] Socket acknowledgement/liveness/replacement, authenticated files.
- [ ] Slack Luau package: setup, protocol, conversations and native surfaces.
- [ ] Collaboration/productivity tools with exact scopes and profiles.
- [ ] Owner-authorized automation and strict measurable work limits.
- [ ] Transient requester-only search and proven engine support/fallback.
- [ ] Web/Android parity and browser/device journey evidence.
- [ ] Required full validation, independent Standards/Spec review and corrections.
- [ ] Commit/push, dedicated PR and successful CI.
- [ ] Real Slack validation when an app/workspace is authorized and configured.

No live Slack app, workspace or credential has been accessed or created.
The previous plugin stack (#215, #223) is already merged; do not repeat those merges.

## Foundation checkpoint

This is an implementation checkpoint for a draft PR, not the completed Slack
delivery. The Slack package, OAuth, UI and other unchecked items above remain.

Validation at this checkpoint: 57 selected installation/Beacon tests, 24 plugin
crate tests, workspace Clippy with warnings denied, and `pnpm check` (511 tests,
typecheck, lint and build) pass. No Android UI was changed or device-tested here.
Full final validation, compatibility/storage review and independent review remain
required before the PR can be marked ready.

## Owner-resource OAuth checkpoint

This adds host-managed OAuth for owner-connected resources, including encrypted
code capture, exact requester/resource/approval binding, confidential and PKCE
clients, nested token mappings, identity validation, cumulative scope diagnostics,
serialized rotation and local disconnection. The callback is available through
Beacon and direct HTTPS and cannot acquire account authority. Revoking a run's
resource grant during renewal prevents the following provider request.

The foundation CI failure was an undeclared Reqwest `form` feature in
`cairn-installation`: workspace feature unification hid it locally. The dependency
is now explicit and the isolated installation library compiles.

Local validation: 91 selected installation/Beacon integration tests, including 25
real direct-peer cases and nine OAuth cases across installation/Beacon; 25 plugin
crate tests; workspace Clippy with warnings denied; `pnpm check` (511 tests,
typecheck, lint and build). This remains a draft checkpoint. Personal OAuth,
configuration drafts, the Slack package, web/Android journeys and every other
unchecked item above still have to be delivered; green CI does not complete them.

## Personal-account OAuth checkpoint

The owner OAuth checkpoint's Quality failure was a scheduling assumption in the
skills snapshot test. A delayed writer reproduced the same failure with a coherent
old snapshot. The corrected test accepts either complete version, checks the
description against it and verifies the immutable bundle and a subsequent snapshot.
The active skills suite passes (16 tests; one performance test intentionally ignored).

Personal provider management now binds OAuth to the authenticated account,
installation-access revision, external subject and configured resource identity.
Code exchange cannot be finished by a different account, and per-person disconnect
invalidates in-flight exchanges without changing other people's authorizations.
The native chat interface records the sender alongside each message, so granted
MCP/CLI operations use only their current turn's author. Unattributed submissions,
owner panels, task scheduling and external plugin messages receive no personal
fallback. External identity linking and the remaining delivery checklist are still
required before this becomes the complete Slack integration.

Local checkpoint validation: 170 selected installation/Beacon integration tests
pass across plugins, native chat/sleep/lifecycle/durability, HTTP, MCP, skills and real direct
peers; one explicit performance benchmark remains ignored. All 25 plugin-crate
tests pass. `pnpm check` passes (511 tests, lint, typecheck and production build),
as does workspace Clippy with warnings denied. No Slack app or device journey was
exercised. Final storage/compatibility measurements and independent review remain
open with the full contract.

## Resource-route checkpoint

The personal-account checkpoint passed Quality and Storage at `c314f3f0`. The
next slice implements host-selected routes, route-local people and requests,
native external turn attribution and proved subject/account links. Tests first
reproduced cross-author cancellation, refusal of the rightful external question
answer, escape to another channel and reuse of a new approval by an old worker.
These are tested through signed ingress, actual Luau workers and granted MCP calls.
This does not deliver external nonmember OAuth, reset/setup UI or the Slack package.

The native question-answer path also participates in requester checks. A test first
showed another participant's answer joining a personal turn (HTTP 200); it now
refuses that transfer (409). The owner retains question control from Cairn, while
that intervention suspends personal authority for the turn and rejects stale
in-flight authorizations. A later turn obtains its own authority. Conversation
purge removes both attribution and suspension records.

The eight route journeys pass, including in-flight personal disconnect and retention
of route access requests across app edits. The two socket-completion fixtures now
install and approve their real generic package instead of changing Discord's loaded
source after approval. The selected regression covers 210 installation/Beacon tests
(including 50 actual direct-peer/authorization cases); one explicit performance test
is ignored. The web checks pass with 511 tests. No client UI or live Slack app was
exercised in this slice. Compatibility/storage qualification and final independent
review remain open with the rest of the contract.

## External-person OAuth checkpoint

The resource-route checkpoint passed Quality and Storage at `bd056980`. The next
slice adds `cairn.oauth.start/finish/status/disconnect` for a resource-routed,
admitted external person. The public callback still captures only a code, and the
person must complete consent through an admitted handler with the same native
route authority. This creates neither a Cairn account nor installation access.
Provider proof for a different person or workspace is refused. Personal providers
must capture a stable resource ID; a display name is insufficient.

The first tracer failed because the capability was absent. Further tests reproduced
unbounded pending consents, name-only resource identity and a retry invalidating
the URL already sent to a person. Consents are now bounded, exact durable start
replays keep their URL, and an interrupted exchange never sends its code twice.
An owner-controlled URL setting supports direct HTTPS callbacks without accepting
a callback destination from external message content. No live Slack app or client
UI was exercised in this slice. The remaining checklist is still required.

The selected regression passes 215 installation/Beacon tests, including 12 route
journeys, the independent external consent, replay and interrupted-exchange cases,
and 50 direct-peer/authorization cases. All 25 plugin-crate tests and `pnpm check`
(511 web tests, lint, typecheck and production build) pass. Final full validation
and independent review remain required after the remaining implementation lots.

The next question-notification tracer first sent Alice's question and then hid
Bob's question because both used the same ID. The acknowledgement is now scoped
to the issuing delivery. The real-worker/HTTP journey receives both notifications
once, keeps one shared conversation, and refuses an old turn's answer button.
An existing Discord test caught an old optional question being republished after
the initial fix. Requiring the issuing turn for every instance fixes that case too.
All 166 selected installation/Beacon cases outside the direct-peer suites pass,
including the 13 route journeys; the 50 direct cases pass across the parallel run
and isolated recheck. One WebRTC opening timed out in the parallel run before any
application exchange; that case passes alone. `pnpm check` passes again (511 tests).

## OAuth capacity and queued turns

Quality and Storage passed on `400d4afc`. The next OAuth tracer fills the actual
authorization capacity through owner HTTP and first reproduces a 429 when an
already disconnected person tries to disconnect. Disconnect now releases an
existing credential before reserving its revocation state and creates no state
for an already disconnected person. Active revocation generations remain live
when another consent starts; consent-only and disconnected generations expire.

A second tracer reproduces Bob's queued request remaining behind Alice's stopped
turn. Routed `submit{newTurn = true}` now records an independent request, verified
again against its admitted person, route and thread binding at the transition.
The same conversation/workspace is kept, the old prompt is cancelled and a fresh
native session starts. Granted HTTP proves Alice's credential before and Bob's
after the transition; Alice's old MCP bearer and stop button are refused.
Owner pause, a revoked queued person and an unsettled workspace all prevent the
transition. Once settlement completes, the queued request advances without
blocking another conversation. These tests use the actual Luau worker and HTTP,
with a prepared-checkpoint fixture; no agent VM or live Slack app was launched.
Full reset, setup drafts, client journeys and every other unchecked item remain.

The checkpoint review found two missing cases, reproduced before correction:
new input cleared an earlier owner Pause, and an old queued question answer
prevented the next independent request from being selected. Native explicit
Pause/Stop now records user intent separately from an automatic stopped-turn
pause; only explicit native resume releases it. Resume advances an independent
queued request rather than replaying the stopped prompt. Candidate selection skips
only native answers belonging to the abandoned turn; ordinary requests keep their
queue order. Both focused regressions pass, including cancellation of the answer.

Before these review corrections, the expanded regression passed 529 selected
Rust tests (11 intentionally ignored), including 359 installation library tests,
and the 50 direct-peer/authorization cases passed across the full run and an
isolated retry. One authorization acknowledgement timed out under the full run;
the same case passes alone. Storage library fixtures require `/usr/sbin` in PATH
for the already-installed ext4 tools and bounded test concurrency. `pnpm check`
passes (511 tests, lint, typecheck and production build). Final regression and
fresh checkpoint review evidence accompany this checkpoint in the draft PR;
neither replaces the full-delivery validation and independent review.

The second checkpoint review found that the reply tracker used the changing run
status for every queued delivery. A queued independent request now remains
waiting during its predecessor's cancellation. Abandonment also records a native
per-message terminal receipt, retained until conversation purge, so a late reply
pump cannot collect the following requester's answer or files. A real-worker
HTTP barrier reproduces that interleaving before correction, both with and without
an initial message acknowledgement. It now observes only a cancellation for the
old requester and the correct answer/files for the new requester.

Candidate selection also accounts for ordinary messages already marked sending,
matching the native queue's recovery behavior. A native owner/HTTP journey first
reproduces an earlier steering message being bypassed during owner resume. It
now keeps the ordinary resume path and queue order. All five concrete checkpoint
review findings have focused failing/passing evidence; the next fresh review
covers the complete corrected lot rather than only its last edits.

The third checkpoint review identified a lifecycle distinction hidden by the first
fixture: `completed` records successful agent output, not process settlement.
The transition no longer waits for a success event that cancelled/failed turns
will never emit. It retains the existing recovery wait and worker guards; the
next native execution always fences the saved process/runner before preparing
or launching its workspace. No parallel checkpoint lifecycle field was added.
The actual scheduler/HTTP-controller test now refuses fencing first (503), proves
that the next requester stays queued with no active execution, then confirms the
stop (204) and observes recovery release. It uses a pending engine account to
prevent a real agent launch; native output remains supplied at the event seam.
Both acknowledged and pre-acknowledgement cancellation paths pass, along with all
17 route journeys. Full VM/live Slack evidence remains outside this checkpoint.

Final web verification twice hit the same legacy JavaScript process test: a
supervisor handle was treated as proof that its agent had started, allowing the
running-process cancellation assertion to race startup. The case passes alone.
It now waits for the fixture agent's observable session-start notification before
cancelling, using the existing fixture startup bound and leaving cleanup timeouts
and the legacy runtime unchanged. Full
verification and a fresh review include this test correction as well.

A subsequent checkpoint review found that an exhausted automatic-retry counter
also survived run reuse. The failed-predecessor variant first reproduced Bob
inheriting Alice's third retry. Abandonment now clears that per-request retry
state alongside the native session, matching ordinary native resume. This does
not change the durable retry policy within a request. The affected route suite
is rerun after this final correction; the broader regression and final complete
Slack validation remain separately identified in the PR evidence.

## Explicit source reset checkpoint

An admitted resource participant can reset a settled thread by naming its current
conversation and source thread. The native binding is retired atomically; Cairn
retains the old history/workspace and creates the next conversation only when a
new source message arrives. Pending messages, native preparation/active guards,
unsettled recovery and undelivered replies prevent reset. Reset does not stop
another author's active work. Source controls/deliveries reject retired bindings;
submission rechecks its binding before resuming and before enqueueing after I/O.

The first actual-worker/signed-HTTP tracer fails with the missing capability and
passes after implementation. It proves queued/active/recovering work cannot be
orphaned, another thread/stale button cannot detach the replacement, and Alice's
history is retained while Bob gets a new conversation. A lost-acknowledgement
variant first reproduces a failed reset replay without its native receipt. The
same admitted event now returns the original receipt after a replacement has
been created; later messages still use that replacement. No process, VM, client
UI or live Slack app is exercised by this native lifecycle fixture. This is still
an incomplete Slack delivery; all unchecked package/client/qualification work
above remains required.

The first independent reset review found one omitted pending-work case: a native
scheduled continuation can survive a successful interim turn and reply. A real
native `sleep` registration reproduces reset detaching that unfinished task.
Reset now refuses every saved wakeup, including a due timed wait and paused
condition wait. The condition variants use prepared native lifecycle state,
including a saved wait whose earlier grant has been reduced; they do not claim a
successful `wait_for` registration by a source agent without installation access.
The owner explicitly cancels the future work before the later reset succeeds.
A fresh full checkpoint review follows this correction.


## Setup challenge checkpoint

Quality and Storage passed on `37290ef4`. The next slice adds a separate,
24-hour owner-bound setup record with a stable UUID, encrypted secret fields,
partial manifest validation, revision checks and a transactional limit of 32.
Owner HTTP can create, resume, edit and discard it. No live instance, grant,
delegation or worker is created. Only signed JSON requests matching the package's
declared setup selectors run pure Luau; every host capability is refused.

The actual HTTP/Luau journey first reproduces missing setup support, missing
owner listing, unauthenticated owner-authority calls, revoked-owner capacity
exhaustion, inaccessible obsolete drafts, revival after reinstall and in-flight
creation across uninstall/reinstall. Each has failing/passing evidence. The
corrected path verifies signatures before contacting the owner authority, retires
obsolete owner records before quota checks, keeps obsolete drafts inspectable and
discardable, deletes drafts/secrets at uninstall and checks the exact current
approval again in the write transaction. It also tests secret non-disclosure,
stale edits, expiration, ownership, challenge results and refusal of work.

The additive native KV namespace uses the existing JSON/TTL and encrypted-secret
envelope. Installed package approval records gain an opaque revision ignored by
older readers; existing records without it remain readable. The optional manifest
setup declaration is a new package contract: old manifest validators reject such
packages. Whole-PR compatibility qualification and reproducible storage measurements
remain mandatory before marking the PR ready. No database-version or compatibility
epoch decision is inferred from this isolated slice. Setup form execution, URLs,
OAuth/resources, activation, clients and the complete Slack package remain open.

The first independent Standards/Spec checkpoint review identifies a delayed
former-owner save deleting the replacement owner's drafts during quota cleanup.
The owner HTTP barrier reproduces the stale request returning 200. The corrected
write compares the whole bounded draft snapshot captured before checking owner
authority; any intervening setup write requires a reload. The replacement owner's
draft and signing secret are exercised through owner and signed public HTTP.
Focused revalidation and a fresh full checkpoint review follow the correction.

The selected regression also exposes a stack overflow at the first public HTTP
delivery in the existing reset journey. It passes with all parent source restored;
removing the new setup routes alone does not fix it. Moving the one-shot webhook
invocation future to the heap fixes the same unmodified journey at its default
thread stack size. No test stack limit was increased, and temporary probes were
removed. The full selected plugin regression is rerun with that boundary change.

The second independent review finds a stale snapshot after the final live-owner
check. Holding that HTTP response reproduces a successful challenge despite an
intervening edit. Draft reads now recheck exact stored metadata/expiry after owner
I/O; operational loads also recheck the captured approval in that same native
read. The authority-barrier journey covers edit, deletion and prepared expiry
during the post-handler check. Final focused regression and a fresh complete
checkpoint review are required on this corrected state.

The third Spec review identifies a different package capture between the initial
draft lookup and the webhook's loaded source. The loaded package is now compared
directly with the captured draft digest before its signature/selectors are used.
This closes an A/B activation interleaving supported by static control-flow
analysis; the precise interleaving has no deterministic HTTP-barrier regression
here. Existing sequential approval-change and full HTTP/worker cases are rerun,
and another fresh complete review assesses the final binding.

## Owner setup forms and durable requester continuation

The main integration checkpoint adds the current scheduler and corrects an OpenCode
fixture race: the test now waits for a positive parsed PID before cancelling the
process. It still asserts timely cancellation and actual process death.

Persisted setup forms use the same stable draft ID and encrypted fields as signed
setup challenges. The owner may temporarily override values for validation without
saving or activating them. Credentials share the draft's expiration; form Lua sees
only a configured flag. Native callback addressing supplies Beacon by default and
an explicitly selected direct HTTPS origin. Deleted or changed drafts cannot
return held HTTP results to Lua. Draft resources, OAuth, activation and both client
journeys are still pending.

Two native lifecycle tracers found requester defects: a durable wake lost its
external resource/channel bounds and personal OAuth subject, and owner question
control suspended personal OAuth by dropping those same source bounds. Native
wake inputs now inherit the verified prior requester and any suspension. Bot tools
retain their original source constraints; personal OAuth stays suspended after
owner intervention. In-flight grants also snapshot suspension. Tests use actual
HTTP, Luau and MCP plus the native sleep/scheduler with a prepared completed
checkpoint; no agent process/VM or live Slack was exercised.

This is a checkpoint, not the complete Slack package or a final whole-PR review.

## Inert draft resources

Owner-only draft resource configuration reuses the declared schema, parent checks,
revision guards and pagination through an explicit native configuration target.
Draft metadata and encrypted fields have the root draft's expiration and a separate
namespace. Live operational paths never fall back to drafts. Discarding the root or
a parent removes its resource tree without affecting another draft or live app.
Draft OAuth, activation and both configuration clients remain unfinished.

## OAuth for inert setup resources

The resource checkpoint passed Quality and Storage on `7d5718de`. The preceding
Android direct-network capacity reconnect timed out once; the same test passed on
that checkpoint without assertion or timeout changes. No precise cause was proven.

Owner-only setup OAuth now shares the native consent/exchange protocol with live
resources, with explicit draft provenance and a separate management prefix.
Pending consent is capped by root expiry; finalized encrypted credentials share
that expiry. Anonymous callbacks capture only a code, while the same verified
owner finishes the exchange. Public resource metadata remains inert; live OAuth
finish and credential resolution refuse the setup provenance. Personal providers
are unavailable until activation. Draft disconnect/edit/discard during a held
provider response prevents late authority from being restored.

The native HTTP/provider tracers cover initial consent through safe status,
namespace isolation and held-exchange revocation. Expiry uses a coherent prepared
persisted draft near the end of its normal lifetime, then the real HTTP protocol.
No live Slack app, client UI, agent process or device was exercised in this slice.
Atomic activation, both clients and the rest of Q1–Q20 remain required.

Independent review found that a consent retained only the package digest: returning
from version B to identical approved A could revive the old state. The HTTP tracer
first observed callback 200 after reapproval. Pending consent now captures the
exact approval record fingerprint and rechecks it at callback, consumption and
final persistence, including ordinary live/personal flows. Both reapproval and
A/B/A transitions are exercised before callback and before finish; revoked consent
must not reach the token provider. Older unfinished consent without this proof
requires a new authorization flow. The expiry tracer also checks the encrypted
pending/final record becomes unavailable at its prepared root deadline, independently
of endpoint refusal; it is not a physical disk-erasure measurement.

## Atomic setup activation

Owner activation prepares the ordinary instance validators without publishing a
temporary connection. Its final form retains the saved UUID and setup URLs and may
supply derived required fields. The final transaction checks the exact owner,
approval, root and complete resource snapshot, validates required fields/secrets
and parents, publishes original IDs, promotes finished owner OAuth, clears draft
credential TTLs and consumes only setup metadata. Pending consent is discarded;
late provider completion cannot restore setup authority. Failures roll back the
entire transition and leave the draft available.

The first owner HTTP/provider tracer reproduced the missing activation endpoint
(404) and then exercised retained IDs and actual Luau OAuth injection. A second
tracer reproduced premature required-field validation before derived form values.
Held form responses cover root/resource edit/delete, prepared expiry, reapproval,
owner-access replacement and a winning concurrent activation. Separate prepared
lifetime tests observe root/resource/OAuth credentials after the original deadline,
not a full-day wait or physical disk erasure. No client UI, device, live Slack or
agent VM was exercised. Both configuration clients and all remaining Slack lots
remain required; this is not final whole-PR qualification.

The first independent activation review found that a form-derived OAuth client ID
could differ from the app which issued the saved tokens. The actual owner/provider
tracer reproduced promotion under the different client (HTTP 200). Activation now
refuses that transition transactionally while preserving the draft authorization.
Saving the final client ID and completing fresh consent allows activation; unrelated
derived fields remain supported. Both fresh review axes must cover the complete
corrected checkpoint before it is committed.

## Generic owner setup and resource interfaces

The native activation checkpoint passed Quality and Storage on `bbd5a90d`.
Web and Android now select a persisted setup for manifests declaring resources,
OAuth or webhooks. Owners can save partial settings, close and resume with blank
secret inputs, prepare resources and activate the same UUID with no agent grants
by default. Discard requires confirmation. Active collection management provides
parent choices, pagination, creation, revision-bound editing and confirmed deletion.
The ordinary instance editor remains available for simple manifests.

The actual installation/browser journey exercises retained IDs, secret masking,
resource creation/promotion/edit/delete and confirmed draft discard. Holding the
resource POST first reproduced an enabled activation button; activation now waits
until resource editing and writes finish. The shared Compose journey covers the
same setup/active resource transitions through synthetic HTTP responses. This does
not yet deliver OAuth controls, routes/profiles/grants, the Slack package or the
rest of Q1–Q20. Device and final checkpoint validation are recorded separately;
no live Slack app, workspace or credential has been used.

The first independent client checkpoint review found two protocol/parity gaps.
Android omitted the changed field from form requests; its HTTP/Compose tracer
observed `null` instead of `clientId`. Both clients also hid preserved drafts when
a newly approved simple manifest no longer requested setup. A real browser
version-change journey and a synthetic Android journey reproduced the missing
discard control. Android now carries the field/credential key, and both clients
list existing drafts independently of how they create new instances. Fresh full
checkpoint reviews cover these corrections; final whole-PR qualification remains
separate.

The second Spec review identified invisible credential errors and a valid Luau
`nil` form result being treated as a failed response. Both setup clients now show
credential errors and normalize absent guidance to an empty successful form.
The browser tracer uses an actual package which requests a credential, then returns
`nil` through save and activation; Android exercises the same HTTP protocol. These
corrections require another full independent checkpoint review.

The third Spec review reproduced an error for a declared hidden field which
silently disabled activation. Setup and ordinary instance editors now summarize
errors without an inline control. Form normalization also accepts explicitly null
optional guidance fields. The shared device scenarios explicitly start their HTTP
fixtures before composing a screen; their initial lazy startup was refused by
Android's main-thread network policy. Static collection fields are delivered here;
scoped dynamic resource forms/diagnostics remain a separate native/client slice.

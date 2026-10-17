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
  - Setup-only expiring drafts remain in the OAuth/configuration slice below.
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
    settlement are respected. Full conversation reset remains open.
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

# Slack plugin implementation

Q20 authorized the complete contract in `SLACK-PLUGIN-SPEC.md`, including its
dedicated branch and PR. Base: `be40dc4b3a75a788a7678e310f185b9bb1eba12a`.

The agreed test seams are the manifest contract, public installation/Beacon HTTP,
the actual Luau worker against HTTP/WSS fixtures, granted CLI/MCP operations and
the web/Android journeys. Tests assert observable behavior at those interfaces.

## First Slack exchange (HTTP slice)

The user reprioritized delivery toward an actual package and exchange before more
foundation polish. `plugins/slack` now contains the manifest, setup form, HTTP
normalizer, conversation/reply handlers and HTTP helper. The real-worker HTTP
fixtures pass: create an inert owner draft, generate the app manifest and stable
URL, accept a signed setup challenge, verify bot/app/public channel, activate,
reject a bad
signature and an unapproved person, commit a mention once, execute it through the
real manager/native chat adapter with a fake provider process, and return that
process's answer to the source Slack thread. Native requester and route/resource
bindings are asserted. Fresh visibility checks prevent import or export after
channel conversion. Native bounded checkpoints prevent repeating uncertain posts
after restart; shared workspace/method cooldowns preserve a 900-second provider
deadline across deliveries and early scheduler wakeups. Method slots are reserved
before provider I/O/admission and retained, so a full store cannot turn a definite
429 into an uncertain write. HTTP readiness is reported by the package's start
handler. These cases execute real
turns through the fixture provider; they do not synthesize completed database work.
This is a local Slack HTTP fixture, not a real workspace
or real model. See [Slack's current slice and outstanding features](SLACK.md).

The complete Q1–Q20 contract remains required. No app, workspace, token or channel
has been created or accessed on Slack; PR #226 stays draft and unmerged. Existing
review findings and interrupted browser qualification remain tracked. Neither this
slice nor its fixture success is a clean full review, complete CI or release gate.

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

## Resource routes and authority checks

Both clients now manage resource routes, participants and pending access requests.
Route edits carry the saved revision. Late reads cannot reopen a closed editor;
uncertain writes require an explicit reload before another mutation. The owner
chooses the agent and optional project from native eligibility results computed
with the same policy used at admission. These results expose eligibility and a
reason, without disclosing an agent's granted access to installation members.

Removing and approving a participant again creates a new admission generation.
Previously queued work and in-flight results cannot acquire that new permission.
Legacy admissions without a participant generation fail closed. Commands and MCP
tools retain the originating requester's resource/channel restrictions, including
when they use another explicitly granted instance of the same integration.

Native chat submission captures its requester together with the message. Routed
submissions pass their verified external principal; management MCP submissions
remain anonymous. Editing another author's queued message does not retain that
author's personal OAuth. Adding a resource grant to an agent already configured
for an Everyone route is refused, as it is for instance-level public delegation.
The existing whole-instance grant contract still rechecks public policy and stops
an incompatible public delegation.

Native scheduling and preparation retain the routed requester's original proof,
including across capacity waits and immediately before agent start. The native
regression first left all four revoked sources queued and now cancels them. The
extra delayed-owner-lookup and temporary-outage cases also pass in the 38-test
native routing/grants/pages checkpoint. Route/grant management uses bounded
cursor pages on both clients. The public HTTP regression reproduced
9,291,339-byte grant and 14,300,719-byte route responses against the 8,000,000-byte
relay limit; the corrected responses remain below that limit across all pages.

A separate public HTTP regression reproduced an 8,336,211-byte setup-draft list
using a documented owner-copied TOML package and ordinary bounded draft requests.
The upload variant was not valid evidence because its canonical manifest could
not be approved. Setup lists now use the same encoded-byte budget and both clients
follow every cursor before exposing the saved drafts. Full corrected-page,
browser and device qualification remains in progress.

Deferred credential exchange and renewal recheck the exact authorization after
remote authority lookup and immediately before provider I/O. Attachment submission
also rechecks owner/package/admission authority after download, after waiting for
the shared quota lock and before publishing file metadata or the message. These
paths share the ordinary attachment quota and clean up unpublished file bytes.

Setup and resource editors reuse the shared field normalization contract on web
and Android, including exact choice IDs, numeric values, empty values and secret
whitespace. Web dictionaries for plugin-defined collection and field names have
no inherited entries.

The current integration includes main through `19300c12`, including agent-centric
instance grants. Saved resource rules remain separate and never grant instance
access themselves. Both clients retain existing plugin editing on hosts without
the served `setupDrafts` marker; supporting hosts still load every setup page.

Configuration writes and resource previews revalidate installed files after the
last remote authority lookup, then retain their transactional approval/revision
fence. OAuth completion and renewal use this boundary too. Question-answer receipt
retries acknowledge an existing answer without recapturing a later turn's author;
reusing that receipt ID for a new question is refused.

New persisted plugin
manifests require a separate compatibility boundary: epoch 13 / SQLite 12, above
main `0a46294c` and its reserved epochs 5–11. The [persistent-format inventory and
forward recovery plan](SLACK-PLUGIN-COMPATIBILITY.md) retain the unchanged legacy
tree and require a separately reviewed production transition. The route-authority
measurements document bounded record shape and their limits; full native/storage
and release qualification remains open. Scoped resource forms retain their app,
parent, owner and OAuth authority on both clients. Remote query/cursor choices
are being qualified in all four configuration contexts, with bounded page windows,
independent selected IDs and a host-owned lookup echo. Route-local usage summaries
are described below; catalogue totals and the remaining Slack delivery list remain
pending.
Android route drafts also use the bounded local input store instead of accumulating complete routes and people directories in activity state. Recovered edits require a fresh matching route revision; missing snapshots cannot become broader writes. Validation and independent review of this checkpoint are still in progress.

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

## Personal OAuth discovery and client controls

The setup UI checkpoint passed Quality, Android and Storage on `d76d008e`.
The next slice exposes a narrow member-readable directory of configured personal
providers. Only the current person's subject/status and safe resource labels are
returned; owner configuration and other people's credentials stay private. Current
membership/owner access are rechecked around package loading, followed by approval
and configuration/authorization reads in one SQLite snapshot. Pagination is bounded
to 100 entries, and paused resources retain only existing personal authorizations
so their owners can disconnect them.

Atelier links to a dedicated personal-accounts page for both roles, on web and
Android. Scopes are explicit, credentials remain native, HTTPS consent is opened
by the person, and completion is checked after returning. Pending callbacks can be
checked again; uncertain exchange failures require a new flow. Nonces and consent
URLs stay in screen memory. Disconnection clearly distinguishes local removal
from provider-side revocation. No agent receives a grant from this operation.

Native tests exercise real provider exchange and identity isolation, 100+1 pagination,
membership removal/rejoin, paused-resource discovery/disconnect and membership,
owner-access or package changes held during discovery. Browser provider consent is
synthetic at the public HTTP boundary; Android shares the same HTTP protocol with
an explicit member navigation case. Neither proves a live Slack installation.
Owner-resource OAuth controls, scoped resource forms, routes/profiles/grants and
all remaining Slack lots are still required; this checkpoint does not finish Q1–Q20.

## Owner resource authorization interface checkpoint

The generic web and Android resource managers now expose shared owner OAuth for
both inert setup resources and active instances. Explicit scopes and a consent
link remain in screen memory. An empty override uses the instance’s configured
callback (Beacon when no direct origin was configured). The effective callback URL
is shown before opening provider consent; a direct HTTPS override is available.
Connecting grants no agent access. Personal providers remain in the separate
personal-account directory and cannot be connected before activation.

Root saving and activation wait for every resource's writes, editor and pending
consent. Providers on one resource serialize consent and disconnection because
they share its revision; providers on different resources remain independent.
Refreshing retains previously visible resource pages and their open consent
widgets, including after sibling insertion or deletion. An older pagination
response cannot overwrite a newer authorization refresh or its resource revision.
Finished or disconnected authorizations refresh resource metadata and revisions;
setup mutations carry their exact root revision. Disconnection removes local
credentials and invalidates pending sign-ins, while provider-side revocation still
belongs to the remaining OAuth work.

The client journeys use actual setup/resource CRUD and activation in the browser
with synthetic provider completion at the public HTTP boundary. Shared Compose
journeys use their HTTP fixture; native OAuth tests independently exercise real
provider exchanges and setup provenance. This remains an incremental interface
checkpoint. Routes, profiles/grants, scoped forms, every remaining Slack lot and
final qualification in the delivery checklist remain required.


## Resource grant clients checkpoint

Owners manage explicit resource access rules on both clients after activation.
A rule selects an agent, an optional cumulative Read/Contribution/Management
profile, per-operation allow/deny exceptions and optional scope limits. No agent
or base profile is preselected. The preview excludes operations missing any
configured scope. Saved rules expose their individual permissions and confirmed
revocation; the interface explains that other rules can independently grant the
same operation. Provider permissions and requester-only OAuth still apply.

Management uses the existing owner-only active resource HTTP namespace. Inert
setup resources expose no operational grants. The host remains authoritative for
agent eligibility, resource access and each tool/command invocation. Clients do
not automatically repeat uncertain writes and require a fresh rule list before
another change. This remains a delivery slice: route management, scoped resource
forms, the Slack package and the other unchecked contract items are still pending.


## Resource-grant CI integration

Quality and Storage passed on `cc84e445`. Android's device shards and release
smoke passed; two JVM journeys failed while testing the merge with newer `main`.
Both failures were reproduced locally by delaying their actual HTTP responses:
the resource editor asserted an empty rule list before its GET completed, and
the connection-recovery journey submitted its next action while the workspace
reload was still busy, so the view model correctly ignored that action.

The shared JVM/device fixtures now hold and explicitly release those responses.
They check the loading controls and the cleared connection notice during an
unfinished reload, then await the completed rule list or workspace before
continuing. Existing assertion deadlines are unchanged.

Independent integration review also found that main’s recovery of failed GETs
could keep the rule editor busy indefinitely while offline. Both clients now
allow dismissal of read-only loading, cancel that read and discard late results.
An outstanding mutation remains locked; its confirmed result permits closing
during the following list reload. Reopening always requests a fresh list, without
repeating the mutation. The browser journey exercises both offline entry and
post-write reload with the real native grant endpoint.

Device qualification also measured first plugin reads starting 5.5–6.5 seconds
after the workspace became idle. A controlled HTTP/UI journey reproduced the
page waiting for its periodic refresh instead of resuming at that transition.
The Android plugin page now restarts its paused read when the workspace becomes
idle. The new journey checks both the occupied-state gate and the first request;
existing assertion deadlines are unchanged.

The integration includes `main` at `19c47868`. This is a CI correction checkpoint;
resource routes, scoped forms and all other unchecked Slack items remain pending.


## Integration with package-owned program installers

The integration with `main` at `870c49c8` retains JSON and TOML package manifests,
package-owned Luau installers, tracked preparation and program check/install/restore
on both clients. Setup, resource, OAuth and grant interfaces remain available.
Ordinary instance forms follow tracked preparation while retaining their existing
normalization of optional guidance. An HTTP `null` response passes through the
preparation helper unchanged on web and Android; it does not become an invalid
operation envelope. Dedicated HTTP/client regressions cover this boundary.

Cold executable preparation rechecks the caller's instance/grants and the loaded
package approval before reading credentials, then again before spawning after
credential I/O. A revoked command cannot start a provider program and merely hide
its result afterwards. Tracked package approval captures the initiating authenticated
owner and exact access grant; both remain required after installation I/O before
publishing activation. Approval records are merged inside the publishing transaction
to preserve other approvals completed during preparation. Native regressions hold
the publisher download while revoking an agent or replacing owner access, including
replacement access for the same account, and check stale authenticated admission.

The same captured owner now accompanies ordinary forms, views, tests, actions and
saves through synchronous or tracked execution. Setup forms retain their separate
draft-owner proof. Program management rechecks its initiating owner and active
package after waiting and I/O, before publishing a candidate, update or restore;
package approval uses its reviewed but not-yet-active package instead. Inert
runtime preparation does not grant permission to execute or activate its result.

Agent scopes also retain the issuing run's host-only bearer. Instance grants and
resource grants recheck that live execution proof; a stopped, expired or revoked
run cannot start a new program or provider request after preparation or OAuth
renewal. HTTP effects recheck caller authority after credential I/O before sending.
Native regressions observe the actual executable marker, installed release and
provider request count while revoking those authorities at controlled barriers.

This integration still requires fresh checkpoint qualification after resolving the
merge. Its prior checkpoint results do not qualify the new source, and all unchecked
Slack deliveries above remain required before final whole-PR validation.

## Android form restoration and CI synchronization

The Android route-edit journey now holds its second eligibility response: Save
must remain disabled until that response arrives, including while the route is
paused. This reproduces and corrects the premature enabled-button assertion in
the preceding PR CI run, without changing production eligibility or test deadlines.

Ordinary route, grant, resource and setup inputs, together with their editor
identity, survive Android recreation. Credentials, typed secrets and OAuth
consents stay in memory only. The restored editor reloads its target and authority
before enabling a mutation and keeps the original revision rather than rebasing
stale input. Failed reads can be retried without discarding the draft. Resource
paging restores only the active editor's small identity reference, not entire
resource lists. An uncertain creation requires an explicit fresh read before
another mutation; recreation never replays the creation.

Four shared JVM/device regressions first reproduced the lost drafts, then passed
with saved ordinary state and cleared secrets. A further regression reproduced a
disabled reload after a failed restoration read. Expanded held-read, retry,
uncertain-creation and later-page coverage is undergoing qualification, along with
the complete Android checks and fresh independent whole-PR review. This is still
an unfinished Slack checkpoint.

The previous exact-commit Quality job passed all 1,276 Rust tests (40 explicit
ignores) and all five browser groups, but its overall result was cancelled at the
30-minute job deadline. The global job budget is now 45 minutes; individual test
and product deadlines are unchanged. The previous full manual Android run passed
502 JVM and 159 API 36 device cases, including 28 plugin journeys, plus lint,
APKs and release/JNI smoke. Those results predate the restoration correction and
do not qualify the new source. Storage passed on that previous commit too.

The [token lifecycle research](SLACK-TOKEN-LIFECYCLE.md) records remaining OAuth
provider work and the documented uncertainty around static app-level migration.
It does not implement those remaining features or establish live Slack behavior.

Three further client-boundary regressions were reproduced during independent
review. Web resource creation could be repeated after the native server committed
but its response was lost. It now keeps a submission barrier until an explicit
read succeeds, including when the first reconciliation read fails. Both active
and prepared resources use this rule.

A valid setup with expanded defaults could exceed the generic 150,000-byte
request limit. Owner instance/setup configuration routes now use the existing
bounded transport budget, while ordinary API and public ingress limits retain
their existing bounds. The native regression first failed with HTTP 413.

The same setup produced 1,543,572 bytes of Android saved instance state. Setup
editors now save their identity and expected revision, reload their server
snapshot and keep larger ordinary setting inputs in device-private, backup-excluded
files behind a small scoped reference. The file store filters declared ordinary
fields, expires snapshots after 24 hours and bounds retained files. Scope, session
and logout cleanup delete those files and retire open editors so a late save
cannot recreate them. Credentials and OAuth consent remain memory-only.

The large setup's recreation regression passed on unchanged sources after an
initial fixture-startup timeout, without increasing its deadlines. Storage
contracts cover scope/credential isolation, atomic recovery, expiry, retirement
and retention, and include a reproducible local read/write/CPU measurement harness. These
corrections still require complete client/native qualification and a fresh
independent review; they do not complete the Slack contract.


## Worker frame and compatibility follow-up

Independent review reproduced a resource-list worker failure: eighteen accepted
resources with 60,049-byte settings produced a 1,086,434-byte capability reply,
above the worker's 1 MiB frame. Resource pages now retain the 100-record ceiling
and add an encoded-byte bound, with ordinary continuation cursors on both HTTP
and Luau. No record is truncated. The real HTTP/worker reproduction and native
regression both failed on the previous source.

The wider configuration HTTP budget also exposed a second boundary: thirty
individually valid Unicode values could be persisted as more than 1 MiB of root
settings. A native HTTP regression reproduced HTTP 200 instead of a safe refusal.
Root settings and partial form inputs now have a 512 KiB encoded aggregate limit,
checked before persistence or worker invocation. Large valid configurations below
that bound retain their complete values. These corrections are undergoing native
qualification.

New persisted manifest declarations require the separate plugin boundary and the
[forward/recovery inventory](SLACK-PLUGIN-COMPATIBILITY.md). After integrating
trusted native execution through `176969f5`, this boundary is epoch 7, above main's
epochs 5 and 6. The previous routing slice's additive-field analysis did not
establish whole-plugin compatibility.

The pre-merge Android correction passes 518 JVM cases, including forty shared
plugin journeys, four local-store contracts and nine scope transitions. Its
[reproducible local reference](performance/ANDROID-PLUGIN-DRAFT-STATE.md) records
1,280 bytes of saved state versus the reproduced 1,543,572-byte baseline, with a
260,251-byte private file. This is Robolectric/tmpfs evidence, not device storage
or live Slack evidence. Current merged-source validation and new independent
whole-PR review remain required. Six TypeScript fixture tests failed while lint
was active; their unchanged quiet qualification is pending, without deadline or
assertion changes. This checkpoint does not complete Slack Q1–Q20.


## Pending-delivery isolation follow-up

A full independent review identified a route-selection conflict during binding
revalidation. An equally specific overlapping route added after admission could
return 409 from the reply pump and repeatedly restart its shared worker, leaving
unrelated replies queued. A real-worker regression reproduces that exact error
before the correction, with the ambiguous delivery first in the queue. Existing
bindings now lose authority with 403; ambiguous new admission continues to return
409. The regression also checks that the revoked route publishes no reply and
the unaffected route finishes through the same live worker. Qualification and a
fresh complete review follow the correction.

A separate intermittent legacy TypeScript fixture assertion observed failed
status before asynchronous account cleanup. The unchanged reference worker on
main exposes that ordering under a held cleanup probe. The test now waits for
the same zero-lease condition and covers a process starting after its original
execution budget; no product timeout is increased. This concerns the retained
reference test fixture, not a Slack transport or the native execution runtime.

The merged native configuration/resource suites pass ten tests, including the
real-worker frame regressions. The prior Android inputs pass 518 JVM cases, lint
and APK builds; those results do not qualify Android changes inherited from the
latest main integration. Full current-source checks, browser/device evidence,
exact-commit CI and the remaining Slack Q1–Q20 lots remain required.


## Requester and runtime-revocation follow-up

A full independent review reproduced cross-author steering into a stopped turn.
On native resume, the old input was delivered while tools still carried the
original author's personal OAuth authority. Admission now checks the current or
resumable turn, and native consumption separately rechecks each queued input
against the current chat, run, delivering message and recorded requester. Owner
question answers retain their existing control and suspend personal authority.
The regression exercises actual native delivery and the authenticated tool call;
a second case covers a formerly accepted input already in persistent storage.

Granting an agent private whole-instance plugin access could also invalidate an
accepted public route with HTTP 400. The scheduler treated that as an internal
failure instead of retiring the source. Runtime public-route revalidation now
returns 403 for that policy refusal, retaining internal errors and the 400
configuration validation contract. The real scheduler reproduction fails before
the correction. Native green qualification is still pending.

On Android, a routed participant request no longer shows the root-instance
approval actions, which target a different endpoint. The regression first found
the incorrect visible controls. Large ordinary inputs in an unsaved resource
editor now use the same scoped private store as setup inputs; typed secrets stay
in memory. The saved-state reproduction failed at 1,173,992 bytes before the
correction and now restores the ordinary inputs and clears the credential within
a 65,536-byte saved-state ceiling. These inputs each meet the field limit but
exceed the aggregate limit for saving a resource; the test does not claim that
such an oversized resource can be persisted. Full current-source Android checks
and fresh whole-PR review remain required.

The reference TypeScript worker's zero-budget resume assertion also waited on
status before asynchronous workspace restoration finished. A held-restore probe
on unchanged main proves that ordering. The regression now waits for actual
worker settlement, still requires failed/time-limit status and no restarted
agent thread, and covers immediate and 1,250-ms workspace restoration without
changing the execution budget or test deadline. This reference fixture work does
not implement the remaining Slack transport, tools, automations or RTS contract.


## Package invalidation, direct availability and encoded ingress follow-up

The next complete independent review found three further boundaries. Actual
regressions reproduce all three on unchanged production sources: an invalid
approved disk manifest returns 400 from a native scheduling tick and blocks a
later ordinary run; a signed 128 KiB UTF-8 control-character body plus valid large
settings returns 502 before its worker handler; an ordinary authenticated chat
message returns 503 when live personal-authority lookup is unavailable.

Approved-package validation now treats an invalid manifest or package-size
refusal as loss of approval, so its admitted source is retired while unrelated
work reaches its capacity check. Owner review/installation still report actual
package validation errors, and internal failures keep their diagnostics.

Ordinary native message/edit/question mutations retain their authenticated
request authority during a Beacon outage. Unavailable personal proof retains the authenticated account but no
personal membership generation, with no retroactive OAuth upgrade or owner-token
fallback. Distinct unverified accounts cannot steer one another’s turns. Explicit identity/membership refusals still fail. Owner question
control follows the authenticated relay/direct role and suspends personal
authority as before. The regression exercises native authenticated HTTP dispatch,
including editing and owner question control; it does not claim a WebRTC handshake.

Generic host event frames now have a 2 MiB encoded budget, while package loads,
capability calls/results and worker output retain the 1 MiB budget. The raw HTTP
bounds and 2.5-second deadline remain intact. The regression requires the complete
control-character body and the first/last large setting to reach the real Luau
handler, which returns a short response within the separate 64 KiB response bound.
The previous red request failed at the input frame before that response boundary.

The preceding twenty-group native run passes 332 cases, with only the two newly
introduced route/frame regressions red and three intentional measurement ignores;
targeted native dispatch reproduction is also red before correction. The current
web run passes 599/602 cases; its three failures are in the retained reference
restart fixtures under concurrent builds/tests, with quiet qualification pending.
Corrected native green checks, fresh complete review, current Android/browser
qualification, exact-commit CI and all remaining Slack lots are still required.


## Requester control and package-file validation

A missing or non-UTF-8 approved package file now loses its source admission
without aborting unrelated scheduling. Other package filesystem failures remain
explicit temporary unavailability with their I/O diagnostics; genuine database
errors retain their normal error handling.

Owner answers carry exact host-owned provenance for native delivery while the
original author keeps later question/steering control. Personal OAuth remains
suspended, including in-flight results and wakeups. Ordinary turns admitted
without membership proof retain that restriction after Beacon recovery, while the
same authenticated account can continue the turn. Storage-shape measurements and
their host limitations are recorded in [requester control](performance/REQUESTER-CONTROL.md).
Actual failing regressions were captured before these corrections; current final
qualification and fresh independent review remain pending. This does not finish
the Slack package or the complete contract.


## Interleaved external consent replay

The actual Luau-worker regression A → B → retry A reproduced invalidation of B's
delivered URL. External OAuth starts now retain event-owned idempotency receipts
in the same transaction as pending consent. The subject/provider lock serializes
starts and finishes; receipt validation refuses superseded or disconnected old
starts without creating another flow. Still-current replays retain their URL.
Event completion, retry exhaustion, obsolete event removal and instance deletion
retire the receipts; pending consent keeps its independent expiry.
[Storage-shape measurements](performance/OAUTH-EVENT-REPLAY.md) record bytes,
read/write timings, CPU, reference and host limits. Targeted and
whole-checkpoint qualification remain pending, as do all unfinished Slack lots.


## Revocation during question and progress delivery

A real-worker regression revoked Alice while her question's HTTP response was
held, before the due progress read. The old pump returned 403 and its supervisor
would stop all routes on that instance. Follow-up source failures now finish only
the revoked delivery, as reply failures already do. The regression also requires
Bob's unrelated route to deliver its answer through the same live worker.
Clippy's two reported form issues are corrected without suppressions. Current
whole-checkpoint validation and a fresh independent review remain pending.

## Integration with trusted native execution

Main through `176969f5` introduces a separate host-execution authority. Chat send,
edit, question and management paths retain both that authority and the plugin
requester's personal proof; one never supplies the other. Pausing and resuming
retain source checks, the owner's pause and the native commit-time authorization.
The upstream skills snapshot test keeps its deterministic FIFO interleaving and
the branch's immutable-bundle and subsequent-snapshot checks.

The HTTP regression reproduced a native agent reported eligible for an external
route whose work has no native host origin. Configuration and its eligibility
preview now use the native policy with an explicitly unknown caller. They cannot
borrow the configuring owner's host grant. The focused HTTP case passes; broader
qualification of the merged tree and fresh independent review remain required. The plugin
compatibility boundary is epoch 7, distinct from upstream native execution's 6;
no transition or deployment is authorized by this draft checkpoint.


## Admission approval and personal directory reconciliation

A signed HTTP regression holds the final owner-authority response while another
package version is approved. Before correction, the request was refused but its
message content remained in the native event queue. Admission now retains the
loaded approval across that lookup, rechecks its digest afterward and fences the
exact approval in the same transaction as content and deduplication. Both
one-shot ingress and persistent workers carry their loaded digest into admission.
The real-worker red case is captured; corrected qualification is still running.

The same review found that completing one personal consent refreshed only the
first directory page, removing other loaded sign-in controls and their memory-only
consent state. Web and shared Compose regressions now exercise two separately
paged sign-ins and an insertion before the later pending consent. The provider
responses in these client tests are synthetic. Both clients now reconcile previously loaded IDs before replacing directory
records, retaining stable widgets and memory-only consent. Current corrected
qualification is pending; no live provider or complete Slack validation is claimed.


## Revoked controls at native consumption

An actual native-input regression reproduced a question answer reaching the
running turn after its author was removed and reapproved. Queued external controls
now retain one exact approval and native source through input preparation. The
consumption and final publication transactions recheck that source; slow file
preparation is followed by fresh owner and package checks. Revocation cancels only
that input and reopens its still-current question. Temporary authority outages
defer external controls; the owner's exact native answer provenance remains
independent and continues to suspend personal OAuth for the turn.

The expanded current-author, person-generation, route pause/deletion,
package/owner replacement and owner-rescue scenarios pass through native input
publication. Whole-checkpoint qualification remains in progress. The complete Android run found
three readiness failures; they pass unchanged in isolation. Tests now wait for
ready fields and enabled authorization controls; the complete plugin class also
exposed a missing wait in the new pagination scenario, corrected before its next
run. No timeout or retry budget is widened. This does not complete the remaining
Slack contract.

A separate held-owner-response regression reproduced a profile grant committing
after another package version was approved. Grant creation now retains its loaded
approval through that lookup and rechecks it in the write transaction. A stale
submission must return 409 without creating a grant; a fresh submission under the
current package remains valid. Corrected qualification and fresh full review are
pending.

The final OAuth policy snapshot now also gates publication: the captured owner
access and initiating Cairn member generation must still match at the transaction.
External people retain route/person proof without acquiring a Cairn membership
requirement. The HTTP regression captured an owner revoked in that last response
still receiving an authorized result; owner, setup and personal variants are under
qualification, including unchanged-authority controls and no late metadata, token
or subject-link persistence after refusal.

## Qualification corrections after native integration

The merged test tree exposed three separate failures. Nested owner-form host
effects exhausted the default test-thread stack; the callback now boxes its
effect future. Both setup activation (3 tests) and webhooks (11 tests) pass with
the ordinary stack size. The native-host fixture now supplies current installation
membership as well as independent host trust, retaining its original owner; all
19 native-node tests pass. The skills snapshot fixture removes its synchronization
FIFO before checking the subsequent snapshot; its suite finishes unassisted with
16 passing tests and one existing ignored test.

On an actual Android device, the synthetic Beacon's native Direct authorization
request was being counted as a plugin mutation. The shared plugin fixture now
answers that transport endpoint explicitly before dispatching plugin requests.
Mutation assertions retain their original guarantees and report request paths
when they fail. No production Direct behavior or timeout budget changes.

Fresh whole-checkpoint review and validation are still required. These corrections
do not implement the remaining Slack package, transports, tools, automation or
ephemeral search.

## Large grant input restoration

A shared Compose/Parcel regression reproduced 289,488 bytes of activity state
for one valid eight-scope grant draft. Retained editors could accumulate several
such buffers, including after a rule was saved or cancelled. Android now keeps
these ordinary inputs in the existing bounded device-local snapshot store,
scoped to account, installation and resource path; saved activity state retains
only its opaque reference (1,272 bytes in the same shared JVM regression).
[Storage measurements](performance/ANDROID-PLUGIN-DRAFT-STATE.md) record the
144,081-byte file, read/write latency, CPU, reference workloads and host limits.
Closing, cancelling and completing a submission retire
the input buffer. An unavailable local snapshot cannot restore a rule with its
limits silently removed: the owner must explicitly start and review a new rule.
Targeted and whole-checkpoint qualification remain in progress.

## Independent queued turns after authority failures

Two regressions through signed ingress, Luau admission and the native conversation
queue reproduced a permanent pause: one after a temporary authority 503, the other
after a queued participant was revoked while another authorized person waited.
The host now defers temporary lookup failures without pausing and retires only the
exact revoked request. A changed message, intent or current turn cannot be retired
by an obsolete lookup. Final admission rechecks the source inside the same
transaction; refusal commits without starting a new turn. The owner's explicit
pause remains authoritative.

The regression suite also covers revocation during a held owner lookup. Corrected
native and whole-checkpoint qualification are in progress. The current Android
plugin journeys pass 45/45 on an actual AOSP API 36 emulator with KVM, including
large grant restoration and missing local snapshots. This does not complete the
Slack package or the remaining Q1–Q20 delivery slices.

## Resource authority after the final OAuth lookup

A root owner action reproduced an HTTP result reaching the Luau package after its
resource was paused during the post-response OAuth authority lookup. That lookup
verified the retained credential after an earlier, separate resource snapshot.
HTTP now shares one final local snapshot of resource/ancestor revisions, resource
grants and OAuth references, both before sending and after awaited response checks.
The regression holds the real post-response lookup: its unchanged control succeeds,
while pausing or editing the resource is refused with 403. All three scenarios pass
through the installed package and actual Luau worker. Whole-checkpoint qualification
and fresh independent review remain required.

## Native controller qualification after a crash

The hosted run at `7bb84816` passes the full Rust and web suites, all browser jobs,
Android including the complete device journeys, and the storage reference. Its
native CPU qualification fails while recreating the crashed controller's transient
systemd unit. The unchanged fixture and a reduced controller-crash scenario both
reproduce that failure in an isolated systemd container with private cgroups.

Captured stderr identifies a still-loaded unit name. A workload leaving its active
state does not establish that its supervisor has been unloaded; the
[systemd transient-unit contract](https://www.freedesktop.org/software/systemd/man/257/org.freedesktop.systemd1.html)
requires the name to be available before defining it again. The fixture now waits
for `LoadState=not-found` under its existing deadline and prints captured command
errors. Production execution and its containment checks are unchanged.

The original negative control still fails after the correction. Ten full corrected
trials pass with the actual Cairn binary, including lease expiry, descendant
fencing, guardian and controller crashes, restored workspace, and retry tombstones.
Hosted qualification of this correction and fresh full review remain required.
This checkpoint does not complete the Slack package or the remaining Q1–Q20 work.


## OAuth credential encoding and owner selectors

Confidential OAuth now form-encodes the client ID and secret independently before
constructing Basic authentication. The installed-package regression uses reserved
characters and checks the exact header for both code exchange and refresh, while
concurrent calls consume only one refresh token. It fails against the previous
implementation and passes after correction. The complete 28-case OAuth suite and
the targeted regression on the final formatted source pass.

New owner-management selectors reuse the shared web `VirtualSelect`: setup agents,
resource parents, grant agents/profiles/operation exceptions, and route projects,
participation and eligible agents. Empty profiles and projects remain valid;
required agents and parents have no implicit selection. Native Android retains its
shared `Choice`/`ChoiceList` presentation and the same exact IDs and eligibility
rules. Browser journeys exercise searching and choosing these values, required
parent validation, explicit incoming-agent authority and the saved native payloads.
The web lint, 602 unit tests and production build pass. These corrections do not
complete scoped resource forms, guided route selectors or the full Slack contract.

## Resource-scoped configuration forms

Collections may opt into the shared package `form` handler with `form = true`.
The handler receives the selected collection, saved self and parent metadata,
nonsecret app settings, typed values and secret-presence keys. A private owner
configuration proof binds approval, root revision, saved self and both old and
selected ancestor chains. It allows paused and inert configuration without
changing operational resource references or agent grants.

HTTP authentication can select only the proved `self` or `parent`. New self
secrets remain in memory; owner OAuth requires a saved target and reuses the
single-use refresher, preserving setup provenance and expiry. Personal OAuth
cannot be borrowed by owner configuration. Native save runs submit validation
again, applies declared nonsecret derived values and retains the atomic resource,
quota and vault publication checks. Cold program preparation follows manager
operations on both clients rather than keeping a relay request open.

Both editors show dynamic choices, field/global errors and read-only guidance.
Changing parents invalidates pending results; restored Android drafts must
revalidate and never restore typed secrets. Unknown save outcomes retain the
existing read-before-another-write recovery. The new native, browser and shared
JVM/device scenarios qualify this slice; final checkpoint results are recorded
separately. Guided query/cursor choices now have a shared host contract and web/
Android renderers; checkpoint qualification is recorded separately. Catalogue
summaries and the actual Slack package remain subsequent work. This does
not complete Q1–Q20.

Unchanged catalog refreshes preserve resource editors and pending owner consent;
only changes to the namespace, setup revision, package version or collection
schema invalidate the resource editor. Setup forms and activation use the same
pollable manager operations when a missing executable needs preparation. The
original owner and the setup's existing revision/expiry/approval checks still
control execution and atomic activation; both clients await completion.

Preparing native executions distinguish a temporary source-authority outage from
an agent failure. They requeue through ordinary fencing, release and admission.
The regression exercises outages before and after marking a run running,
automatic re-admission of that same run, and cancellation if owner authority
changes during its next launch check. This does not relax source verification.

Root and resource form callbacks share partial-value normalization. Cleared or
missing optional fields stay absent before Luau runs; final server save/default
completion retains its existing behavior. The HTTP regression compares the same
installed Luau handler on root and collection forms for active and inert setups,
including omitted/null/defaulted scalar fields and explicit zero values.

Ordinary saved-instance previews and remote lookups retain the same configuration
target as resource forms. Root edits and deletion invalidate captured credentials
before and after provider I/O and before the handler result is returned. Saving
also rechecks that root in its publication transaction, so a stale form cannot
overwrite a newer root. New unsaved instances have no persisted root to borrow.

Closed Android grant widgets retire their local snapshots. Only an active rule
editor saves scope inputs, so closed widgets cannot consume the shared bounded
store and evict an unrelated editor during activity-state saving.

## Unattributed questions at the persisted-format transition

The upgrade fixture reproduces a base-release pending question without `turnId`
remaining invisible to plugin delivery after restart. A run can span several
turns, so the host cannot safely fill that field from its current execution.
Startup now atomically retires unattributed open questions and their exact
undelivered answer messages, with one content-free recovery notice per affected
conversation. The notice asks the owner to have the agent repeat the question;
it changes no execution status. Attributed and closed questions retain their
original records. A one-time marker prevents duplicate notices on restart.
The [compatibility inventory](SLACK-PLUGIN-COMPATIBILITY.md) covers this transition
and keeps actual prior-production-data qualification as a delivery gate.
Corrected native/client presentation qualification and fresh complete independent
review remain required. This does not complete the Slack contract.

## Route owner summaries

Paged route management now projects route-local admission usage and pending people
from the same database transaction as the listed routes. The measured hour is
explicit. Volatile summaries do not enter stored records, editing revisions or
execution authority. Web and native Android retain unknown state on older hosts
and hide the preceding list when a reload fails, rather than displaying zero.
Management actions reload the summaries with the current route revisions.

The new public HTTP regression first reproduced missing counters on 35 paged
routes. It distinguishes route and app counters, repeated requests by one person
and requests on another route, pending and denied requests, paused/deleted routes,
owner-only visibility and stable edit revisions. Qualification results are recorded
in the delivery evidence. Catalogue bounds and the rest of Q1–Q20 remain unfinished.

## Separate package reads

The owner API now separates bounded compact catalogue cards, the selected package
manifest, paged version metadata and exact version review. It shares approval and
display-version selection with the existing complete catalogue. Changes to any
Luau file still withdraw approval, and malformed local versions remain reviewable
as diagnostics. These reads do not expose instance settings or personal credentials.
They add no stored format, cache or execution authority.

The new public HTTP fixture includes more than 32 packages and versions, large
setting defaults, exact file/digest review, changed-module approval rejection,
invalid/empty packages and owner-only access. Both client catalogue migrations,
bounded instance and grant selectors, and the remainder of Q1–Q20 remain unfinished.

The synthetic [read/write observation](performance/PLUGIN-CATALOGUE-READS.md)
compares the complete response and first compact page in the same binary. It
records bytes, first/warm read wall time and CPU, unchanged approval-write timing,
workload and host conditions. The compact page is bounded in bytes and count;
validating approval still scans the versions of each selected package.

## Compact catalogue clients and instance reads

Web and native Android catalogue cards consume all compact pages without loading
every instance's settings. One native SQL statement supplies each page's saved
instance counts, live states, pending requests and distinct configured agents
across root and resource rules. These counters are presentation only, not persisted
revisions or execution authority. Earlier hosts with only global resource totals
show unavailable per-package agent totals instead of fabricating zero.

Version review pages aggregate metadata, then fetch the exact selected version
and the active comparison manifest. Pending or failed reads cannot approve code;
changing selection or closing invalidates late results. Approval submits the
digest returned with the displayed files. Invalid versions retain diagnostics.

Additional owner instance APIs select one package before decoding settings and
enrich at most 33 lookahead records inside a read transaction. Count and byte
bounds determine continuation. An exact read retains the complete current grant
IDs for concurrent-save merging, including when a package is paused or its source
has disappeared. Instance/detail and grant-selector client migrations remain.

R59 Android CI's sole JVM failure was reproduced with a held restoration read:
the test clicked Cancel while route revalidation correctly disabled it. The
shared scenario now explicitly holds that read, checks the disabled state, releases
it and waits for the enabled control. The controlled reproduction failed before
the wait correction and passed afterward. This is separate from the older,
still-unexplained 25-second capacity failure. Full checkpoint qualification and
the remainder of Q1–Q20 are still required; the Slack package is not yet present.


## Parent discovery across a changed resource editor

Independent checkpoint review found that a held later parent page could reopen
a resource editor after its namespace or schema had changed. The actual Vue
component reproduced stale editors for schema and instance replacement and a
stale error from a rejected read, in both Chromium and WebKit. Parent discovery
now retains its original path and generation, discards obsolete pages before
publication, and suppresses errors from an invalidated opening. Fresh editing
still loads all parent pages and saves the current schema to the current instance.
The six held-read regressions fail before the correction and pass after it.
Complete web/browser qualification and fresh full independent review remain
required; this correction does not complete Slack.

## Selected package configuration clients

Web and Android detail screens now read the complete configuration manifest for
one selected package, page all its saved instances, and separately page compact
saved access across packages. The latter includes distinct resource memberships
as well as root grants, including paused or unavailable instances. Public-agent
choices retain these restrictions; native authorization remains authoritative.
The compact projection does not materialize settings in Rust or load credentials
or activity. SQLite still reads each underlying instance JSON record.

Configuration DTOs contain no fabricated version history. Review reads remain
separate. An older host explicitly falls back to its complete catalogue; a failed
supported page stays an error. Compatibility saves re-read the exact instance's
complete current root IDs and refuse a vanished instance. A missing package is
confirmed through one compact support page, clearing the screen after uninstall
without an aggregate read. The web captures the
save target and typed fields before this asynchronous read, so closing the editor
cannot turn an update into a creation or clear its outgoing credential.

This slice does not migrate the agent/grant selectors or deliver the Slack package.
Those selectors, provider lifecycle, wake delivery, transport/files, Slack surfaces
and tools, automations, ephemeral search and final qualification remain pending.

## Resource authorization reads on demand

Owner resource OAuth status was fetched on every widget mount. A valid page of
41 small resources and eight shared providers created 328 automatic reads,
exceeding the account's 300-read minute budget on a responsive installation.
The actual Vue component regression observes automatic status requests before
the correction in both engines. Both clients now load a provider only after the
owner opens its controls. Viewing interest is separate from consent: it survives
a resource revision so the selected provider re-reads current status, while the
existing revision key still destroys obsolete consent state. Unopened providers
stay unknown; pending consent on an unchanged resource survives page refresh.
The focused browser scenario passes after the correction; complete checkpoint
qualification and fresh independent review remain required.

An unrelated browser validation ended on a native read quota after a successful
setup. A controlled predecessor fills the real read window and reproduces the
next scenario's failure; a normal backend restart makes that same next read pass.
Plugin journeys now retain fixture data and sign-in while restarting the backend
between scenarios, isolating process-local request budgets. Production quotas
and application deadlines are unchanged. This does not attribute that earlier
failure to resource OAuth, which that setup did not declare, or complete Slack.

## Independent queued requests after a completed turn

The successful-predecessor path now uses the same individual retirement and
requester recheck as the stopped-predecessor path. A revoked queued participant
must not pause the later authorized participants. Temporary authority lookup
failure keeps that request queued; owner Pause, workspace settlement and a
scheduled continuation keep their precedence. A successful predecessor retains
its own completed reply even when its run is reused for the next request.

The HTTP/worker regression first reproduced both the revoked-request queue pause
and the skipped temporary-authority check after a successful predecessor. The
corrected route suite passes 53 tests; chat processes and scheduled continuations
pass 23 and 10 tests, and remote waits pass 32 functional tests (their opt-in
metadata benchmark is separate).

The on-demand OAuth Android regression also exposed a test synchronization error:
its network-first condition skipped Compose semantics reads while waiting for the
refreshed UI to issue that network request. Reading semantics first keeps the same
10-second limit and exact request-count assertions and passes the isolated case.
Temporary diagnosis probes are removed. Whole-checkpoint Android, browser/device
and fresh complete independent qualification remain in progress. This does not
complete the Slack package or Q1–Q20.


The complete Android run also exposed an unnecessary account reconnection in the
shared chat test fixture during state restoration. That reconnect clears the
ViewModel's local drafts. The fixture now retains its existing connection, as the
plugin fixture already does. A regression waits for the restored connection check
and asserts one actual account-session read across restoration; it first observed
two. The corrected isolated scenario passes while retaining all original draft
and swipe assertions. Complete final-source Android qualification remains pending.


## Agent selectors use compact metadata and selected schemas

Both clients now read every compact package, instance-access and saved-rule-count
page for agent summaries and whole-instance selectors. The directory has its own
DTOs rather than pretending that metadata is a complete manifest or instance
configuration. Disabled or unavailable held grants remain visible, including
saved rules whose resource or instance has disappeared. Counts describe saved
rules and never grant execution authority. Package-owned nouns and access help
replace the previous 1Password-specific selector presentation.

Resource/profile editors fetch only the chosen package schema and projected
resource choices. A failed supported read stays an error with an explicit retry;
only an older host takes the complete-catalogue adapter. Closing or changing the
selected instance discards an obsolete schema, while refreshing only saved-rule
counts preserves an unchanged open editor. Grant writes retain their existing
host checks. The complete-read measurement and its limitations are recorded in
[selector measurements](PLUGIN-SELECTOR-MEASUREMENTS.md).

The shared management catalogue adds five read operations for these selectors:
`list_plugin_packages`, `get_plugin_package`, `list_plugin_instance_access`,
`list_plugin_agent_grant_counts` and `list_plugin_resource_choices`. Public MCP
requires read scope; the installation tool rechecks its current execution grant.
MCP pages retain room for both the text and structured result copies. The HTTP
package/version reads decode individual URL components before validating the
registry's path domain; encoded separators and traversal remain refused.

These reads do not complete the new UI/public-MCP/installation-tool parity gate.
Owner and personal effects, configuration, resources, routes, participant controls
and OAuth still need equivalent management operations with their original
identity and revocation checks. In particular, existing installation-agent
restrictions on granting owner secrets or exclusive reservations are preserved.
The Slack package, transport/files, remaining provider lifecycle, wake delivery,
tools, automations, transient search and complete final qualification remain open.

## Integration with generic provider conditions and device sign-in

Dormant condition probes retain the original requester's membership generation,
run, snapshot and exact wait. They use a dedicated observation proof rather than
a running agent bearer. A revoked requester cannot send another provider request
or receive a provider result. Grouped probes discard a result whose representative
wait changed during I/O; unchanged peer conversations remain eligible for their
own subsequent observation. This does not grant external Slack turns access to
a shared provider credential.

Generic device-code sign-in captures the owner generation and approved package.
Status, token use and provider polling recheck that proof, including after I/O.
Configuration carries the completed flow identifier instead of exposing its token.
Agent merging refuses unresolved resource routes and granular grants, including
inactive records. The owner must reassign routes and explicitly resolve access
through normal approvals before retrying; merging cannot infer or widen resource
rights from ordinary agent access.

These adaptations preserve the newer generic provider interfaces from main; they
do not deliver the Slack package, token lifecycle, socket primitives or remaining
UI/public-MCP/installation-tool management parity. PR #226 remains a draft.

## Main credential OAuth integration: remaining configuration gates

The composed branch preserves main's credential OAuth, Gmail and transfer APIs
alongside the resource and personal OAuth model. The demonstrated Slack HTTP
journey uses a root signing secret and a workspace bot secret; it does not prove
root `credential.oauth` combined with inert setup.

That generic combination remains unfinished in web, Android and both tool
interfaces. The native setup API now carries owner-bound root OAuth consent,
retains it through revision-checked saving, and distinguishes the setup ID from
the active connection namespace during activation. Configuration forms use
serialized renewal for saved and inert roots. Client/tool integration is still
required; this does not establish a complete user-facing OAuth setup journey.

These are unresolved integration gates, together with the documented equivalent
public-MCP/installation-tool configuration operations. They must be completed
before final delivery and merge; a green Slack fixture or CI checkpoint does not
waive them. Current Slack work continues with concrete protocol and conversation
features under the accepted Q1–Q20 scope. The PR remains a draft, and no live Slack
workspace or production OAuth exchange has qualified these combinations.

### Native root consent before activation

The owner setup API can start root authorization-code OAuth after saving the
draft's client settings. The common callback keeps its account binding and PKCE,
and additionally retains the exact setup snapshot and package approval before
and after token I/O. A completed flow can be saved only to that draft and
revision; active-account flows and other setup roots cannot consume it. Successful
saving consumes the in-memory flow and writes the existing encrypted credential
key with the original setup expiry. Changing client, scope settings or secret
retires the previous consent. Pasted OAuth tokens are refused.

Draft forms and activation share serialized root renewal, including committed
refresh consumption before provider I/O and exact credential/approval/setup
fences afterward. Intent and completion retain the draft's original expiration;
uncertain refresh requires new consent and prevents activation. Required fields
which the form derives after authentication do not block starting consent; final
activation still requires them. Preparation uses the explicit setup namespace
rather than looking for a live connection under the same ID.

Independent review found concurrent activation could promote a consumed refresh
started by another form after preparation. The actual provider-gated regression
reproduced HTTP 200 instead of conflict; activation now checks the root's consumed
flag inside its transaction. The draft remains available and activation succeeds
after the held renewal confirms. A separately prepared earlier-draft `{token}`
record also reproduced activation without OAuth consent; such root OAuth drafts
now require reconnection, preserving their metadata and other configuration.

Native regressions use actual owner HTTP, the real Luau worker and synthetic
provider responses. They cover consent/save/form/activation, cross-target refusal,
late edit/delete/owner/package changes, expired renewal, uncertain response and
secret expiration. Prepared expiry uses the real credential obtained by code
exchange. [Storage observations](performance/SETUP-OAUTH-EXPIRATION.md) compare
the same encrypted value with and without a KV deadline. Client sign-in surfaces,
both tool mutation interfaces and the complete Slack OAuth lifecycle remain open;
this API evidence is not a complete product or live Slack qualification.


## First Socket Mode exchange and measured protocol blockers

The Luau package now selects signed HTTP or Socket Mode from its existing generic
form. Its Socket worker obtains an ephemeral URL with a distinct app-level
`connections:write` credential, verifies Hello's app ID, and shares HTTP's exact
mention/resource/native requester admission path. Tests drive the actual manager,
transport/event workers and native adapter, checking the same restrictive agent,
original person and exact reply thread. Replaying the message with another envelope
ID starts no extra work. This is fixture evidence, without a live Slack workspace.

A held conversations.info fixture reproduced two concrete failures: peer Ping
stalled behind the Luau handler, and an ACK was sent after Slack's three-second
window. The native I/O supervisor now runs independently of sequential handlers,
with 64 command slots, 128 event slots and a 32 MiB installation-wide encoded-payload
budget (additional wire/IPC/value overhead remains). Optional eventTimeout limits
message handling from receipt, including queue time; Slack requests 2500 ms. Expiry
stops that worker/socket, leaves the envelope unacknowledged and preserves any
already committed work's native deduplication. With metadata held beyond 3 seconds,
no request is imported and no native turn starts.

The initial five Socket journeys pass, including real WSS with an explicit
synthetic CA and ordinary certificate/name validation; an untrusted CA fails
before envelope handling. Earlier plain WS evidence was not TLS evidence. The
provider/model/controller remain synthetic, without VM isolation or production
storage evidence. Full Slack/Discord regressions, fresh checkpoint review and
exact-head CI qualification remain required. Socket lifecycle/overlap/active
liveness (addressed by the later health-check checkpoint below) and app quota/restart handling, all other Q1–Q20 lots and the three R72
integration findings remain open before final delivery or merge.


### Socket and capacity checkpoint follow-through

The first Socket exchange uses app-level connection authorization, a verified app
Hello, durable admission and the same requester-bound turn/reply path as HTTP.
Its actual-worker fixtures also exercise verified WSS, refusal of an untrusted
fixture CA, peer Ping/Pong while metadata is held and cancellation of overdue
metadata before a late ACK/import. No live Slack service was used.

Review reproduced two additional failures: the first interrupted connection-URL
response had no recovery timer, and native pending-work saturation deleted an
already-ACKed event after five failed handler attempts. The package now retains a
startup recovery path. A new optional inbound work hint preflights fresh Slack
requests while preserving controls; accepted work waits outside the finite error
budget and still checks its current source/requester authority. Tests must prove
busy admission leaves no duplicate marker, six saturated scheduler wakeups retain
the request, releasing capacity produces the actual answer, submitted receipts
remain usable while full, and Slack reset/Discord textual stop still work. These
are checkpoint assertions, not completion of Socket lifecycle or Q1–Q20.


### Established Socket health and resumed exchange

A verified-WSS actual-worker fixture first reproduced a successful first answer
followed by a silent application peer: its TCP socket stayed open, but it stopped
reading or answering WebSocket control frames without Close or FIN/RST. No
replacement appeared within 14 seconds. This is not full network packet-loss
qualification; the kernel can still acknowledge TCP traffic.

The package now requests generic RFC 6455 health monitoring from the independent
I/O supervisor, with 5,000 ms client Ping/matching-Pong deadlines and a separate
30,000 ms peer-Ping deadline. The host supplies a fresh nonce and starts the Pong
deadline after flushing the Ping; unrelated and expired replies cannot renew it.
The package chooses the policy and obtains a new temporary URL after failure.
No timers/nonces are persisted; checks belong only to the current socket. Existing
packages without this option retain their existing network behavior.

Four WSS journeys cover blackholes, unrelated Pongs, absent peer Pings while
client Pongs succeed, and a healthy connection across 30 seconds with both
control-frame directions. The first three produce a second actual requester-bound
turn/answer in the same Cairn conversation and Slack thread, with replay deduplication.
A follow-up resumes its settled Cairn run; two controller plans prove the turns,
rather than incorrectly requiring two database runs. The initial test's short-final
marker deliberately produced a constant fixture answer and was removed before
asserting the echoed next request. These fixture assertion corrections are not
production regressions. All twelve Socket cases pass without retries on the
corrected fixture; synthetic provider/model/controller and TLS trust limits remain
those described in SLACK.md. No real Slack service has been used.

This advances established-connection health, not full protocol or Q1–Q20 delivery.
Reason-specific rotation/overlap, connection/source ACK identity, app quotas and
restart behavior remain required, alongside all accepted Slack features and final
OAuth/tool-parity/security/compatibility/client-review/CI gates. The three existing
generic OAuth/tool-parity integration gaps remain unresolved; the PR stays draft.


### Metadata read errors versus uncertain writes

A fresh independent review found that conversations.info JSON `ok:false` with
HTTP200 became a permanent403, causing a Socket ACK without admission. Actual
package fixtures reproduced that ACK, the HTTP403 instead of503, a signed reset
refusal, and consumption of a queued answer's error budget. An initial HTTP
fixture had invalid route limits; that setup failure was corrected before using
its403/503 result as regression evidence.

The Lua HTTP helper classifies documented transient platform errors only for GET
reads. Definite JSON ratelimited/rate_limited refusals follow existing429
cooldown/Retry-After logic for reads and writes; other transient reads use503 and
retain accepted replies through the existing retryAfter
path. This introduces no persisted fields or storage API; the previously measured
queue/checkpoint shapes and retry path are reused. Six forced scheduler wakeups
retain the pending actual answer without fabricating completion, then permit its
real adapter/provider-process output. Tests separately preserve definite access
denials and uncertain POST handling: a recorded post followed by HTTP200
internal_error remains unreplayed after a real manager restart. All four focused
transient cases and the signed reset journey pass on the rebuilt package.

A second independent review reproduced a distinct write case: HTTP200 JSON
ratelimited/rate_limited on a later answer chunk left its durable state as sending,
so no further chunk arrived. Both actual-worker fixtures failed before the
correction. The classifier now recognizes only those explicit throttle rejections
for mutations, reusing the existing cooldown and retryable chunk checkpoint.
The fixtures require the entire ordered answer without repeated earlier parts
and a provider attempt delayed by Retry-After. Internal-error write outcomes
remain uncertain; their restart guard must continue to refuse a second post.

Full Q1–Q20 remains incomplete, including questions/stop, broader conversation
inputs, provider OAuth, tools/automations/RTS and the remaining Socket lifecycle.
The management-parity and older root OAuth setup/refresh integration findings
remain mandatory final gates. This checkpoint does not authorize merge/release.

### Ordinary progress and requester-bound Stop

The next slice reuses the existing generic native `progress` callback and
delivery-owned storage. Luau waits at least eight seconds from its first callback
before posting ordinary working status in the exact source thread. Queued work
does not publish a working status; an issued question suppresses duplicate
progress. Only the native short step labels leave Cairn, never tool arguments,
model output or reasoning. A changed step edits the confirmed bot message through
[`chat.update`](https://docs.slack.dev/reference/methods/chat.update/), explicitly
providing text and blocks. This is periodic progress, not agent-session streaming.

The issued Stop carries a delivery reference. Its binding includes original
author, resource, workspace, channel, thread and confirmed message timestamp;
question actions cannot use a progress reference. Native execution checks are
repeated after consumer-side provider visibility checks, retaining revocation and
current-turn fences after fast HTTP/WSS acknowledgement. Cancellation reaches the
actual provider process and its final answer returns to the source thread.

Write intent precedes provider I/O. Definite quota refusals reuse the existing
method cooldown and channel pacing; uncertain creation is not repeated across
restart. An uncertain edit keeps the confirmed Stop usable and suppresses further
active edits. Final handling deletes the reference before one optional terminal
edit without controls, then delivers the final answer even if cleanup fails.
Uncertain cleanup can leave a visibly stale but inert control. Native terminal,
revocation and purge removal remain the authoritative lifetime boundary.

Actual-worker HTTP/WSS fixtures exercise the hanging provider, step transition,
real cancellation, Bob/forged-message/forged-answer refusal, post-ACK visibility
and revocation, uncertain edits/cleanup, definite creation quotas and uncertain
creation through manager restart. These use simulated Slack/model/controller
boundaries, not a real workspace. The [storage observation](performance/SLACK-PROGRESS-STATE.md)
records the representative progress value and its lifetime cost. No native API,
web/Android UI capability or new persisted envelope is added; the inventory stays
within the unqualified epoch-12 / SQLite-11 boundary. Full final review/validation,
live Slack qualification and all remaining Q1–Q20 gates still apply.

### Root OAuth renewal in resource configuration

The actual owner HTTP/Luau regression reproduced a resource form using an expired
root token without invoking renewal. Saved roots now use the existing serialized
configuration refresh helper, preserving paused-instance editing. Concurrent
previews share one refresh and receive only provider data, never credentials.
Inert roots now use the same renewal fences. Their native root OAuth consent and
activation API is described below; client/tool integration remains mandatory.

The next fixture reproduced late writes after owner revocation and package
approval replacement: the form response failed but its rotated token had already
been published. Root previews, save-time validation and resource forms now carry
the captured owner grant into renewal. The helper rechecks it after provider I/O,
then revalidates installed files/approval after the final remote authority lookup.
Approval and instance identity are fenced again in the vault-write transaction.
Fresh review also found adoption of a replacement approval before the helper's
own snapshot. A provider owner-policy gate reproduced that race before token I/O;
the helper now receives the original `ApprovedPackage` from each configuration
caller, including root saving and setup preparation, rather than loading a new
configuration approval. The stale form cannot consume or renew under a successor.
Fixtures hold the real token endpoint response while changing owner access,
editing/removing the root or replacing approval; stale work cannot publish tokens
or send the downstream resource request. Provider-side token consumption cannot
be rolled back; interrupted or refused renewal may require reconnection.

The inherited root engine also repeated a refresh after an interrupted provider
response. The public form regression first observed a second successful request
where reconnection was required. Renewal now commits consumed state before the
POST, removes the replayable refresh token, and compares exact previous state plus
approval/instance/management authority in both consumption and completion writes.
The owner/approval checks repeat immediately before the POST and final write.
The final remote lookup is followed by a read of the exact instance and consumed
credential under their authorization snapshot; deletion or revision changes
during that lookup cannot send an old token. Root preview/save callers retain the
connection captured by their original Target instead of reloading a successor.
Both races were reproduced separately at public owner-policy HTTP gates before
the correction, requiring zero token POSTs from the stale request.
Uncertainty remains quarantined across a fresh native Service/store reopening;
new code consent and ordinary save restore a working form. This is not a process
restart or physical recovery experiment. Late authority changes can retain a
consumed intent but cannot publish new tokens; deleting the root removes it.

The [storage observation](performance/ROOT-FORM-CREDENTIAL-RENEWAL.md) compares
actual pre/post-renewal encrypted records using local native reads/writes. The
existing vault envelope and key count are reused; the optional consumed metadata
is inventoried inside the unreleased epoch-12 / SQLite-11 boundary. This repairs
foundation gaps; it does not complete Slack OAuth,
root setup OAuth, management-tool parity or the accepted Q1–Q20 contract.

## Guided setup integration compatibility boundary

Main `d81a3a87` uses epoch 12 / SQLite 11 for guided setup declarations. The
unreleased Slack branch therefore raises its earlier 12/11 reservation to
**epoch 13 / SQLite 12**. Earlier checkpoint evidence above describes the source
then tested; it does not qualify this integrated tree or a production transition.
The retained Main migration plan remains documentation only for this draft;
there is no automatic migration declaration to epoch 13. Full qualification and
forward recovery remain required before delivery.

The merge uses the same root-delegation predicate for setup derivation/validation,
package cards and connection saves. Resource-routed packages keep participants
and agent selection on their routes; editing the root does not require an
app-wide answering agent. The shared Rust/web/Android corpus covers this case,
and a browser regression exercises the real owner save. Android's narrow
configuration read retains the server-resolved setup instead of requiring the
legacy full catalogue. These integrations do not provide the remaining inert
root OAuth client or management-tool mutation journey.

A default-stack native regression also reproduced overflow while a resource form
renewed its parent OAuth. Synchronous installed-package validation was nested
inside the HTTP/worker/credential call chain. Approval reads now run that same
file loader in the blocking pool, retaining the original approval and later
checks. The uncertain-refresh regression passes with unchanged stack/deadlines;
[local measurements](performance/PACKAGE-APPROVAL-READ.md) record its I/O/CPU
limits. Complete merged-tree validation and fresh review remain separate gates.

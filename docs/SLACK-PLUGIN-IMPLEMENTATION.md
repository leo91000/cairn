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
    authorization. Nonmember self-service linking, configuration drafts, provider revocation/token
    exchange and both client interfaces remain unfinished. This does not complete
    OAuth or the Slack package.
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

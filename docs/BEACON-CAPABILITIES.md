# Beacon account capabilities (B6)

First slice of #421, under #397 / #395: `list_account_methods`. The ticket permits
splitting by complete command across web, Android, public MCP and
`cairn_installation`. This slice converts that command on all four clients. The
inventory below records the remaining commands; it does not claim their conversion.

Campaign decision 3 is **not accepted**. Installation access is not account
access, even when a run's task author is an owner. No account ID, cookie, proof,
confirmation flag or owner boolean is accepted as command input. There is no new
challenge or delegated-proof protocol in this slice. Existing email, OAuth and
passkey protocols remain the only human validation channels. Future challenge
start/status commands must not expose their completion as a proof an execution
can manufacture; their authority design still needs the owner's decision.

## Converted command

`crates/contracts/src/beacon.rs` owns `ListAccountMethodsInput`, the generated
`AccountSignInMethod` and the `ListAccountMethodsOutput` tagged union, descriptor,
HTTP binding and installation denial. This extends B1's static catalogue and
schemars + ts-rs + json-kotlin-schema-codegen pipeline, without a new dispatcher.

| Surface / actor | Execution and result |
| --- | --- |
| Web / Android, authenticated account | Generated call to `GET /api/account/methods`; Beacon verifies the current account session and reads its unremoved methods in one Postgres read snapshot. Returns `listed`. An installation owner and a member both read only their own account. No new recent-proof requirement is imposed on the existing read. |
| Public MCP with a current installation grant, any scopes | The existing Beacon grant authentication and relay remain in force. The shared installation catalogue discovers the command. Its policy returns `accountProofRequired`, with no account read or new grant. It never treats scopes as a Beacon session. |
| `cairn_installation` with a current execution grant | The existing run/message/permission checks remain in force, before discovery and invocation. The same contract returns `accountProofRequired`, with no call to Beacon and no local account replica. |
| Missing, expired or revoked transport credential | Existing unauthorized error, before invocation. No method list is returned. |

The tools' typed refusal is a result variant, within the existing MCP code-mode
`structuredContent.result` envelope. Invalid command input remains a protocol
error. Neither tool can obtain the `listed` variant under its current authority.
The HTTP success now includes `kind: "listed"`; both apps consume that contract.
The wire fields for each method remain `id`, `kind`, `label`, never a credential.

Beacon account routes always use the account connection. They are not installation
routes and never switch onto the direct DataChannel. An offline installation does
not prevent an authenticated app from listing account methods. Public MCP retains
its existing installation availability requirement: while offline, the relay
returns its existing unavailable error before an operation can execute. Neither
transport fallback nor reconnection converts an installation grant into a session.

## Existing action inventory

Reference: main `40349271` (includes B1 #435 and legacy removal #394). Paths below
are HTTP adapters, not a second registry. Each row identifies an explicit server
action, its handler and its present authority. All unmarked rows remain to be
converted in later slices of #421, with generated clients and tool refusals/proofs.
`A` means an authenticated Beacon account, `P` a recent human proof on that same
session, `O` the installation's current owner, and `B` a bootstrap protocol proof.
These are existing requirements, not proposed delegated rights.

| Action / route under `/api` | Existing handler | Authority / stage |
| --- | --- | --- |
| POST account/email-code | request_code | Bootstrap; email challenge issuance, rate limited; also used by authenticated confirmation |
| POST account/verify | verify_code | B: email code + browser challenge; login or method enable |
| POST account/reauth/email | reauthenticate_email | A + email challenge/code; updates only that session's proof age |
| GET account/session | session | Bootstrap session status, unauthenticated result permitted |
| POST account/logout | logout | A + CSRF; current session only |
| GET account/options | oauth::options | Bootstrap provider availability |
| POST account/oauth/{provider}/start | oauth::start | Bootstrap login; A + P when linking |
| GET account/oauth/{provider}/callback | oauth::callback | B: bound browser OAuth state/code, not an agent assertion |
| POST account/oauth/google/callback | oauth::google_credential | B: verified native Google credential |
| GET account/oauth/{name}/native/browser | oauth::native_browser | B: single-use native launch token |
| POST account/oauth/{name}/native/finish | oauth::native_finish | B: native handover secret/status; linking keeps the original session |
| POST account/oauth/{name}/native/confirm | oauth::native_confirm | B: verified native linking confirmation |
| POST account/passkeys/login/start | passkeys::login_start | Bootstrap challenge |
| POST account/passkeys/login/finish | passkeys::login_finish | B: signed WebAuthn response and bound challenge |
| POST account/passkeys/reauth/start | passkeys::reauth_start | A: challenge for this account |
| POST account/passkeys/reauth/finish | passkeys::reauth_finish | A + B: signed response; proof for this session |
| POST account/passkeys/register/start | passkeys::register_start | A + P: registration challenge |
| POST account/passkeys/register/finish | passkeys::register_finish | A + P + B: validated registration |
| **GET account/methods** | **capabilities::LIST_ACCOUNT_METHODS via methods::list** | **Converted: A, generated contract; tools return typed refusal** |
| POST account/methods/remove | methods::remove | A + P; retain at least one method |
| GET account/sessions | account::sessions | A: own sessions, no bearer/digest |
| DELETE account/sessions/{session} | account::revoke_session | A; P unless revoking current session |
| POST account/sessions/revoke-others | account::revoke_others | A + P |
| POST account/delete | account::delete | A + P + matching account email |
| GET account/audit | audit::list | A: own account audit |
| GET installations | installations::status | A: accessible installations and status |
| POST installations/claim-code | installations::claim_code | A + CSRF: creates claim challenge |
| POST installations/device-claim/preview | installations::preview_device | A: inspect supplied device claim code |
| POST installations/device-claim | installations::approve_device | A: approve pending device claim |
| PATCH installations/{installation} | installations::rename | A + O |
| DELETE installations/{installation} | installations::forget | A + O + P: irreversible revoke/forget |
| POST installations/{installation}/detach | installations::detach | A + O + P: detach |
| GET account/invitations | sharing::pending | A: invitations for own email |
| POST account/invitations/{invitation}/accept | sharing::accept | A: matching verified email |
| GET installations/{installation}/sharing | sharing::list | A + O |
| POST installations/{installation}/sharing/invitations | sharing::invite | A + O + P |
| DELETE installations/{installation}/sharing/invitations/{invitation} | sharing::cancel | A + O |
| DELETE installations/{installation}/sharing/members/{member} | sharing::remove | A + O + P |
| DELETE installations/{installation}/sharing/membership | sharing::leave | A: current member, never owner |
| GET installations/{installation}/tokens | mcp::list | A + O: grant metadata, no tokens |
| POST installations/{installation}/tokens | mcp::personal | A + O + P: issue installation MCP grant |
| DELETE installations/{installation}/tokens/{grant} | mcp::revoke | A + O |
| POST mcp/oauth/preview | mcp::preview | A: preview bound OAuth request |
| POST mcp/oauth/consent | mcp::consent | A + O + P: accept bound OAuth request |
| GET account/notifications | notifications::configuration | A: Web Push configuration |
| GET account/notifications/android | notifications::android_configuration | A: Android Push configuration |
| POST account/notifications/subscriptions | notifications::subscribe | A + P, including renewal |
| POST account/notifications/android | notifications::subscribe_android | A; same registration proof rule |
| GET account/notifications/subscriptions/{id} | notifications::registered | A: registration belongs to current account |
| DELETE account/notifications/subscriptions/{id} | notifications::unsubscribe | A: owned registration |

The external MCP OAuth endpoints `/oauth/register`, `/oauth/authorize`,
`/oauth/token` and `/oauth/revoke` retain their client-registration, browser-consent,
PKCE/code/refresh-token and revocation proofs. Discovery metadata is public.
They are protocol adapters, not evidence of permission to issue an account grant.

Installation-machine claim/start/poll/rotation (`/api/relay/claim`,
`/api/relay/device-claim/start`, `/api/relay/device-claim/poll`, token rotation), internal
membership/author reconciliation, direct signalling, relay frames and notification
delivery are machine/transport protocols. They are not account UI commands and
are not newly exposed. Telemetry read/write is a separate diagnostics capability.
No UI-only local operation (menus, language, sorting) needs a server capability.

## Follow-up boundary

Continue by complete command or cohesive family: session management; method
mutation and login bootstrap/challenge start/status; claim and installation
lifecycle; invitations/members; MCP authorization; notification devices. Preserve
current proof binding and transaction revalidation. The inventory is the starting
point, not permission to enable these actions for an installation grant. No new
proof issuer, account persistence change, historical migration, or visual redesign
is part of this slice. Account ownership and membership remain authoritative only
in Beacon Postgres.

Contract qualification uses `tests/fixtures/account-methods-contracts.json` in
Rust, TS/JSON Schema and generated Kotlin. Existing Beacon account/security,
sharing, installation, MCP and relay suites exercise the unchanged authority.
Browser account/provider journeys and shared native account cases cover the two
apps. The focused installation regression covers shared discovery, refusal,
caller-supplied proofs and revoked execution grants; the public Beacon regression
covers the real grant/relay path, including revoked public access.

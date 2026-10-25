# Beacon account actions

The [owner arbitration for campaign #395](https://github.com/leo91000/cairn/issues/395#issuecomment-6401632363)
closes B6 (#421): Cairn account actions managed by Beacon (sessions, sign-in
methods, invitations, members, MCP authorizations and notification devices) stay
in the web and Android apps, with web/Android parity. They are excluded from the
public MCP and `cairn_installation`: installation access does not confer authority
over a person's Cairn account. No delegated account-proof protocol is planned by B6.
This supersedes the agent exposure and follow-up plan introduced by #436.

## Shared app read

`crates/contracts/src/beacon.rs` retains the generated Rust/TS/Kotlin method models
and response used by both apps. Generated calls use `GET /api/account/methods`;
`methods::list` authenticates the Beacon session and reads its unremoved methods
in one Postgres snapshot. The response is `{methods: [...]}`; the agent-only
`accountProofRequired` variant, empty command input and tool descriptor are removed.
Missing or revoked sessions retain the existing HTTP unauthorized response.

Account reads use the Beacon connection, including while an installation is
offline. Direct/relay installation transport supplies no account identity. Both
tool catalogues omit `list_account_methods` and reject invocation as an unknown
operation, including under a current owner-issued installation grant.

## Existing action inventory

Paths below are Beacon HTTP adapters, not an agent capability registry. They
record existing account authority; B6 requires no further conversion into agent
commands. `A` means an authenticated Beacon account, `P` a recent human proof on that same
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
| GET account/methods | methods::list | A: generated app contract |
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
`/oauth/token` and `/oauth/revoke` retain their existing protocol proofs. Account
ownership and membership remain authoritative only in Beacon Postgres. Machine
claim/rotation, relay and direct signalling remain transport protocols; this
boundary changes none of their behavior or other installation capabilities.

## Validation

The shared account corpus qualifies app responses in Rust, TS/JSON Schema and
Kotlin. Beacon account/security tests cover different accounts, revoked sessions,
removed methods and offline installations. Tool regressions check absence from
both catalogues and unknown-operation rejection; the public Beacon regression
also exercises a real grant and relay. Browser account/provider journeys and
`NativeAccountCases` cover the two apps.

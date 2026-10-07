# Official service and Leo accounts

The `leo-official` binary is distinct from the installation's `leo` binary. It
owns a separate Postgres database and serves the web application. This first
account slice provides email, Google, GitHub and web passkey sign-in, management
of sign-in methods, and an empty installation screen. Installation access,
claiming, sharing and Android sign-in are separate tickets.

## Development

```sh
pnpm install --frozen-lockfile
pnpm build
docker compose -f deploy/official/compose.yaml up
```

Open http://localhost:4311. The development-only mailbox at
http://localhost:8025 shows the emails in memory; it never sends real email or
logs codes. Both ports are bound to loopback. The first Rust compilation takes
a few minutes. `docker compose -f deploy/official/compose.yaml down` stops it;
add `-v` to discard the development database and build caches.

## Production

Use the `official` target in `Dockerfile` and
`deploy/official/compose.production.yaml`. The image bundles the official binary
and matching web output, runs as UID 1000, and requires environment-only secrets.
The existing `deploy/official/compose.yaml` remains development-only.
Follow [the cairn.build runbook](PRODUCTION-CAIRN.md) for the fresh same-server
installation, proxy trust, email, OAuth, approval gates and Postgres rollback.
`/health` returns no-store JSON (`status`, `commit`, `runtimeId`) and verifies
Postgres availability; deployment checks its exact validated build identity.

## Configuration

Production runs `cargo build --locked --release --bin leo-official`, then the
binary with the `dist/` produced by `pnpm build`. Configure the following env
variables through the operator's secret management:

| Variable | Meaning |
| --- | --- |
| `LEO_OFFICIAL_DATABASE_URL` | Required Postgres URL, separate from installation SQLite |
| `LEO_OFFICIAL_ORIGIN` | Required browser origin, HTTPS except localhost/loopback development; no path, query, or fragment |
| `LEO_OFFICIAL_LISTEN` | Bind address, default `127.0.0.1:4311` |
| `LEO_OFFICIAL_TRUSTED_PROXIES` | Comma-separated proxy IPs/CIDRs; empty by default |
| `LEO_OFFICIAL_WEB_DIR` | Frontend output directory, default `dist` |
| `LEO_INSTALLATION_IMAGE` | Tested immutable `ghcr.io/leo91000/leo-agent-manager@sha256:<64 lowercase hex>` approved for new and automatic installation updates; unset returns 503 from `/install/release` |
| `LEO_OFFICIAL_EMAIL_FROM` | Required sender address on a verified email domain |
| `LEO_OFFICIAL_EMAIL_KEY` | Required email provider bearer key; never logged |
| `LEO_OFFICIAL_EMAIL_ENDPOINT` | Default `https://api.resend.com/emails`; override only for the loopback development setup |
| `LEO_OFFICIAL_GOOGLE_CLIENT_ID`, `LEO_OFFICIAL_GOOGLE_CLIENT_SECRET` | Optional pair, enables Google sign-in |
| `LEO_OFFICIAL_GITHUB_CLIENT_ID`, `LEO_OFFICIAL_GITHUB_CLIENT_SECRET` | Optional pair, enables GitHub sign-in |

The production adapter uses [Resend's send-email contract](https://resend.com/docs/api-reference/emails/send-email)
(`from`, `to`, `subject`, `text` over HTTP). `EmailSender` is the replaceable
interface for integration tests and other delivery implementations. Delivery
errors are generic, never provider bodies. No code is returned to the client.
Automatic, checksum-checked database migrations run at startup; the database role
needs migration permissions. All filenames use `YYYYMMDDHHmm` versions. The
upstream SQLx migration macro is imported directly from `sqlx-macros`: the full
`sqlx` facade resolves a conflicting SQLite dependency even in Postgres-only
builds. A build script tracks the directory so adding a migration recompiles it.
This pre-public-deployment renumbering changes versions 1–3 to 202610030001–3.
An old development database must be backed up and its three ledger versions
updated to those timestamps before startup (SQL contents/checksums are unchanged),
or replaced with an empty disposable development database. Do not run the old
binary against the updated ledger. Rate limits are shared through Postgres.

Only **one official relay process** is supported for deployment. Route all browser
account/access mutations, installation API requests and relay WebSockets to that
process. Logout, detachment, member removal, token rotation and **Revoke and forget**
notify only its in-memory relay registry; there is no inter-process revocation
notification. Sticky routing per installation alone does not provide immediate
session revocation across installations. Persisted checks eventually detect
machine revocation in another process, but do not make that deployment supported.
Do not add official replicas until shared connection routing and revocation
notifications are implemented. [ADR-0032](adr/0032-single-official-relay-process.md)
records this restriction of spec #43 and the persisted-check tolerance trade-off.
Terminate TLS at the public origin with a reverse proxy. The service checks the
configured origin on every account mutation. IP limits use the TCP peer unless
it matches `LEO_OFFICIAL_TRUSTED_PROXIES`. In that case they use the rightmost
`X-Forwarded-For` address outside the trusted proxy ranges, across all account
and machine endpoints. Each trusted proxy must append its actual peer address.
Trust only your controlled proxy hops; headers from other peers are ignored.
Missing or malformed trusted suffixes fall back to the TCP peer. Do not expose
Postgres or the development mailbox publicly.

The official SPA denies embedding with `frame-ancestors 'none'` and
`X-Frame-Options: DENY`, including deep links, to protect account and claim actions
from clickjacking. HTTPS official origins also send `Strict-Transport-Security:
max-age=31536000` through the TLS-terminating proxy; loopback HTTP development
does not set HSTS. Relayed responses keep their existing sandbox policies.

Codes expire after 10 minutes and allow five failed verification attempts per
challenge, with a shared ceiling of fifty failures per mailed code. An exhausted
challenge is refused without spending other challenges' attempts. Requesting
another code while an unexpired code still has a global budget returns a fresh
challenge for the same mailed code, without sending another email or resetting
its original expiration or global failures. Consuming the code invalidates every
challenge. This lets the mailbox owner sign in if a third party requested the
code first or exhausted their own challenge, including during the delivery
cooldown or after the address's delivery budget is exhausted.
At fifty failures, the code is unusable for every challenge; the next request must
send a new code within the same per-address delivery budgets. During the cooldown
or after those budgets are exhausted it returns 429, never a challenge for a dead
code. A distributed attacker can still exhaust global codes and the address's
delivery budgets to deny email sign-in temporarily; passkeys and OAuth remain
available. Already issued challenges preserve their consumed attempts during the
per-challenge migration, and code failures count toward the new global ceiling.
Existing codes retain their original challenges when the migration is applied.
Code requests are limited to ten per minute per resolved client. New email deliveries
are limited to one per minute per normalized email;
verification is limited to thirty per minute per resolved client. Each address
can receive at most six codes per hour and twenty per day. These delivery budgets
are committed together under an address lock, so concurrent processes cannot
replace a pending proof or exceed either budget. Limits are atomic in
Postgres and survive process restarts. Expired challenges, sessions, invitations and rate buckets are
removed by an hourly maintenance task, also run once at startup. Expiration is
enforced on reads without waiting for cleanup. Email addresses are trimmed and lowercased.

Sessions expire after seven days. Only their SHA-256 digest is stored. Cookies
are HttpOnly, SameSite=Lax, host-only, Path=/ and Secure on HTTPS. Account
responses use Cache-Control: no-store. Logout requires the session's CSRF token
in `X-CSRF-Token` plus the exact origin, deletes the server session and clears
the cookie. Login also requires the exact origin to prevent login CSRF.

## OAuth and web passkeys

Register a web OAuth client with Google and an OAuth app with GitHub. Register
these exact callback URLs, using the configured `LEO_OFFICIAL_ORIGIN`:

- `/api/account/oauth/google/callback`
- `/api/account/oauth/github/callback`

Configure both client credentials for each enabled provider. An incomplete pair
fails startup; unconfigured providers are hidden on the sign-in screen. The
Compose development setup reads the four optional credential variables from the
operator's environment. Credentials and provider tokens are never returned to
the browser or forwarded to installations. Provider access tokens are discarded
after fetching identity; no refresh tokens are requested or stored.

Google uses `openid email` and the
[verified email from UserInfo](https://developers.google.com/identity/openid-connect/openid-connect).
GitHub requests only `user:email` and uses its
[verified primary email](https://docs.github.com/en/rest/users/emails), ignoring
the public profile's email. GitHub sign-in provides no repository access to
agents. Verified emails are normalized exactly like email-code sign-in and
can create a new Leo account. For an existing account, a new Google identity is
automatically attached only when Google is
[authoritative for the email](https://developers.google.com/identity/sign-in/web/backend-auth):
`@gmail.com`, or a verified Workspace `hd` matching the email's domain. Other
Google emails and GitHub require a current Leo session to link. Once linked,
the identity remains a normal sign-in method. An identity already attached to
a different account is rejected; linking while signed in must match the current
account's verified email. Concurrent first sign-ins reuse the same account and
method without a uniqueness error.

Default endpoints are the official Google and GitHub endpoints. Tests replace
only their HTTP endpoints. Operators can override
`LEO_OFFICIAL_{GOOGLE,GITHUB}_{AUTHORIZATION_URL,TOKEN_URL,USERINFO_URL}` and
`LEO_OFFICIAL_GITHUB_EMAILS_URL`; endpoints require HTTPS, except HTTP loopback
endpoints when the official origin is also loopback. OAuth uses authorization
codes, S256 PKCE and a five-minute, single-use state bound to an HttpOnly browser
cookie. Explicit linking also checks the initiating session and CSRF token. OAuth
cancellation or rejection returns to the sign-in screen with a generic message.
OAuth and passkey browser cookies are cleared after completion or rejection.

Passkeys use [webauthn-rs](https://docs.rs/webauthn-rs/latest/webauthn_rs/)
for signature, origin, relying-party and required user-verification checks. The
relying-party ID is the exact official origin's hostname; changing it invalidates
existing passkeys. Use an HTTPS hostname in production and `http://localhost`
for local passkey development. IP origins still support email/OAuth sign-in but
do not offer passkeys. The web uses the browser's WebAuthn JSON methods; older
browsers receive an unavailable message and can use email or OAuth instead.
Passkey sign-in is discoverable: the browser selects a resident key without an
email lookup. Start options are identical for known, unknown and omitted emails
and contain no credential IDs. Registration requires a resident credential.
Adding a named passkey requires a current session, origin, CSRF token and an
independent email-code or existing passkey proof from the last five minutes in
that session, checked at both registration start and finish. A session alone
cannot enroll an attacker's passkey to manufacture a deletion proof. The Sign-in
methods screen offers Confirm identity before registration; OAuth users can
confirm a code sent to their verified account email. Up to
20 passkeys can be registered per account. Registration and authentication states
stay only in Postgres, expire after five minutes and are consumed once, even on
invalid proofs. Only public credentials are stored. Removing a passkey also
invalidates challenges already issued for it. Removing any sign-in method
requires the same recent independent proof as registration, checked before and
after waiting for the account lock. OAuth alone cannot remove recovery methods.

Sign-in methods are managed from the empty installation screen. Concurrent
removals lock the account and preserve at least one method. Removed email and
OAuth identities remain recorded so an unauthenticated sign-in cannot silently
re-enable them. Re-enable email by confirming a code while signed in; re-link
Google/GitHub while signed in. Removing a method leaves active sessions intact;
sessions can be revoked separately from Account security. OAuth/passkey requests have
persisted rate limits, using the same trusted-proxy resolution as email sign-in.

## Validation

Use a disposable Postgres database that can create schemas. Each Rust test owns
an isolated schema. Use the backend launcher to isolate live agent credentials:

```sh
LEO_OFFICIAL_TEST_DATABASE_URL=postgres://leo:test-only@localhost/leo_official_test pnpm test:backend -p leo-official-service
LEO_OFFICIAL_TEST_DATABASE_URL=postgres://leo:test-only@localhost/leo_official_test pnpm test:backend
cargo build --locked --bin leo-official
LEO_OFFICIAL_TEST_DATABASE_URL=postgres://leo:test-only@localhost/leo_official_test pnpm test:e2e --project=journeys-official-account
```

CI supplies Postgres for both integration and browser tests. Browser tests run
the real official binary and replace only external email delivery with an HTTP
mailbox. They cover email sign-in, invalid codes, empty installations, reload,
responsive widths, sign-out, OAuth linking/removal and passkey registration,
sign-in/removal with Chromium's virtual authenticator. API tests use HTTP OAuth
providers and a software WebAuthn authenticator (a client adapter supplies its
locally retained credential and user handle for discovery), including unverified emails,
replay, explicit re-linking and concurrent removal of the final methods. The installation remains untouched by this slice.

## Installation sharing

An owner opens **Installation options → Share installation** to see members and
pending invitations, invite a verified email address, cancel an invitation or
remove a member. The invitation screen warns that members use the owner's
coding-agent accounts and secrets. Invitations are sent through the existing
email adapter, expire after seven days, and cannot duplicate a pending invitation
or an account that already has access. Delivery failure removes the invitation
so the owner can retry.

The email links to `/?invitations=1`, where a recipient can sign in or create a
Leo account and accept invitations addressed to that verified email. Invitations
are also available from the installation options menu, including for people who
already have an installation. A member can choose **Leave installation** and
confirm; returning requires a new invitation.

The official HTTP routes are:

- `GET /api/account/invitations` and `POST /api/account/invitations/{id}/accept`.
- `GET /api/installations/{id}/sharing` for the owner's member/invitation list.
- `POST /api/installations/{id}/sharing/invitations` with `{ "email": "…" }`.
- `DELETE /api/installations/{id}/sharing/invitations/{invitation}` to cancel.
- `DELETE /api/installations/{id}/sharing/members/{account}` to remove a member.
- `DELETE /api/installations/{id}/sharing/membership` to leave.

Mutations require the official session, origin and CSRF token. Accepting an
invitation and changing membership serialize on the installation record.
Removal and departure call the existing relay revocation mechanism after commit:
open chat/run streams close immediately, the next request is denied, and the
installation's runs and other accounts remain unaffected. The installation
receives the official account and role through its existing trusted identity.

Members can read agents, projects and skills, and create conversations and work
with agents. The installation refuses management of agents, projects, skills,
nodes, coding-agent accounts, connections, secrets and settings. The web hides
these actions and owner-only pages, skips owner-only requests in shared views,
and offers skills for reading only.

## Public deliverables and MCP access

Public deliverable URLs are `/api/public/installations/{id}/artifacts/{token}`
on the official origin. The installation retains file bytes, share-token
verification and revocation. The public request carries no account identity,
cookie or bearer credential through the relay, and permits only GET/HEAD of
that file. Recipients need no session. Revoking visibility or moving a
conversation to trash invalidates the link; republication creates a new link.
An offline installation returns 503 with an explicit offline message.

The official service imposes no-store, nosniff, a sandbox CSP, no-referrer and
noindex headers on public responses, including errors. Headers from an
installation cannot relax this policy. Downloads and byte ranges use the
anonymous CORS policy (`Access-Control-Allow-Origin: *`, without credentials) and
existing credit-controlled relay streams, including files larger than a
finite relay frame. The service never persists their contents.

External MCP clients use the official `/mcp` URL. Discovery is at
`/.well-known/oauth-protected-resource/mcp` and
`/.well-known/oauth-authorization-server`; `/oauth/register` registers public
clients. Authorization uses the Leo account session, an explicit installation
choice and S256 PKCE. Codes expire after five minutes; access tokens after an
hour. Refresh tokens rotate, expire after 30 days and revoke their entire
authorization when a used token is replayed. A resource parameter, when supplied,
must match the official MCP URL. Only credential digests are retained.

Create, list and revoke grants at `/api/installations/{id}/tokens` (DELETE
`/{grant}`), using the official session, origin and CSRF rules. Personal tokens
last 30 days and are returned once. Every credential is pinned to that
installation and its read/run/manage scopes; it stops working after revocation,
detachment or account deletion. Installation management permissions remain
reserved for its owner. Use Settings in the current installation to manage them.

Public file downloads have four dedicated relay slots per installation, separate
from authenticated API and live streams. Idle downloads are cancelled after 30
seconds without file progress, independently of downstream reads. HTML and SVG
are always served as attachments, and Content-Length is preserved for downloads,
byte ranges and HEAD responses.
The public file route also allows at most 30 requests per minute per TCP peer
across installations, using the existing persisted rate-limit module. Forwarded
IP headers supplied by clients are ignored; a reverse proxy shares this limit.
Authenticated account routes use their own quotas.
Used authorization code digests remain linked to the issued grant. A replay
with the matching client, redirect and PKCE proof revokes the whole token family,
including rotated refresh tokens. Revocation removes the consumed code too.

MCP calls are limited to 120 requests per minute per grant using persisted rate
buckets. Each grant admits at most four simultaneous uploads or remote requests
in the relay process; refresh rotation shares the same allowance. Excess calls
return 429 before reading their bodies. Bodies are limited to 8 MB and ten
seconds total, and are read before reserving installation capacity. The service
rechecks the credential after upload completion and again before dispatch, so
revocation cannot leave slow uploads occupying the owner's relay slots. An
upload timeout returns 408 and releases admission. Admitted remote work retains
its grant permit until the installation responds, even if the caller disconnects.

## Account security and audit (#59)

**Account security** is available on the welcome screen and in Installation
options. Active sessions show the browser's unverified device description
(bounded to 256 characters, with control and Unicode bidi characters removed), sign-in date and expiration. Their public IDs are
independent of bearer digests and CSRF tokens. Expired sessions are hidden.
`GET /api/account/sessions` returns these records; `DELETE
/api/account/sessions/{id}` revokes one, and `POST
/api/account/sessions/revoke-others` preserves the caller while revoking every
other device. Revoking the current session also clears its cookie. Mutations
require the exact origin and CSRF, are limited to 30 per minute per account,
and close that session's relay bodies before returning. Other sessions and
already admitted agent work continue. Session creation and expiry remain the
same for email, OAuth and passkeys; migrations preserve existing sessions.

Deletion requires typing the verified account email and confirmation in the
web. `POST /api/account/delete` accepts `{ "email": "…" }`, uses the caller's
identity, and requires origin and CSRF (five attempts per minute per account).
In one transaction it detaches owned installations, clears their sharing,
MCP grants, authorization codes and device claims, removes the account's other
memberships, invitations, pending email proofs, sign-in methods and sessions.
Every affected tunnel or member/session body is closed immediately after commit.
No request deletes installation data or stops admitted work. Owned installations
remain unclaimed under their existing IDs; run `leo claim` on each machine and
restart its manager to recover access. Members never inherit ownership.
Deleting a member preserves other people's installations and sessions.
Account deletion, installation detachment, definitive installation revocation,
sign-in method removal, invitations, member removal, personal token creation,
MCP consent approval and revocation of other devices (individually or together)
require an email-code or user-verified passkey proof from the last **five minutes**, in the **calling session**. A fresh email
or passkey sign-in qualifies; OAuth sign-in alone does not. Sessions issued before
the additive migration remain usable but have no qualifying proof. The web asks
for an explicit confirmation before enabling deletion, including after a reload
or cancelling and reopening the form. Reconfirm if the five-minute window expires.

The proof grants a **reusable five-minute window**, not a one-use authorization:
several sensitive actions can use it in that session until it expires. The email
code or WebAuthn challenge itself is still consumed once. `web_sessions.last_proof_at`
records the last independent email/passkey proof, not an OAuth login or session
activity. A new rename migration preserves existing proof timestamps,
CSRF, cookie digests and deadlines, leaving historical SQLx checksums intact.
The rename is a forward-only schema change, not an additive compatibility change.
Update the official binary with this migration; older binaries still using the
previous column name cannot serve that database. Upgrade its web bundle too.

The installation actions expose **Confirm identity** in their confirmation form;
after verification the app returns to that form without performing the action.
Sign-in methods offers the same control for both registration and removal,
including email confirmation on browsers without WebAuthn support. Sharing,
personal token creation, MCP consent and active sessions offer it too. The forms
preserve invitation addresses, token names/permissions and the selected
installation, and verification never automatically repeats an operation.
Signing out the current device, denying consent, cancelling an invitation,
leaving an installation and revoking an existing grant do not require this proof.

`POST /api/account/reauth/email` accepts the existing email-code challenge and
code, using the normal delivery and verification limits. It checks the proof's
address against the caller's account, consumes the code once, and confirms only
this session. It neither enables a removed email sign-in method nor issues a new
session. An OAuth-only account can still prove control of its verified mailbox.
The same delivery cooldown applies when confirming just after email sign-in.
`POST /api/account/passkeys/reauth/start` and `/finish` use the existing WebAuthn
implementation, with a single-use challenge bound to the current account/session.
A passkey belonging to a different account cannot confirm the caller. Origin,
CSRF, current session and current credential checks remain mandatory. Confirmation
preserves the session bearer, CSRF and expiration; other devices are unaffected.
Sensitive actions return 403 without a recent proof before taking account/access
locks. Unconfirmed attempts do not spend the operation's rate budget.
Each action rechecks the current session and proof after waiting for all its
account/installation locks, immediately before mutations. Installation removal
locks the account then its installation, consistently with account deletion.
Sharing changes and new grants use the same order and revalidation. Their account
lock serializes mutations without blocking an OAuth exchange's foreign-key check
while that exchange holds an installation read lock.
Rejected actions preserve the installation, sharing and live streams; successful
removal retains the existing transaction and relay-revocation behavior.

Deletion shares the address lock with code delivery/verification, rechecks the
session against the current clock after waiting, and retries a rolled-back
Postgres deadlock at most twice. Persistent contention returns the existing
storage-error response; no relay access is revoked before a successful commit.

`GET /api/account/audit` returns at most the latest 100 events belonging to the
caller or their installations during their ownership period. Members see only
their own actions. It requires a current official session and uses no-store.
Audit entries accompany successful claims (including device recovery),
detachments, definitive revocations, invitations/acceptances/cancellations/delivery failures,
member removals/departures, session revocations and account deletions in the same
Postgres transaction. Failed authorization or rolled-back changes add no event.
The service stores only action, time and opaque actor/installation/target IDs;
never emails, names, credentials, request bodies or conversation content.
Pseudonymous incident metadata survives account/installation deletion for 90
days. Deleted-account entries are no longer available through its account API.
Audit audiences are opaque IDs without account foreign keys, so recording a
member departure never locks another owner's account. Deletion clears its own
audience IDs in the same transaction and retains only pseudonymous incident data.
The journal is **not append-only at database level**: account deletion updates
its audience IDs to NULL and maintenance deletes expired records. Application
mutations use a closed action enum and named event fields; operators with write
access can still alter the database. Existing migration checksums (including the
audience FK creation/removal) are preserved; the new actor index and proof column
use an additive migration rather than rewriting an applied migration history.
Reads enforce retention immediately; hourly maintenance removes expired rows.

Operators investigate the full retained history using their protected Postgres
access, without adding a public operator role or endpoint:

```sql
SELECT id, created_at, action, actor_id, installation_id, target_id
FROM account_audit
WHERE created_at > now() - interval '90 days'
ORDER BY id DESC;
```

Restrict database access and exports to operators. The account endpoint's bound
is a recent-activity view, not an exhaustive export. The single-relay-process
restriction in ADR-0032 still applies to immediate session/account revocation.

The invitation email keeps the installation name as plain text with only letters,
numbers, spaces, hyphens and underscores; URL/email punctuation is replaced by
spaces. The real name remains visible in the authenticated app. Alongside the
10/minute attempt limit, an account can attempt at most 20 invitation deliveries
per 24-hour window across its installations. Cancellation/reinvitation cannot
reset that budget; failed delivery still consumes it. Rejected ownership checks
and duplicate invitations do not consume the daily delivery allowance. Installation
GitHub repository discovery is reserved to its owner, like its coding accounts.
Members can read the agent list and avatars, while individual agent management
routes remain owner-only.

Browser streams have eight simultaneous slots per Leo account in the official
process, across installations and sessions. This complements the existing
24-stream/32-request limits per installation; cancellation, expiry and revocation
release the allowance together with the remote subscription. Extra streams return
503 and use the client's existing retry behavior. MCP grants and public downloads
retain their independent allowances. Filling one member's eight streams leaves
installation slots for the owner's live views and ordinary requests.

## Web push notifications

Devices belong to a Leo account, not an installation. Open **Notifications** in
the official account screen or the installation options menu to enable or
disable this browser once for all owned and shared installations. Installation
settings no longer contain device registration. Android native push is handled
separately in #58.

The official routes are `GET /api/account/notifications`,
`POST /api/account/notifications/subscriptions`, and
`GET` / `DELETE /api/account/notifications/subscriptions/{id}`. They require the
official session; writes also require the official origin and CSRF token.
Registration is idempotent, limited to 50 devices per account, and a browser
endpoint has one current account. Registering it after switching accounts moves
it to the new account. A foreign account cannot inspect or remove a device.
Only supported HTTPS browser push endpoints are accepted.

Configure `LEO_OFFICIAL_VAPID_PRIVATE_KEY` (the base64url P-256 private key) and
`LEO_OFFICIAL_VAPID_SUBJECT` (an operator contact, `mailto:…` or an HTTPS URL) in
the official service's private operator environment. The public key is derived
from that private key and is the only VAPID material returned to browsers. Keys
are never generated or stored by an installation. With no VAPID configuration,
the notification panel reports that push is unavailable. Keep the same operator
key across restarts and processes; changing it requires browser re-enrollment.

Events arrive through the authenticated installation relay. Recipients are the
current owner and members, rechecked before each provider request. Removal,
departure and detachment serialize with an already admitted send, whose timeout
is five seconds, and prevent subsequent sends. Account devices remain available
for other installations; there is no installation-specific subscription to
preserve accidentally after removal. Deleted accounts lose their devices via
the account foreign key. Provider 4xx responses other than 429 remove the
registration, including stale VAPID credentials (401/403). Rate limits (429),
server errors and transport failures remain retryable.

The official service keeps event content and delivery work only in bounded
memory. The installation retains events for up to one hour until acknowledged;
transient failures or tunnel loss retry without interrupting agent execution.
Delivery is at least once: interruption after a provider accepted a push can
repeat it. Notification tags include the installation and event identifiers.
Question payloads contain generic text and identifiers, never question fields.

Successful deliveries are remembered per event and current device registration
in process memory, across tunnel reconnects. An unavailable device therefore
does not repeat pushes to healthy devices. Receipts include the installation
credential generation, account and subscription keys, expire after one hour,
and have a limit of 20,000 per process. At capacity, new attempts stay in the
installation outbox. Cancelled or failed attempts release their reservation.
A service restart can repeat an already accepted push; this remains at-least-once
delivery, with the existing notification tags.

Push delivery uses a separate Postgres pool of four connections per process,
so provider waits cannot occupy the account/session API pool. Sharing locks
remain held through each admitted provider attempt to serialize with revocation.

Treat a subscription endpoint and its encryption keys as device credentials.
Account authentication authorizes registration but does not prove physical
ownership of the browser: someone holding the complete subscription can
explicitly register it on another account and move its endpoint. Avoid logging
or sharing subscription values. Endpoint transfer supports explicit account
switching; it is not performed on sign-in or session changes.

When explicitly enabling notifications, the web app replaces any browser subscription
whose application server key differs from the official public key (including old
installation VAPID subscriptions). Re-enrollment after an operator key rotation uses
the same flow.
## Android sign-in and native notifications

Android uses the same account identity, verified-email linking policy and passkey
store as the web. Google Credential Manager starts with
`POST /api/account/oauth/google/start` and `{"native":true}`, receiving a one-use
challenge, nonce and the existing Google web client ID. Its ID token is exchanged
at `POST /api/account/oauth/google/callback`; the service validates Google's
signature, issuer, audience, expiration, nonce and verified email before applying
the existing account policy. Provider tokens never become coding-agent grants.

GitHub uses a Custom Tab because Credential Manager has no GitHub provider.
`POST /api/account/oauth/github/start` with `{"native":true}` returns an official
launcher URL and an exchange secret. The launcher sets the browser challenge
cookie, then uses the existing PKCE authorization-code callback and `user:email`
scope. The callback shows an explicit confirmation naming the verified account
and **Leo for Android**, warning against links received from another person.
Opening the launcher and completing GitHub authorization alone neither links
the identity nor makes a Leo session available to the initiating device.
`POST /api/account/oauth/github/native/confirm` consumes the form's one-use proof,
bound to a separate HttpOnly browser cookie and protected by the exact official
origin. Only confirmation applies the shared linking policy and releases the
handover. The page forbids framing, scripts and foreign form destinations.
This is explicit consent, not a cryptographic device attestation; only approve
a flow you just initiated in Leo on your own device.
Android polls `POST /api/account/oauth/github/native/finish` with the
challenge and secret: 202 means pending; success creates the ordinary Leo session.
The launcher and completed exchange are each one-use and expire in five minutes.
The handover stores no session or provider access tokens, and authenticated linking
remains bound to the original Leo session, rechecked after consent. The additive
consent migration invalidates older in-flight handovers. Never log handover URLs,
confirmation proofs or secrets.

Set `LEO_OFFICIAL_ANDROID_CERTIFICATES` to comma-separated SHA-256 fingerprints
of approved APK signing certificates (colon-separated hex is accepted). Only
these exact native passkey origins are trusted. The same RP publishes
`/.well-known/assetlinks.json` for `dev.leo.manager`; without certificates it
advertises no Android association. Registration, login, reauthentication and
method removal keep their existing start/finish contracts and proof requirements.

Production Compose uses `LEO_OFFICIAL_FCM_SERVICE_ACCOUNT_JSON`, the complete
private service-account JSON supplied through the operator's secret environment,
so it needs no credential file mount. Never configure both JSON and file sources.
The existing file configuration remains supported for other deployments:
set `LEO_OFFICIAL_FCM_SERVICE_ACCOUNT` to a private service-account JSON **file
path** on the official server. The file must contain `project_id`, `client_email`
and the signing `private_key`; never distribute it to Android or installations.
The sender uses FCM HTTP v1, caches a short-lived OAuth access token and sends
data-only messages containing account/installation/conversation/event IDs.
No question fields, titles, answers or provider credentials enter FCM messages.
VAPID remains separately configurable for web devices.

An authenticated Android client checks `GET /api/account/notifications/android`
and registers its stable device UUID and current FCM token with
`POST /api/account/notifications/android` (`deviceId`, `token`). The returned `id`
uses the existing subscription lookup/removal routes. Token rotation updates the
same registration; the current account receives events from every accessible
installation. The existing recipient locks and membership checks also govern
native sends, so member removal stops subsequent sends immediately. Android
checks current access again before displaying delayed messages. FCM authorization
errors, outages and `INVALID_ARGUMENT` never delete device registrations: that
error can indicate our payload rather than the device token. Only a 400/404 with
the FCM-specific `UNREGISTERED` code removes an expired registration.

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

## Configuration

Production runs `cargo build --locked --release --bin leo-official`, then the
binary with the `dist/` produced by `pnpm build`. Configure the following env
variables through the operator's secret management:

| Variable | Meaning |
| --- | --- |
| `LEO_OFFICIAL_DATABASE_URL` | Required Postgres URL, separate from installation SQLite |
| `LEO_OFFICIAL_ORIGIN` | Required browser origin, HTTPS except localhost/loopback development; no path, query, or fragment |
| `LEO_OFFICIAL_LISTEN` | Bind address, default `127.0.0.1:4311` |
| `LEO_OFFICIAL_WEB_DIR` | Frontend output directory, default `dist` |
| `LEO_OFFICIAL_EMAIL_FROM` | Required sender address on a verified email domain |
| `LEO_OFFICIAL_EMAIL_KEY` | Required email provider bearer key; never logged |
| `LEO_OFFICIAL_EMAIL_ENDPOINT` | Default `https://api.resend.com/emails`; override only for the loopback development setup |
| `LEO_OFFICIAL_GOOGLE_CLIENT_ID`, `LEO_OFFICIAL_GOOGLE_CLIENT_SECRET` | Optional pair, enables Google sign-in |
| `LEO_OFFICIAL_GITHUB_CLIENT_ID`, `LEO_OFFICIAL_GITHUB_CLIENT_SECRET` | Optional pair, enables GitHub sign-in |

The production adapter uses [Resend's send-email contract](https://resend.com/docs/api-reference/emails/send-email)
(`from`, `to`, `subject`, `text` over HTTP). `EmailSender` is the replaceable
interface for integration tests and other delivery implementations. Delivery
errors are generic, never provider bodies. No code is returned to the client.
Automatic database migrations run at startup; the database role needs migration
permissions. Multiple processes share the same database and rate limits.
Terminate TLS at the public origin with a reverse proxy. The service checks the
configured origin on every account mutation and ignores forwarded IP headers;
IP limits use the TCP peer (with a proxy this is a shared limit). Do not expose
Postgres or the development mailbox publicly.

Codes expire after 10 minutes and allow five verification attempts. Requesting
another code invalidates the previous code for that address. Requests are limited
to one per minute per normalized email and ten per minute per TCP peer;
verification is limited to thirty per minute per TCP peer. Limits are atomic in
Postgres and survive process restarts. Expired challenges and rate buckets are
removed during code requests. Email addresses are trimmed and lowercased.

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
attach to the existing Leo account. A provider identity already attached to a
different account is rejected; linking while signed in must match the current
account's verified email.

Default endpoints are the official Google and GitHub endpoints. Tests replace
only their HTTP endpoints. Operators can override
`LEO_OFFICIAL_{GOOGLE,GITHUB}_{AUTHORIZATION_URL,TOKEN_URL,USERINFO_URL}` and
`LEO_OFFICIAL_GITHUB_EMAILS_URL`; endpoints require HTTPS, except HTTP loopback
endpoints when the official origin is also loopback. OAuth uses authorization
codes, S256 PKCE and a five-minute, single-use state bound to an HttpOnly browser
cookie. Explicit linking also checks the initiating session and CSRF token.

Passkeys use [webauthn-rs](https://docs.rs/webauthn-rs/latest/webauthn_rs/)
for signature, origin, relying-party and required user-verification checks. The
relying-party ID is the exact official origin's hostname; changing it invalidates
existing passkeys. Use an HTTPS hostname in production and `http://localhost`
for local passkey development. IP origins still support email/OAuth sign-in but
do not offer passkeys. The web uses the browser's WebAuthn JSON methods; older
browsers receive an unavailable message and can use email or OAuth instead.
Passkey sign-in asks for the account email before the browser selects a key.
Adding a named passkey requires a current session, origin and CSRF token; up to
20 passkeys can be registered per account. Registration and authentication states
stay only in Postgres, expire after five minutes and are consumed once, even on
invalid proofs. Only public credentials are stored. Removing a passkey also
invalidates challenges already issued for it.

Sign-in methods are managed from the empty installation screen. Concurrent
removals lock the account and preserve at least one method. Removed email and
OAuth identities remain recorded so an unauthenticated sign-in cannot silently
re-enable them. Re-enable email by confirming a code while signed in; re-link
Google/GitHub while signed in. Removing a method leaves active sessions intact;
session management belongs to its separate ticket. OAuth/passkey requests have
persisted rate limits, using the TCP peer rather than forwarded headers.

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
providers and a software WebAuthn authenticator, including unverified emails,
replay, explicit re-linking and concurrent removal of the final methods. The installation remains untouched by this slice.

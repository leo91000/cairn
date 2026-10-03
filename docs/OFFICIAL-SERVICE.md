# Official service and Leo accounts

The `leo-official` binary is distinct from the installation's `leo` binary. It
owns a separate Postgres database and serves the web application. This first
slice provides email sign-in and an empty installation screen. It does not
provide installation access, claiming, sharing, OAuth, or Android sign-in.

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
mailbox. They cover sign-in, an invalid code, empty installations, reload,
responsive widths and sign-out. The installation remains untouched by this slice.

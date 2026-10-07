# Production on cairn.build with an installation on the same server

This is a **preparation runbook, not authorization to deploy**. Léo must approve
production changes after #105's direct-connection qualification and review of
ticket 110. No DNS, Coolify, tag or production changes are made by implementing #110.
The initial deployment is a **fresh installation**. There is no migration of
existing conversations, local accounts or volumes.

## 1. Record the release and prepare the host

Choose a fully successful `Quality and container` run for the intended Git tree.
Its schema-3 `validated-image-<tree>/image.json` records both immutable digests,
the tested commit, tree, repository, run ID and installation CLI versions. Use:

- `LEO_OFFICIAL_IMAGE=ghcr.io/leo91000/leo-agent-manager-official@sha256:<official digest>`;
- `LEO_INSTALLATION_IMAGE=ghcr.io/leo91000/leo-agent-manager@sha256:<installation digest>`.

Take both from **the same evidence**, never from a moving candidate, `latest`, or
two independently chosen releases. A reused PR image reports its tested merge
commit, whose Git tree matches the release, rather than a later tag commit.
The official image contains `leo-official` and the matching `pnpm build` output;
it has no worker CLI or VM layers. `/health` returns no-store JSON with `status`,
`commit` and `runtimeId`, and returns 503 when Postgres is unavailable.

Reserve capacity for Coolify/Traefik, the official process, Postgres and Garage
before assigning VM budgets. The official Compose caps each of official and
Postgres at 1 GiB and one CPU. These are initial limits, not measured production
capacity guarantees. The installer currently caps its manager at 4 GiB and its
runner at 20 GiB; Garage, Docker caches, guest disks and host services also use
resources. Those defaults can exceed a small server's RAM. Before starting agent
work, adjust `/var/lib/leo-installation/compose.json` budgets and installation
node capacity to the actual spare memory/CPU; keep that reserve when increasing
concurrency. Monitor disk space and memory under real work. Rerunning the
installer regenerates its Compose defaults; reapply reviewed budgets afterwards.

The installation host needs Linux x86-64, systemd, Docker Engine/Compose,
Python 3, curl, working `/dev/kvm`, `/dev/net/tun`, `/dev/fuse`, and at least
16 GiB free **in addition to** Postgres/backups and conversation disks. See
[installation prerequisites](INSTALLATION.md). KVM is for the installation,
not the official process. Do not give the official container privileged mode,
KVM, the Docker socket or installation volumes.

## 2. Remove the old public manager (explicit data loss)

Schedule downtime and record the old service UUID, image digest and its exact
volume/bind-mount names. Disable the old tag deployment target before the first
new release: replace the GitHub `production` environment's
`COOLIFY_SERVICE_UUID` with the **new official** service UUID in step 5.
Disable `leo-cli-update.timer` and any other old host dispatch/update timers
while dismantling the old installation. The historical **Update agent tools**
workflow's Coolify deploy job is disabled in code for this cutover; do not reuse
it with the official service UUID. New installation updates use their own timer.

Stop and remove the old Leo manager/runner Coolify service and its public domain
route. Remove **only that service's identified** data, agent-home, workspaces,
runner-state and any old installation S3 volumes if discarding the old work.
Deleting these volumes permanently deletes conversations, project checkouts,
agent sign-ins, private credentials and disks. Retain a private encrypted backup
first if any recovery may be needed. Never prune all Docker volumes or remove
Coolify/Traefik volumes. Do not reuse those old volumes for the new installation.
The new official Postgres volume is separate and must not be deleted here.

## 3. Cloudflare DNS and TLS

In the Cloudflare zone for `cairn.build`, create/update an apex **A** record
(name `@`) to the production server's public IPv4. Add **AAAA** only if that
server and its TLS proxy actually serve IPv6; remove a stale apex AAAA record.

The initial supported setup uses **DNS only** (grey cloud): Cloudflare hosts DNS,
while browser and installation HTTPS reach Traefik directly. This avoids an
additional trusted proxy hop and caching rules. Keep server ports 80/443 open
for the existing proxy and certificate issuance; never publish 4311 or Postgres
on the host. Verify the apex resolves and Traefik can issue a valid certificate.
DNS-only apex records are described in
[Cloudflare's apex-record documentation](https://developers.cloudflare.com/dns/manage-dns-records/how-to/create-zone-apex/).

If Cloudflare proxying is enabled later, use Full (strict) TLS, allow WebSockets,
bypass caching for Leo's dynamic endpoints, and review the complete trusted
forwarded-header chain first. Do not add all public IP ranges to trusted proxies.

## 4. Resend and optional sign-in/push

Add the sending domain `cairn.build` in Resend and copy the exact SPF/DKIM and any
other verification records it supplies into Cloudflare DNS. Do not guess their
values or overwrite unrelated mail routing. Wait until Resend marks the domain
verified. Create a restricted sending API key for that domain; configure it only
as the private `LEO_OFFICIAL_EMAIL_KEY` environment value in Coolify. Set
`LEO_OFFICIAL_EMAIL_FROM` to `Cairn <leo@cairn.build>` (or another sender on the
verified domain). Production uses `https://api.resend.com/emails`; there is no
development mailbox or endpoint override in production Compose.
See [Resend domain verification](https://resend.com/docs/dashboard/domains/introduction).

Optional OAuth configuration, each enabled provider requiring **both** values:

| Provider | Application settings | Private environment |
| --- | --- | --- |
| GitHub | OAuth App homepage `https://cairn.build`; authorization callback `https://cairn.build/api/account/oauth/github/callback` | `LEO_OFFICIAL_GITHUB_CLIENT_ID`, `LEO_OFFICIAL_GITHUB_CLIENT_SECRET` |
| Google | Web OAuth client; authorized JavaScript origin `https://cairn.build`; redirect URI `https://cairn.build/api/account/oauth/google/callback`; configure consent screen/publishing as required | `LEO_OFFICIAL_GOOGLE_CLIENT_ID`, `LEO_OFFICIAL_GOOGLE_CLIENT_SECRET` |

Leaving both variables empty hides the provider. GitHub sign-in requests only
`user:email`, never repository access; agent GitHub credentials are installed
separately. Passkeys use the `cairn.build` relying-party ID automatically: keep
this origin stable. For outbound installation MCP OAuth connections, the separate
callback remains `https://cairn.build/oauth/mcp/callback`, not an account callback.

Optional Web Push: generate and retain a VAPID P-256 key privately; set
`LEO_OFFICIAL_VAPID_PRIVATE_KEY` (base64url) and `LEO_OFFICIAL_VAPID_SUBJECT`
(`mailto:` operator contact or HTTPS contact URL) together in Coolify. The service
derives the public key. No keys in Git, build arguments, logs or screenshots.
With neither configured, Web Push is unavailable; email sign-in still works.

Optional Android/FCM (delivered by #58): create a Firebase project with an Android
application for `dev.leo.manager`. Supply the matching approved APK SHA-256 signing
certificate fingerprints to `LEO_OFFICIAL_ANDROID_CERTIFICATES` (comma-separated,
colon-separated hex accepted). The official service publishes their association
at `https://cairn.build/.well-known/assetlinks.json`. Register that Android package
and its signing certificate in the Google OAuth project too; the existing Google
web client ID remains the token audience.

Enable the Firebase Cloud Messaging HTTP v1 interface and obtain a dedicated
service account permitted to send for this project. Set its complete private JSON
only as the secret `LEO_OFFICIAL_FCM_SERVICE_ACCOUNT_JSON` in the Coolify service
environment. It must contain `project_id`, `client_email`, and the signing
`private_key`; preserve the JSON escapes/newlines. The production Compose requires
no secret file mount. The older file-path configuration remains supported outside
this Compose; never configure both sources. Never distribute service-account keys
to Android, an installation, build arguments or artifacts.

For the matching Android release, set the repository's **public** workflow
variables `LEO_ANDROID_FIREBASE_APP_ID`, `LEO_ANDROID_FIREBASE_PROJECT_ID`,
`LEO_ANDROID_FIREBASE_API_KEY`, `LEO_ANDROID_FIREBASE_SENDER_ID` from that Firebase
Android app, and the repository variable `LEO_OFFICIAL_ORIGIN=https://cairn.build`.
These are separate from the GitHub `production` environment variables used by
server deployment. Changing them requires APK revalidation under the existing
Android workflow. Do not release the APK or create a tag until Léo approves.
Verify opt-in native delivery on a real configured device; CI's controlled FCM
adapter is not live-provider evidence. With FCM unset, native push is unavailable
but sign-in remains usable. See [Android setup](../android/README.md#native-account-providers-and-push).

## 5. Create the official Coolify service

Use `deploy/official/compose.production.yaml`, distinct from the development
`compose.yaml`. It runs official + Postgres, a persistent Postgres volume,
healthchecks, rotated logs, explicit budgets and **one official process**.
Keep stop-first replacement; never run old and new official processes together
or add a second application replica (ADR-0032).

Connect only official to the existing external proxy Docker network (default
`coolify`, override `COOLIFY_PROXY_NETWORK` if needed). Check the actual Traefik
configuration before using the labels: defaults are HTTPS entrypoint `https`
and resolver `letsencrypt`, overridable with `LEO_PROXY_HTTPS_ENTRYPOINT` and
`LEO_PROXY_CERT_RESOLVER`. The supplied labels route the apex to HTTP port 4311.
Keep the existing proxy's HTTP-to-HTTPS redirection enabled. Choose this label
configuration instead of adding a second Coolify-generated router for the same
hostname. See [Coolify's Traefik configuration](https://coolify.io/docs/core/networking/proxy/traefik/overview).

Set these values in the service environment, without committing an `.env` file:

| Variable | Value/action |
| --- | --- |
| `LEO_OFFICIAL_IMAGE` | Validated official digest from step 1 |
| `LEO_INSTALLATION_IMAGE` | Paired installation digest from the same evidence |
| `LEO_OFFICIAL_POSTGRES_PASSWORD` | Strong generated private password |
| `LEO_OFFICIAL_DATABASE_URL` | `postgres://leo:<URL-encoded same password>@postgres:5432/leo_official` |
| `LEO_OFFICIAL_TRUSTED_PROXIES` | Actual controlled Traefik peer IP (`/32` or `/128`), plus only necessary controlled proxy hops |
| `LEO_OFFICIAL_EMAIL_FROM`, `LEO_OFFICIAL_EMAIL_KEY` | Verified sender and private Resend key |
| Optional OAuth/VAPID pairs and Android/FCM | Step 4, or leave empty |

The official browser origin is fixed to `https://cairn.build` by Compose.
Postgres is on a separate internal network, with no published port. Official has
only `expose: 4311`, no host port. Inspect the actual proxy peer address/network
privately; `LEO_OFFICIAL_TRUSTED_PROXIES` must match the TCP peer seen by official.
Prefer a stable specific proxy address. If a CIDR is necessary, restrict who can
join it; never trust a whole shared Docker network containing arbitrary workers.
Recheck after proxy/network recreation. Traefik must append the real client peer
to `X-Forwarded-For`; client-supplied headers alone must not choose quotas.

Start the new service only after Léo's approval. Verify `/health` reports the
expected tested commit, `/install/release` reports the paired digest, `/` and
`/claim` serve the SPA with framing protection, and an email-code sign-in works.
Back up Postgres before subsequent updates (step 7). Automatic checksum-checked
migrations run on startup; do not reuse a historical development database.

Configure GitHub's **production** environment:

| Setting | Kind | Value |
| --- | --- | --- |
| `COOLIFY_TOKEN` | Secret | Dedicated API token with read/write/deploy abilities |
| `COOLIFY_URL` | Variable | Existing Coolify HTTPS origin |
| `COOLIFY_SERVICE_UUID` | Variable | **New official Compose service** UUID |
| `LEO_OFFICIAL_ORIGIN` | Variable | `https://cairn.build` |

Protect this environment with Léo as required reviewer so a future `v*` tag
cannot deploy without approval. Remove the obsolete `LEO_PUBLIC_URL` deployment
variable; the CLI no longer deploys a standalone public manager. Main/PR builds
never deploy. A tag promotes both tested digests, sets `LEO_OFFICIAL_IMAGE` and
`LEO_INSTALLATION_IMAGE`, restarts only the official service, then verifies both
its build identity and approved installation release. No automatic official
rollback is attempted after a failed rollout; use step 7.

## 6. Install and claim on the same server

Sign in at `https://cairn.build`, choose **Add an installation**, then run its
complete one-command installer on this same host within ten minutes. It creates
`/var/lib/leo-installation` with fresh manager, runner and Garage data and starts
`leo-installation-update.timer`. No installation service publishes a host port;
manager/runner/Garage communicate over their private Compose network.

The host **and its containers** must resolve and reach `https://cairn.build:443`
via the server's public address and TLS proxy, even though official is on the
same machine. Verify DNS, firewall/hairpin routing and the certificate from both
contexts. Do not replace the official origin with `localhost`, `http://official`
or a container address: authentication, passkeys and origin checks use the public
HTTPS origin. Permit outbound registry HTTPS and mail/OAuth/push provider access
as needed. Permit outbound UDP for direct WebRTC when possible; blocked UDP uses
the authenticated HTTPS relay. No incoming installation port is required.

If the code expired, obtain a new command or run `sudo leo claim`. Open the
printed `https://cairn.build/claim` URL, compare the displayed name/fingerprint
with the machine, approve, wait for success, then restart the manager:

```sh
sudo docker compose --project-directory /var/lib/leo-installation \
  -f /var/lib/leo-installation/compose.json restart manager
```

Verify the claimed installation appears online, conversation reads/writes work,
and access returns after restart. There is no anonymous local access, local
password or old manager web login. Connect agent accounts separately through
Connections. The installation update timer follows `/install/release` every five
minutes; it drains/checkpoints and replaces manager+runner together, with its
existing health verification and rollback. Updating official does not directly
restart the installation. Do not start a second old installation on these mounts.

## 7. Backups and rollback

Before **each** official upgrade, record the current official **and approved
installation** digests and health commit, then stop official (short downtime).
Take a private Postgres custom-format backup and keep it with that release:

```sh
umask 077
# Run from the official service's operator checkout using its protected environment.
docker compose -f deploy/official/compose.production.yaml stop official
docker compose -f deploy/official/compose.production.yaml exec -T postgres \
  pg_dump -U leo -d leo_official -Fc > /private/backup/official-before-release.dump
docker compose -f deploy/official/compose.production.yaml start official
```

Check the backup can be restored on a disposable Postgres 17 instance; encrypt it
and copy it off the server. It contains account/session material and must never
be attached to GitHub or published. The installation separately needs stopped
backups of **its whole directory including Garage and private identity**; a
Postgres backup does not include conversations, disk objects or agent accounts.

If official deployment fails, freeze further release approvals/tags, stop
**official**, and restore `LEO_OFFICIAL_IMAGE` to the previous verified digest.
Restore `LEO_INSTALLATION_IMAGE` to its previous paired digest too. An already
updated installation will see that approved rollback via its existing timer;
only approve it if its database remains compatible, otherwise restore its
matching stopped backup as described in [installation recovery](INSTALLATION.md).

For official, startup migrations can be incompatible with the older binary.
Restore the **matching pre-release Postgres backup** into an empty database while
official is stopped; preserve the failed database privately for investigation.
Use a fresh isolated volume/database, or explicitly recreate `leo_official` only
after retaining a backup, and restore with `pg_restore -U leo -d leo_official
--exit-on-error` through the protected container connection. This loses account,
claim and sharing changes made after the backup; reconcile installation identities
using [claim recovery](INSTALLATION-RELAY.md) if necessary. Never start the older
binary against a newer schema or reuse a backup from an unrelated release.

Restart the single official process, verify `/health`'s previous commit,
`/install/release`'s previous digest, email sign-in, claim/relay reconnection and
conversation access. TLS/DNS do not need changing for this rollback. Keep the
failed release, backups and previous digests until recovery is confirmed.

# Production on cairn.build with an installation on the same server

This is a **preparation runbook, not authorization to deploy**. Léo must approve
production changes after #105's direct-connection qualification and review of
tickets #110 and #113. No DNS, Coolify, tag or production changes are made by
implementing these tickets.
The initial deployment is a **fresh installation**. There is no migration of
existing conversations, local accounts or volumes.

## 1. Record the release and prepare the host

Choose a fully successful `Quality and container` run for the intended Git tree.
Its schema-3 `validated-image-<tree>/image.json` records both immutable digests,
the tested commit, tree, repository, run ID and installation CLI versions. Use:

- `CAIRN_BEACON_IMAGE=ghcr.io/leo91000/cairn-beacon@sha256:<official digest>`;
- `CAIRN_INSTALLATION_IMAGE=ghcr.io/leo91000/cairn@sha256:<installation digest>`.

Take both from **the same evidence**, never from a moving candidate, `latest`, or
two independently chosen releases. A reused PR image reports its tested merge
commit, whose Git tree matches the release, rather than a later tag commit.
The official image contains `cairn-beacon` and the matching `pnpm build` output;
it has no worker CLI or VM layers. `/health` returns no-store JSON with `status`,
`commit`, `runtimeId` and `stun` (`status`, `receiveErrors`, `sendErrors`). STUN
status is independent of HTTP/database readiness: `running`, `retrying` after a
receive error, or `stopped` after termination. UDP errors are counted and logged
without addresses or packets; receive errors retry with a bounded pause instead
of silently ending address discovery. Receive and send warnings are each limited
to one per 30 seconds, while the health counters record every error.
HTTP/relay readiness stays usable during
STUN loss. Readiness is sampled every five seconds with a
one-second query timeout; HTTP probes read the cached result without querying
Postgres. Database loss is reported within six seconds, and recovery at the next
successful sample. It is unavailable until the first sample succeeds.

Reserve capacity for Coolify/Traefik, the official process, Postgres and Garage
before assigning VM budgets. The official Compose caps each of official and
Postgres at 1 GiB and one CPU. These are initial limits, not measured production
capacity guarantees. The installer currently caps its manager at 4 GiB and its
runner at 20 GiB; Garage, Docker caches, guest disks and host services also use
resources. Those defaults can exceed a small server's RAM. Before starting agent
work, adjust `/var/lib/cairn-installation/compose.json` budgets and installation
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
Disable `cairn-cli-update.timer` and any other old host dispatch/update timers
while dismantling the old installation. The historical **Update agent tools**
workflow is entirely disabled in code for this cutover: manual dispatch only
explains the release path and never reads production configuration. Do not reuse
its host timer with Beacon UUID. New installation updates use their own timer.

Stop and remove the old Cairn manager/runner Coolify service and its public domain
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
bypass caching for Cairn's dynamic endpoints, and review the complete trusted
forwarded-header chain first. Do not add all public IP ranges to trusted proxies.

## 4. Resend and optional sign-in/push

Add the sending domain `cairn.build` in Resend and copy the exact SPF/DKIM and any
other verification records it supplies into Cloudflare DNS. Do not guess their
values or overwrite unrelated mail routing. Wait until Resend marks the domain
verified. Create a restricted sending API key for that domain; configure it only
as the private `CAIRN_BEACON_EMAIL_KEY` environment value in Coolify. Set
`CAIRN_BEACON_EMAIL_FROM` to `Cairn <cairn@cairn.build>` (or another sender on the
verified domain). Production uses `https://api.resend.com/emails`; there is no
development mailbox or endpoint override in production Compose.
See [Resend domain verification](https://resend.com/docs/dashboard/domains/introduction).

Optional OAuth configuration, each enabled provider requiring **both** values:

| Provider | Application settings | Private environment |
| --- | --- | --- |
| GitHub | OAuth App homepage `https://cairn.build`; authorization callback `https://cairn.build/api/account/oauth/github/callback` | `CAIRN_BEACON_GITHUB_CLIENT_ID`, `CAIRN_BEACON_GITHUB_CLIENT_SECRET` |
| Google | Web OAuth client; authorized JavaScript origin `https://cairn.build`; redirect URI `https://cairn.build/api/account/oauth/google/callback`; configure consent screen/publishing as required | `CAIRN_BEACON_GOOGLE_CLIENT_ID`, `CAIRN_BEACON_GOOGLE_CLIENT_SECRET` |

Leaving both variables empty hides the provider. GitHub sign-in requests only
`user:email`, never repository access; agent GitHub credentials are installed
separately. Passkeys use the `cairn.build` relying-party ID automatically: keep
this origin stable. For outbound installation MCP OAuth connections, the separate
callback remains `https://cairn.build/oauth/mcp/callback`, not an account callback.

Optional Web Push: generate and retain a VAPID P-256 key privately; set
`CAIRN_BEACON_VAPID_PRIVATE_KEY` (base64url) and `CAIRN_BEACON_VAPID_SUBJECT`
(`mailto:` operator contact or HTTPS contact URL) together in Coolify. The service
derives the public key. No keys in Git, build arguments, logs or screenshots.
With neither configured, Web Push is unavailable; email sign-in still works.

Optional Android/FCM (delivered by #58): create a Firebase project with an Android
application for `build.cairn.app`. Supply the matching approved APK SHA-256 signing
certificate fingerprints to `CAIRN_BEACON_ANDROID_CERTIFICATES` (comma-separated,
colon-separated hex accepted). Beacon publishes their association
at `https://cairn.build/.well-known/assetlinks.json`. Register that Android package
and its signing certificate in the Google OAuth project too; the existing Google
web client ID remains the token audience.

Enable the Firebase Cloud Messaging HTTP v1 interface and obtain a dedicated
service account permitted to send for this project. Set its complete private JSON
only as the secret `CAIRN_BEACON_FCM_SERVICE_ACCOUNT_JSON` in the Coolify service
environment. It must contain `project_id`, `client_email`, and the signing
`private_key`; preserve the JSON escapes/newlines. The production Compose requires
no secret file mount. The older file-path configuration remains supported outside
this Compose; never configure both sources. Never distribute service-account keys
to Android, an installation, build arguments or artifacts.

For the matching Android release, set the repository's **public** workflow
variables `CAIRN_ANDROID_FIREBASE_APP_ID`, `CAIRN_ANDROID_FIREBASE_PROJECT_ID`,
`CAIRN_ANDROID_FIREBASE_API_KEY`, `CAIRN_ANDROID_FIREBASE_SENDER_ID` from that Firebase
Android app, and the repository variable `CAIRN_BEACON_ORIGIN=https://cairn.build`.
These are separate from the GitHub `production` environment variables used by
server deployment. Changing them requires APK revalidation under the existing
Android workflow. Do not release the APK or create a tag until Léo approves.
Verify opt-in native delivery on a real configured device; CI's controlled FCM
adapter is not live-provider evidence. With FCM unset, native push is unavailable
but sign-in remains usable. See [Android setup](../apps/android/README.md#native-account-providers-and-push).

## 5. Create the official Coolify service

Use `deploy/official/compose.production.yaml`, distinct from the development
`compose.yaml`. It runs official + Postgres, a persistent Postgres volume,
healthchecks, rotated logs, explicit budgets and **one official process**.
Keep stop-first replacement; never run old and new official processes together
or add a second application replica (ADR-0032).

Connect only official to the dedicated external proxy Docker network configured
below, using `COOLIFY_PROXY_NETWORK`. Check the actual Traefik
configuration before using the labels: defaults are HTTPS entrypoint `https`
and resolver `letsencrypt`, overridable with `CAIRN_PROXY_HTTPS_ENTRYPOINT` and
`CAIRN_PROXY_CERT_RESOLVER`. The supplied labels route the apex to HTTP port 4311.
Keep the existing proxy's HTTP-to-HTTPS redirection enabled. Choose this label
configuration instead of adding a second Coolify-generated router for the same
hostname. See [Coolify's Traefik configuration](https://coolify.io/docs/core/networking/proxy/traefik/overview).

Set these values in the service environment, without committing an `.env` file:

| Variable | Value/action |
| --- | --- |
| `CAIRN_BEACON_IMAGE` | Validated official digest from step 1 |
| `CAIRN_INSTALLATION_IMAGE` | Paired installation digest from the same evidence |
| `CAIRN_BEACON_POSTGRES_PASSWORD` | Strong generated private password |
| `CAIRN_BEACON_DATABASE_URL` | `postgres://cairn:<URL-encoded same password>@postgres:5432/cairn_official` |
| `COOLIFY_PROXY_NETWORK` | Dedicated external bridge with persisted static proxy IP, configured below |
| `CAIRN_BEACON_TRUSTED_PROXIES` | The persisted static Traefik peer IP (`/32` or `/128`), plus only necessary controlled proxy hops |
| `CAIRN_BEACON_EMAIL_FROM`, `CAIRN_BEACON_EMAIL_KEY` | Verified sender and private Resend key |
| Optional OAuth/VAPID pairs and Android/FCM | Step 4, or leave empty |

The official browser origin is fixed to `https://cairn.build` by Compose.
Postgres is on a separate internal network, with no published port. Official has
only `expose: 4311`, no host port.

### Persist a fixed Traefik peer across recreation

Do not copy today's dynamic IP from the shared `coolify` network into trust.
Use a dedicated external bridge for the official/proxy connection, and persist
Traefik's static address in its **saved main proxy Compose configuration**
(`/data/coolify/proxy/docker-compose.yml`, editable in Coolify's server Proxy
configuration). Keep its existing networks, ports, command, labels and volumes.
Do not use only `docker network connect --ip`: that attachment is lost on
recreation. Do not put this setting in a Traefik dynamic-routing file.

The following subnet is an **example**. Choose a nonoverlapping private subnet
on the actual server; check Docker IPAM and host/VPN routes first. Reserve the
static address outside the automatic allocation range:

```sh
docker network create --driver bridge --subnet 172.30.113.0/24 \
  --ip-range 172.30.113.128/25 --gateway 172.30.113.1 cairn-beacon-proxy
```

Merge this into the existing proxy Compose (the actual service key may differ):

```yaml
services:
  traefik:
    networks:
      coolify: {} # retain every existing attachment too
      cairn-beacon-proxy:
        ipv4_address: 172.30.113.2
networks:
  # retain the existing coolify declaration and all other declarations
  cairn-beacon-proxy:
    external: true
    name: cairn-beacon-proxy
```

In Beacon set `COOLIFY_PROXY_NETWORK=cairn-beacon-proxy` and
`CAIRN_BEACON_TRUSTED_PROXIES=172.30.113.2/32` (adapt both to the selected network).
The Compose's network label selects this exact network for backend traffic.
Never trust `172.30.113.0/24`, the entire shared `coolify` network, or workers.
Only official and the controlled proxy should join the dedicated bridge;
Postgres stays on its internal database network. Leave Traefik's incoming
forwarded-header trust at its secure default for this DNS-only deployment.

After Léo approves proxy changes, save and recreate it through Coolify, then
verify its address survived, with the actual proxy container name:

```sh
proxy_container=coolify-proxy
docker inspect --format '{{with index .NetworkSettings.Networks "cairn-beacon-proxy"}}{{.IPAddress}}{{end}}' "$proxy_container"
```

The output must equal the saved host address (`172.30.113.2` in this example).
Verify the official container is on that bridge and its network label selects
it. Verify sign-in and rate limits through HTTPS. Repeat this check after every
proxy recreation, network change or Coolify update; if Coolify regenerates its
main proxy configuration, reapply the saved static attachment **before** Cairn
traffic resumes. Never widen trust to fix a mismatch. Sources:
[Coolify main proxy configuration](https://coolify.io/docs/core/networking/proxy/traefik/overview),
[Compose static network addresses](https://docs.docker.com/reference/compose-file/services/#ipv4_address-ipv6_address).

Start the new service only after Léo's approval. Verify `/health` reports the
expected tested commit, `/install/release` reports the paired digest, `/` and
`/claim` serve the SPA with framing protection, and an email-code sign-in works.
Back up Postgres before subsequent updates (step 7). Automatic checksum-checked
migrations run on startup; do not reuse a historical development database.

Configure GitHub's **production** environment:

| Setting | Kind | Value |
| --- | --- | --- |
| `COOLIFY_TOKEN` | Secret | Dedicated API token with read, read:sensitive, write and deploy abilities |
| `COOLIFY_URL` | Variable | Existing Coolify HTTPS origin |
| `COOLIFY_SERVICE_UUID` | Variable | **New official Compose service** UUID |
| `CAIRN_BEACON_ORIGIN` | Variable | `https://cairn.build` |

Protect this environment with Léo as required reviewer so a future `v*` tag
cannot deploy without approval. Remove the obsolete `CAIRN_PUBLIC_URL` deployment
variable; the CLI no longer deploys a standalone public manager. Main/PR builds
never deploy. A tag promotes both tested digests, updates `CAIRN_BEACON_IMAGE` and
`CAIRN_INSTALLATION_IMAGE` through the bulk endpoint, reads both literal values
back, and repairs partial updates before restart. Failed repair restores the
previous pair without restart; if even restoration fails, freeze all manual
restarts and repair both values to one validated pair before retrying. The script
then verifies both
its build identity and approved installation release. No automatic official
rollback is attempted after a failed rollout; use step 7.

## 6. Install and claim on the same server

Sign in at `https://cairn.build`, choose **Add an installation**, then run its
complete one-command installer on this same host within ten minutes. It creates
`/var/lib/cairn-installation` with fresh manager, runner and Garage data and starts
`cairn-installation-update.timer`. No installation service publishes a host port;
manager/runner/Garage communicate over their private Compose network.

The host **and its containers** must resolve and reach `https://cairn.build:443`
via the server's public address and TLS proxy, even though official is on the
same machine. Verify DNS, firewall/hairpin routing and the certificate from both
contexts. Do not replace the official origin with `localhost`, `http://official`
or a container address: authentication, passkeys and origin checks use the public
HTTPS origin. Permit outbound registry HTTPS and mail/OAuth/push provider access
as needed. Permit outbound UDP for direct WebRTC when possible; blocked UDP uses
the authenticated HTTPS relay. No incoming installation port is required.

If the code expired, obtain a new command or run `sudo cairn claim`. Open the
printed `https://cairn.build/claim` URL, compare the displayed name/fingerprint
with the machine, approve, wait for success, then restart the manager:

```sh
sudo docker compose --project-directory /var/lib/cairn-installation \
  -f /var/lib/cairn-installation/compose.json restart manager
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
installation** digests and health commit. Back up Postgres while official is
stopped (short downtime). Use the actual Coolify service UUID to list containers,
then select the current official and Postgres container IDs by role and image:

```sh
service_uuid='the-official-coolify-service-uuid'
docker ps -a --filter "label=com.docker.compose.project=$service_uuid" \
  --format '{{.ID}}  {{.Names}}  {{.Image}}  {{.Label "com.docker.compose.service"}}'
```

Coolify can suffix service/container names; do not assume `cairn-beacon-postgres-1`
or the checkout's Compose project. If the installed Coolify version uses a
different project label, find the IDs in its service Containers screen and
confirm the project, role and image with the same listing. Select only this
service's current containers, never the installation or Coolify's own database.
Do not print `docker inspect` environment values or `docker compose config`.

Run from a trusted checkout containing the helper; no Compose interpolation or
operator `.env` file is needed. The database commands use the container's local
operator connection and never pass a password through shell arguments:

```sh
umask 077
official_container='selected-official-container-id'
postgres_container='selected-postgres-container-id'
backup_path='/private/backup/official-before-release.dump'
# Each step runs only if its predecessor succeeded; on error investigate.
docker stop --time 60 "$official_container" && \
  bash deploy/official/postgres-backup.sh backup "$postgres_container" "$backup_path" && \
  docker start "$official_container"
```

The helper writes a private custom-format dump to a temporary adjacent file,
then publishes it atomically only after `pg_dump` succeeds. An existing backup
is never overwritten; the directory must support hard links. Failure removes
the partial file.
Use a new filename for each release and retain the release/digest record privately.

Check the backup can be restored on a disposable Postgres 17 instance; encrypt it
and copy it off the server. It contains account/session material and must never
be attached to GitHub or published. The installation separately needs stopped
backups of **its whole directory including Garage and private identity**; a
Postgres backup does not include conversations, disk objects or agent accounts.

If official deployment fails, freeze further release approvals/tags, stop
**official**, and restore `CAIRN_BEACON_IMAGE` to the previous verified digest.
Restore `CAIRN_INSTALLATION_IMAGE` to its previous paired digest too. An already
updated installation will see that approved rollback via its existing timer;
only approve it if its database remains compatible, otherwise restore its
matching stopped backup as described in [installation recovery](INSTALLATION.md).

For official, startup migrations can be incompatible with the older binary.
Restore the **matching pre-release Postgres backup** into an empty database while
official is stopped; preserve the failed database privately for investigation.
Use a fresh isolated volume/database, or explicitly recreate `cairn_official` only
after retaining a backup of the failed database. Rediscover the current container
IDs after a failed deployment; never restart a stale container from before it.
Set the **previous paired digests** in Coolify while stopped. For the explicitly
chosen database container, with every official process stopped:

```sh
# Destructive recovery: only after preserving the failed database privately.
docker exec "$postgres_container" dropdb -U cairn --force cairn_official && \
  docker exec "$postgres_container" createdb -U cairn -O cairn cairn_official && \
  bash deploy/official/postgres-backup.sh restore "$postgres_container" "$backup_path"
```

The helper invokes `pg_restore --exit-on-error --no-owner --no-privileges` into
that empty database. The schema/data are owned by the current `cairn` operator
role; database roles and grants must be provisioned separately if that changes.
A failed restore keeps official stopped; recreate the empty recovery database
before retrying rather than continuing on a partial schema. This loses account,
claim and sharing changes made after the backup; reconcile installation identities
using [claim recovery](INSTALLATION-RELAY.md) if necessary. Never start the older
binary against a newer schema or reuse a backup from an unrelated release.

`node tests/official-postgres-backup.mjs` exercises this exact helper against
pinned, disposable Postgres, including a renamed container, file permissions,
known-data round trip, refusal to overwrite a previous backup, rejection of a
nonempty target/invalid dump, and cleanup
of a failed backup. It uses synthetic data and never connects to production.

Restart the single official process, verify `/health`'s previous commit,
`/install/release`'s previous digest, email sign-in, claim/relay reconnection and
conversation access. TLS/DNS do not need changing for this rollback. Keep the
failed release, backups and previous digests until recovery is confirmed.


## 8. Manual release checklist (required before approving a tag deployment)

The GitHub runner has no host Docker access. Automatically wiring a database
backup would add privileged host access and secret backup storage to CI, so #113
keeps the backup **manual and mandatory**, behind Léo's production environment
approval. Do not approve that deployment until:

- [ ] [#105](https://github.com/leo91000/leo-agent-manager/issues/105) is qualified
      and the independent review of #113 has passed.
- [ ] The exact Git tree has complete green CI and one schema-3 evidence pair;
      record both candidate and previous image digests privately.
- [ ] Verify the saved proxy static IP and actual peer still match the exact
      `/32` or `/128` trusted address; no shared Docker CIDR is trusted.
- [ ] Stop official, take the pre-release Postgres backup using step 7, restore
      it on isolated Postgres 17, encrypt/copy it off-host and record its release.
      Never publish this production dump as a CI artifact.
- [ ] Check installation schema rollback compatibility; otherwise retain a
      matching stopped backup of the entire installation, including Garage.
- [ ] Resume the previous single official process, verify health/sign-in, then
      explicitly approve the tag's `production` job. This preparation task
      creates no tag and performs no deployment.
- [ ] After rollout, verify the exact health identity, paired installer digest,
      email sign-in, relay reconnection, and conversation access. On failure,
      freeze releases and follow step 7 before approving another pair.

## STUN and direct installation connectivity (#101)

The single `cairn-beacon` process serves STUN **Binding only** on UDP 3478. It
never relays TURN, authenticates an installation or transports application data.
The Compose definition publishes `3478:3478/udp` directly; Traefik continues to
serve HTTPS. `CAIRN_BEACON_STUN_URL` defaults to
`stun:<CAIRN_BEACON_ORIGIN host>:3478`; production pins `stun:cairn.build:3478`.
The public DNS address must reach the host directly for UDP (the documented DNS
only configuration does this). `CAIRN_BEACON_STUN_LISTEN` controls the bind address.
Authorize/renew responses and the authenticated installation tunnel supply this
URL to clients and installations. TURN and public third-party STUN remain deferred.

Before an authorized deployment, allow inbound UDP 3478 in the host firewall and
cloud security group (for example `ufw allow 3478/udp`), retaining HTTPS 443.
Docker's published UDP port must use iptables/nftables DNAT, preserving the
**external client's source IP and port**. A userland docker-proxy or intervening
UDP proxy can instead report its own address. Verify from a separate host with
a Binding probe and compare XOR-MAPPED-ADDRESS with that host's public address;
never accept a gateway or the server's own address as evidence. The reproducible
namespace bench checks an equivalent DNAT path. A disposable Docker published
port check is described in NETWORK-BENCH.md; deployment-specific firewall and
source preservation verification stays a pre-release gate in #113/#105. No
production firewall, Docker-daemon setting or deployment is changed by this PR.

The installation still publishes **no UDP port**. Its Compose NAT generally
preserves outgoing ports and permits matching reply tuples. A host firewall or
security group denying unsolicited UDP can prevent peer-reflexive discovery,
especially against a symmetric-NAT client; HTTPS relay fallback is expected.
`symmetric-client` in the bench models this installation NAT against a client
with destination-dependent mappings. No host-network or fixed-port opt-in is
introduced, preserving ADR-0028's no-incoming-port decision.

When installation and official STUN share a host, the installation's query to
`cairn.build:3478` may hairpin through Docker and expose a Docker gateway as its
reflexive address. `same-server` reproduces that error: the external client sees
its actual NAT address, while the installation sees `10.102.2.1`. For an explicitly
verified, **port-preserving** installation NAT, set `CAIRN_DIRECT_PUBLIC_IP` to the
host's public unicast IP in the installation environment. The peer retains its
bound host candidate and additionally advertises that public alias with the same
ephemeral port through authenticated signaling. This opens no listener or port
mapping. The bench verifies a successful authorized DataChannel with this setting.
It cannot repair a NAT that changes the public port; leave direct disabled or use
the relay on such a host until qualification establishes a supported mapping.
A wrong setting only makes direct fail; it never relaxes grant or DTLS validation.
`CAIRN_DIRECT_STUN_URLS` can also select a dedicated official STUN endpoint, useful
where a distinct reachable official address avoids hairpinning. It must remain
operator-controlled; no automatic third-party fallback is configured.


Before qualification #105 approves IPv6 STUN, repeat the external-source probe
from a separate IPv6 host through the actual published UDP 3478 port. Check both
source IPv6 address and source port: Docker's userland proxy may translate IPv6
to IPv4 or expose a gateway even when IPv4 DNAT preserves the source. Do not
infer IPv6 support from the IPv4 namespace or Docker test.

The Binding responder accepts standard 20-byte requests for interoperability.
Its maximum UDP-payload amplification is **32/20 = 1.6× for IPv4** and
**44/20 = 2.2× for IPv6**. With FINGERPRINT it is 40/28 (about 1.43×) and 52/28
(about 1.86×); optional attributes only increase request size. It never returns
more than one response, retransmits, or serves TURN. The existing 20 responses
per source IP per second and 1000 responses globally per second remain, with
at most 1000 tracked IPs and 512-byte request parsing. No unbounded state or
application content enters STUN. Requiring padded requests would break minimal
standard WebRTC Binding discovery, so #117 documents this bounded ratio instead.

## Naming cutover: manual actions reserved for Léo

After review and #105 qualification, before the first approved release:

- GitHub `production`: configure `CAIRN_BEACON_ORIGIN=https://cairn.build`,
  `COOLIFY_URL`, the new Beacon `COOLIFY_SERVICE_UUID`, and private `COOLIFY_TOKEN`;
  keep environment approval required. Replace the former product-prefixed variable
  names rather than providing aliases.
- Coolify: use the production Compose above and configure its `CAIRN_BEACON_*`,
  `CAIRN_INSTALLATION_IMAGE` and `CAIRN_PROXY_*` values. The paired images are
  `ghcr.io/leo91000/cairn` and `ghcr.io/leo91000/cairn-beacon`; approve only validated
  digests from the same Git tree. Replace the old service and timers as described
  in step 2; no production configuration is changed by this PR.
- Resend: verify `cairn.build`, authorize the new sender `Cairn <cairn@cairn.build>`
  and put its sending key in `CAIRN_BEACON_EMAIL_KEY`. The email wordmark is served
  by Beacon at `/brand/cairn-wordmark.png`.
- OAuth: rename the displayed product Cairn and configure the Google/GitHub
  homepage, origins and callbacks in step 4. Preserve the separation between
  account sign-in and coding-agent repository credentials.
- Android/Firebase: register a **new** app `build.cairn.app`, reinstall the app,
  configure the four `CAIRN_ANDROID_FIREBASE_*` public workflow variables and
  repository `CAIRN_BEACON_ORIGIN`; register the approved signing certificate in
  Firebase/Google and `CAIRN_BEACON_ANDROID_CERTIFICATES` for `assetlinks.json`.
  Prepare the signing key with alias `cairn-android` (the prior key must be
  re-aliased or a new key provisioned; changing an environment variable does not
  change a keystore alias).
- GitHub repository: **Léo** renames `leo91000/leo-agent-manager` to
  `leo91000/cairn`. Then update the repository references listed in
  [naming exceptions](NAMING-EXCEPTIONS.md). GitHub redirects old links; the image
  paths already use Cairn and remain independent of the repository name.

No DNS, provider setting, repository rename, release or deployment is performed
by implementing #122/#132.

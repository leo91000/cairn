# Production configuration on cairn.build

Reference for provisioning or changing the host, DNS/TLS, account providers,
Coolify and installation networking. These are configuration tasks; an ordinary
application release keeps the existing setup and follows the
[release runbook](PRODUCTION-CAIRN.md). Infrastructure changes need authorization
for their own scope.

The [v0.53 migration archive](archive/CAIRN-V0.53-MIGRATION.md) records the initial
cutover and retirement of the old public manager. It is not a setup prerequisite
for an installation already using Beacon.

## Host and release images

Choose a fully successful `Quality and container` run for the intended Git tree.
Its schema-3 `validated-image-<tree>/image.json` records both immutable digests,
the tested commit, tree, repository, run ID and installation CLI versions. Use:

- `CAIRN_BEACON_IMAGE=ghcr.io/leo91000/cairn-beacon@sha256:<beacon digest>`;
- `CAIRN_INSTALLATION_IMAGE=ghcr.io/leo91000/cairn@sha256:<installation digest>`.

Take both from **the same evidence**, never from a moving candidate, `latest`, or
two independently chosen releases. A reused PR image reports its tested merge
commit, whose Git tree matches the release, rather than a later tag commit.
The beacon image contains `cairn-beacon` and the matching `pnpm build` output;
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

Reserve capacity for Coolify/Traefik, the beacon process, Postgres and Garage
before assigning VM budgets. The beacon Compose caps each of beacon and
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
16 GiB free **in addition to** Postgres and conversation disks. See
[installation prerequisites](INSTALLATION.md). KVM is for the installation,
not the beacon process. Do not give the beacon container privileged mode,
KVM, the Docker socket or installation volumes.

## DNS and TLS

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

## Email, OAuth and push

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

**Publish the Google consent screen.** Until it is published, the Google project
`cairn-e2bea` stays in *Testing*: only its listed test users can continue with
Google. Publish it after a Beacon release serving the privacy policy is live:

1. Check that `https://cairn.build/privacy` loads in a private window without
   signing in, and that the sign-in screen links to it.
2. In Google Auth Platform → **Branding**, set the application home page
   `https://cairn.build`, the privacy policy link `https://cairn.build/privacy`,
   and the authorized domain `cairn.build`.
3. In **Data access**, keep only `openid` and `.../auth/userinfo.email`: Beacon
   requests `openid email` and nothing else. Adding a scope requires updating the
   privacy policy first.
4. In **Audience**, publish the app to *In production*, then check that an account
   outside the test users can continue with Google.

The policy's contact address `privacy@cairn.build` is configured separately.
Update the policy text and its date in `apps/web/src/views/PrivacyPolicy.vue`
whenever Beacon's stored data, retention or providers change.

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

## Coolify service and trusted proxy

Use `deploy/beacon/compose.production.yaml`, distinct from the development
`compose.yaml`. It runs beacon + Postgres, a persistent Postgres volume,
healthchecks, rotated logs, explicit budgets and **one beacon process**.
Keep stop-first replacement; never run old and new beacon processes together
or add a second application replica
([single-relay-process decision](adr/0032-single-official-relay-process.md)).

Connect only beacon to the dedicated external proxy Docker network configured
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
| `CAIRN_BEACON_IMAGE` | Validated beacon digest from the same release evidence |
| `CAIRN_INSTALLATION_IMAGE` | Paired installation digest from the same evidence |
| `CAIRN_BEACON_POSTGRES_PASSWORD` | Strong generated private password |
| `CAIRN_BEACON_DATABASE_URL` | `postgres://cairn:<URL-encoded same password>@postgres:5432/cairn_beacon` |
| `COOLIFY_PROXY_NETWORK` | Dedicated external bridge with persisted static proxy IP, configured below |
| `CAIRN_BEACON_TRUSTED_PROXIES` | The persisted static Traefik peer IP (`/32` or `/128`), plus only necessary controlled proxy hops |
| `CAIRN_BEACON_EMAIL_FROM`, `CAIRN_BEACON_EMAIL_KEY` | Verified sender and private Resend key |
| Optional OAuth/VAPID pairs and Android/FCM | [Email, OAuth and push](#email-oauth-and-push), or leave empty |

The beacon browser origin is fixed to `https://cairn.build` by Compose.
Postgres is on a separate internal network, with no published port. Beacon has
only `expose: 4311`, no host port.

### Persist a fixed Traefik peer across recreation

Do not copy today's dynamic IP from the shared `coolify` network into trust.
Use a dedicated external bridge for the beacon/proxy connection, and persist
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
Only beacon and the controlled proxy should join the dedicated bridge;
Postgres stays on its internal database network. Leave Traefik's incoming
forwarded-header trust at its secure default for this DNS-only deployment.

After Léo approves proxy changes, save and recreate it through Coolify, then
verify its address survived, with the actual proxy container name:

```sh
proxy_container=coolify-proxy
docker inspect --format '{{with index .NetworkSettings.Networks "cairn-beacon-proxy"}}{{.IPAddress}}{{end}}' "$proxy_container"
```

The output must equal the saved host address (`172.30.113.2` in this example).
Verify the beacon container is on that bridge and its network label selects
it. Verify sign-in and rate limits through HTTPS. Repeat this check after every
proxy recreation, network change or Coolify update; if Coolify regenerates its
main proxy configuration, reapply the saved static attachment **before** Cairn
traffic resumes. Never widen trust to fix a mismatch. Sources:
[Coolify main proxy configuration](https://coolify.io/docs/core/networking/proxy/traefik/overview),
[Compose static network addresses](https://docs.docker.com/reference/compose-file/services/#ipv4_address-ipv6_address).

**Lessons from the first deployment (2026-10-10).**

- Recreating `coolify-proxy` outside Coolify drops the networks that Coolify had attached
  to it dynamically (`docker network connect`), one per service. Every service routed through
  Traefik then times out. After any manual recreation, reconnect the proxy to each service
  network before resuming traffic, and check every routed host:

  ```sh
  # Coolify names each service network after the service UUID (24 lowercase characters).
  for n in $(docker ps --format '{{.Networks}}' | tr ',' '\n' | grep -E '^[a-z0-9]{24}$' | sort -u); do
    docker network connect "$n" coolify-proxy 2>/dev/null || true
  done
  ```
- In the Beacon Coolify service, disable **Escape special characters in labels**
  (`is_container_label_escape_enabled=false`). Otherwise Coolify writes the `${...}` label
  expressions literally, and Traefik reports the router as using a nonexistent entrypoint
  and resolver.
- Beacon's labels only define the HTTPS router. Plain HTTP for `cairn.build` is
  redirected by the dynamic file `/data/coolify/proxy/dynamic/cairn-build-redirect.yaml`
  (router `cairn-build-http` on entrypoint `http`, `redirectScheme` to `https`, service
  `noop@internal`).
- Coolify also attaches Postgres to the service's own network, which the proxy joins.
  Postgres stays unpublished and password-protected, but it is not limited to the internal
  database network as intended above.
- `/health` and STUN can be checked from the host itself; a client with outbound UDP blocked
  cannot validate STUN.

**Proxy access logs.** `coolify-proxy` runs Traefik without `--accesslog`
(checked on 2026-10-10), so it keeps no access log, and the privacy policy says
so. Do not enable the proxy's access logs without first updating `/privacy`
(`apps/web/src/views/PrivacyPolicy.vue`) and its date.

Start the new service only after Léo's approval. Verify `/health` reports the
expected tested commit, `/install/release` reports the paired digest, `/` and
`/claim` serve the SPA with framing protection, and an email-code sign-in works.
Use the compatibility checks and failure procedure in the
[release runbook](PRODUCTION-CAIRN.md#failed-releases) for subsequent updates.
Releases create no backups. Automatic checksum-checked migrations run on startup; do not reuse a historical development database.

Configure GitHub's **production** environment:

| Setting | Kind | Value |
| --- | --- | --- |
| `CAIRN_RELEASE_COOLIFY_TOKEN` | Secret | Dedicated API token with read, read:sensitive, write and deploy abilities |
| `COOLIFY_URL` | Variable | Existing Coolify HTTPS origin |
| `COOLIFY_SERVICE_UUID` | Variable | **New beacon Compose service** UUID |
| `CAIRN_BEACON_ORIGIN` | Variable | `https://cairn.build` |
| `CAIRN_RELEASE_SSH_KEY` | Secret | Dedicated key restricted to the root-owned release helper |
| `CAIRN_RELEASE_SSH_HOST` | Variable | Installation/Beacon host name or address |
| `CAIRN_RELEASE_SSH_USER` | Variable | Host account carrying the forced-command key |
| `CAIRN_RELEASE_SSH_KNOWN_HOSTS` | Variable | Verified SSH host key entry; never discover it during deployment |

Keep the environment's deployment branch/tag policy restricted to the intended
release refs. Routine stable tags deploy automatically, without required
reviewers, after data compatibility checks succeed.
Enable this only after the host helper, SSH identity and new workflow are installed;
approve any older pending release separately after its own compatibility checks.
Use a fresh `CAIRN_RELEASE_COOLIFY_TOKEN` credential for this workflow, then remove
the old environment secret `COOLIFY_TOKEN` before removing required reviewers.
Historical workflows lack the current release checks and must no longer have a usable
deployment credential when rerun. Keep the new token in the operator password
manager as well as the production environment secret.
Remove the obsolete `CAIRN_PUBLIC_URL` deployment variable; the CLI no longer
deploys a standalone public manager. Main/PR builds never deploy.
A tag promotes both tested digests, updates `CAIRN_BEACON_IMAGE` and
`CAIRN_INSTALLATION_IMAGE` through the bulk endpoint, reads both literal values
back, and repairs partial updates before restart. Failed repair restores the
previous pair without restart; if even restoration fails, freeze all manual
restarts and repair both values to one validated pair before retrying. The script
then verifies both
its build identity and approved installation release. No automatic beacon
rollback is attempted after a failed rollout; use the
[rollback procedure](PRODUCTION-CAIRN.md#backups-and-rollback).

### Read-only release verification host

The host needs Python 3 and Docker. Install `deploy/beacon/release-verify.py`
as the root-owned `/opt/cairn-release/release-verify.py`. The CI identity must
not be able to modify the helper or its configuration. Create
`/etc/cairn-release.json`, mode 0600:

```json
{ "installation_project": "cairn-installation" }
```

Authorize the dedicated CI key with all SSH forwarding disabled:

```text
restrict,command="sudo -n --preserve-env=SSH_ORIGINAL_COMMAND /usr/bin/python3 /opt/cairn-release/release-verify.py" ssh-ed25519 <dedicated-CI-public-key>
```

The account needs passwordless permission only for that exact root-owned helper.
It accepts only `verify <immutable-digest>`; it checks both running images and
manager execution readiness. It cannot stop containers, create or download
backups, or open a shell. Installation updates remain owned by their supervisor.

When replacing the former backup helper, update its forced SSH command and exact
sudoers rule together, validate sudoers, and test the read-only verifier. Remove
`/opt/cairn-release/release-backup.py`, `postgres-backup.sh`,
`/etc/cairn-release-backup.json` and its recipient file after the switch.
Remove release backup archives from the host, local operator copies and GitHub
Actions recovery artifacts. Keep deployment SSH credentials: they authenticate
readiness verification and are independent of the removed backup encryption key.

## Install and claim on the same server

Sign in at `https://cairn.build`, choose **Add an installation**, then run its
complete one-command installer on this same host within ten minutes. It creates
`/var/lib/cairn-installation` with fresh manager, runner and Garage data and starts
`cairn-installation-update.timer`. No installation service publishes a host port;
manager/runner/Garage communicate over their private Compose network.

The host **and its containers** must resolve and reach `https://cairn.build:443`
via the server's public address and TLS proxy, even though beacon is on the
same machine. Verify DNS, firewall/hairpin routing and the certificate from both
contexts. Do not replace the beacon origin with `localhost`, `http://beacon`
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
existing health verification and rollback. Updating beacon does not directly
restart the installation. Do not start a second old installation on these mounts.

## STUN and direct installation connectivity

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
port check is described in [NETWORK-BENCH.md](NETWORK-BENCH.md). Qualify the actual
firewall and source preservation when provisioning or changing the network path.
The initial qualification was tracked in #105/#113; an unrelated application
patch does not repeat that migration checklist.

The installation still publishes **no UDP port**. Its Compose NAT generally
preserves outgoing ports and permits matching reply tuples. A host firewall or
security group denying unsolicited UDP can prevent peer-reflexive discovery,
especially against a symmetric-NAT client; HTTPS relay fallback is expected.
`symmetric-client` in the bench models this installation NAT against a client
with destination-dependent mappings. No host-network or fixed-port opt-in is
introduced, preserving ADR-0028's no-incoming-port decision.

When installation and beacon STUN share a host, the installation's query to
`cairn.build:3478` may hairpin through Docker and expose a Docker gateway as its
reflexive address. `same-server` reproduces that error: the external client sees
its actual NAT address, while the installation sees `10.102.2.1`. For an explicitly
verified, **port-preserving** installation NAT, set `CAIRN_DIRECT_PUBLIC_IP` to the
host's public unicast IP in the installation environment. The peer retains its
bound host candidate. Its `cairn-public` alias uses the same ephemeral port and
is sent through authenticated signaling only after that exact UDP socket has
successfully sent a real ICE Binding check to a numeric remote UDP candidate of
the same family, from the offer or trickle signaling. Private IPv4/unique-local
IPv6 destinations cannot release the public alias. This ordering creates the
outgoing NAT mapping before the client learns the alias: otherwise the client's
first check can reserve an unanswered host conntrack tuple and force Docker
MASQUERADE to allocate a different source port (#163).

The alias window is five seconds from peer setup. Without an eligible remote
candidate and a successful socket send in that window, no alias is advertised
for this negotiation, including if a candidate arrives later. Ordinary ICE
candidates continue immediately; the handshake still has its existing 30 s
limit and the relay remains usable. STUN discovery alone does not prepare this
peer-specific mapping. This opens no listener, incoming port mapping or host
network mode. The `same-server` bench delays browser trickle signaling to make
an eager alias receive the client's check first, observes the kernel's local
and public ports throughout negotiation, and requires an authorized DataChannel
plus preserved ports after the correction.
It cannot repair a NAT that changes the public port; leave direct disabled or use
the relay on such a host until qualification establishes a supported mapping.
A wrong setting only makes direct fail; it never relaxes grant or DTLS validation.
`CAIRN_DIRECT_STUN_URLS` can also select a dedicated beacon STUN endpoint, useful
where a distinct reachable beacon address avoids hairpinning. It must remain
operator-controlled; no automatic third-party fallback is configured.


Before enabling IPv6 STUN, repeat the external-source probe
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

## Optional readable installation hostnames

The slug migration is automatic. Subdomain routing is disabled by default and
requires a separate activation after these prerequisites are ready:

1. Point `*.cairn.build` to the same Beacon proxy as `cairn.build`, including
   correct IPv6 behavior. Do not delegate any child hostname to untrusted hosting:
   the HttpOnly session cookie is shared across this domain.
2. Prepare a Traefik DNS-01 certificate resolver for `cairn.build` and
   `*.cairn.build`. An HTTP-01 resolver cannot issue the wildcard certificate.
   Supply its existing name as `CAIRN_PROXY_WILDCARD_CERT_RESOLVER`; credentials
   belong in the proxy’s secret configuration, never in this repository.
3. Merge `deploy/beacon/compose.subdomains.yaml` with the production compose file.
   Review the rendered configuration before deploying. The overlay retains the
   single Beacon service and adds the wildcard route, certificate names and
   `CAIRN_BEACON_INSTALLATION_SUBDOMAINS=true`.

```sh
docker compose -f deploy/beacon/compose.production.yaml \
  -f deploy/beacon/compose.subdomains.yaml config
```

Keep `CAIRN_BEACON_ORIGIN=https://cairn.build`. OAuth registrations, passkey RP ID,
MCP endpoints, push service worker and installation enrollment remain canonical.
The proxy must preserve Host. Unknown or unclaimed installation hostnames return
404; malformed hosts return 421. Session deadlines and identity proofs are
preserved when cookies migrate to the shared domain.

To roll back routing, remove the overlay or set the flag to `false` and deploy.
Keep wildcard DNS/TLS and the proxy hostname route temporarily so existing child
links can redirect to `/installations/{slug}/...` on the canonical origin. Account
session refresh restores the host-only cookie and clears the shared cookie.
Browser drafts are retained in their original per-origin storage; rollback does
not automatically copy drafts created on child origins back to the root.

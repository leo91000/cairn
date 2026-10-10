# Deployment and recovery

For the production target **https://cairn.build** and a fresh installation on the
same Coolify/Traefik server, follow [the production runbook](PRODUCTION-CAIRN.md).
Deployment requires Léo’s explicit go-ahead.

For the beacon installation flow (one machine, integrated S3, no
incoming ports), see [One-command installation](INSTALLATION.md).

## Required migration to Beacon

This release removes the installation's browser login and application hosting.
Administrator passwords, `SETUP_TOKEN`, local browser sessions and CSRF tokens no
longer grant access. Local MCP OAuth/PAT entry points and public artifact URLs
also stop working; their beacon replacements are delivered separately (#55).
MCP OAuth setup that redirects to the old manager `/oauth/mcp/callback` also
needs an beacon callback route before it can work again; existing stored
outbound MCP credentials remain usable by agents. Old Android clients that use
local login need the separate Beacon sign-in/client migration (#57).
Do not deploy this as a transparent upgrade of the old public site.
**The `.env.example` default follows `:latest`. Pulling that tag after this
release removes local browser access immediately.** Before any pull or automated
upgrade, pin `CAIRN_IMAGE` to the currently verified immutable digest and complete
the migration preparation below. Keep that digest for rollback.

Before upgrading, back up the volumes below and deploy Beacon
with its own Postgres database, email delivery, HTTPS origin and web bundle
([Beacon setup](BEACON.md)). Upgrade its binary and web bundle
together before running the new `cairn claim`: earlier beacon binaries do not
provide the device-review confirmation step. Sign in there. The installation
and Beacon must use compatible relay protocols. Keep one relay process,
and route account/access mutations, installation API requests and relay
WebSockets to it. Revocation notifications are process-local; multiple beacon
replicas are unsupported even with sticky installation routing. See
[beacon process limits](BEACON.md#configuration) and
[ADR-0032](adr/0032-single-beacon-relay-process.md) for this explicit restriction
of spec #43 and the tolerance of persisted revocation checks.

For an existing installation, preserve all volumes, add `CAIRN_BEACON_ORIGIN`
to the manager environment, upgrade the image, then run:

```sh
docker compose exec manager cairn claim
```

The command prints the beacon `/claim` URL and a temporary device code. Open
that URL, sign in to the Cairn account that will own the installation, and approve
only the code displayed on your own machine. Wait for the command to confirm
success, then restart the manager to load its new relay identity:

```sh
docker compose restart manager
```

The restart uses the usual execution recovery; do it at a suitable maintenance
window. No automatic migration of browser accounts, grants or installations is
provided. Alternatively, reinstall with fresh volumes and claim the new
installation. **Discarding old volumes discards their data**: retain backups
until the replacement has been verified. The one-command installer includes local
S3 setup; this manual Compose procedure still needs existing S3 configuration.

After migration, use the beacon site. Verify the installation appears, a
conversation can be read and modified, and access returns after a manager
restart. Direct `/api/session`, `/api/setup`, `/api/login`, static pages, local
OAuth, local `/mcp` and public artifact requests must be refused. Remove obsolete
setup secrets and the installation's public browser domain/proxy. `PUBLIC_URL`
is now an optional **internal manager origin** for execution nodes, defaulting to
loopback; it is not a browser address or a relay prerequisite. Agents in VMs
reach run-scoped MCP through their [VM-local channel](MICROVMS.md#vm-local-mcp-channel),
not through this origin.
For additional nodes use a reachable private LAN/VPN origin and keep the node
channel available. Agent execution and node traffic continue during an beacon
service outage, while browser access is unavailable.

The read-only `/health` readiness probe and authenticated deployment lease remain
operational endpoints. `/internal/nodes`, disk/workspace/execution channels and
run-scoped `/mcp-workspace` and `/mcp-gateway` retain their own machine credentials;
they do not accept browser sessions or local personal tokens. Keep port 4310
private, only reachable by runners/nodes and deployment health checks.

Detaching or definitively revoking an installation requires an email/passkey
proof from the last five minutes in the calling session; use **Confirm identity**
in its confirmation form.

Detaching an installation revokes access and disconnects the active tunnel while
preserving the beacon installation record and its local data. Deleting its
owner account has the same effect. To claim it again, run `cairn claim` on its
machine; successful approval rotates and atomically replaces the private identity
file. A still-owned installation cannot be claimed by another account. A failed
or expired claim keeps the existing identity file intact. Remove a used
`CAIRN_INSTALLATION_CLAIM_CODE` from the environment. Protect
`/data/installation-relay/identity.json` like node credentials and never publish it.

If recovery is interrupted after the service attached the owner but before the
machine saved its new token, detach that installation in the app and run
`cairn claim` again with the existing private file. The service retains the proof
used to start recovery separately from the tunnel credential, so the same
installation record remains recoverable; that proof never reconnects a tunnel.
A still-owned installation must be detached before any recovery attempt.

If the identity file is missing/corrupt (or an initial claim response was lost
before any identity was saved), choose **Revoke and forget installation** in the
beacon app. This also works when it is offline and permanently invalidates old
credentials and recovery proofs. Stop the manager, back up the entire private
`installation-relay` directory outside the deployment volumes, remove that
directory, and run `cairn claim` with `CAIRN_BEACON_ORIGIN`. This creates a new
beacon installation ID while preserving existing data volumes.
Retain backups until access has been verified. Use this **Revoke and forget**
path after machine-token theft too: the thief may rotate first, making the old
local token unusable for `cairn rotate-token`.

To renew a credential while retaining the installation ID and its sharing,
run `cairn rotate-token` with the deployment's usual `DATA_DIR`, then restart the
manager. Retry the same command if its response is lost; retain `rotation.json`
until the command succeeds. See [credential renewal](INSTALLATION-RELAY.md#renewing-a-machine-credential).

### Roll back this access migration

This ticket makes **no installation SQLite schema migration**. To restore the
previous access model, stop the new manager, set `CAIRN_IMAGE` back to the exact
previous verified image digest for both manager and runner, then run
`docker compose pull && docker compose up -d`. Keep all four existing volumes.
For Coolify, restore that same digest in `CAIRN_IMAGE` and restart the service.
Restore the previous manager origin/proxy and its old setup configuration if
removed; the previous image reads the preserved administrator/session records.
Check its `/health` commit and local sign-in before declaring rollback complete.
This deliberately restores the old local-access behavior. The private relay
identity file can stay on disk; the old image ignores it. Detach the installation
in the beacon app to revoke its relay identity if abandoning the migration.

The beacon Postgres migrations are separate and remain applied: do not run an
older beacon binary against that database without its matching backup. If a
later installation release has changed SQLite, follow the image-and-backup
rollback rule below instead of assuming this procedure is still sufficient.

## Additional execution nodes

In the beacon app, open the installation’s **Nodes → Add a machine** screen.
Supply the **Direct manager address** reachable from that machine: a LAN address,
VPN hostname or optional public HTTPS origin, including its port. The browser
continues to use the beacon relay; the additional node connects directly to
this address. No public domain is required. The manager needs a listener or
reverse proxy reachable from that LAN/VPN. The one-command installation keeps
its manager on the private Docker network; expose only the manager’s node
channel through a TLS proxy on your chosen private interface. For Compose,
keep its loopback binding behind that proxy rather than exposing plaintext 4310.
Use a certificate trusted by the node host and its container. Self-signed
certificates are not silently trusted.

HTTPS remains mandatory outside loopback, even on a private network or VPN.
HTTP on loopback is the existing development/test exception, which does not
send credentials across the network. Node identities and enrollment codes
remain stored as hashes on the manager; enrollment codes expire and can be
consumed only once. The address is connection configuration, not authorization.
Disk blocks travel over the authenticated direct node channel or to configured
S3 storage, never through the beacon relay.

The chosen address is visible as an argument in the generated command:
`curl …/internal/nodes/install.sh | sudo bash -s -- 'https://manager.vpn.example'`.
The installer refuses URL query parameters that could hide a different download
origin, and validates its argument before downloading the supervisor or changing
the host. A usable configured manager origin remains the default for older
commands without an argument; a Docker-only origin requires the explicit address.
The address is saved in the node’s protected identity and supervisor configuration. The node uses
this authenticated connection address for disk preparation and restoration,
even if the manager’s internal origin is loopback. The manager’s optional
`PUBLIC_URL` remains a fallback for older enrollment clients and
manager-local calls; it need not be the browser’s origin. Installers require
`CAIRN_NODE_IMAGE` to pin the manager’s approved image digest. Without it, use the
matching `cairn node-enroll` binary with the address and temporary code shown in
the app. Changes of address after installation require updating the node’s
private identity and supervisor configuration together while its service is
stopped; no credential is displayed by the app.

The node channel accepts the Host preserved by a private TLS proxy independently
of `PUBLIC_URL`. Enrollment still needs its single-use code; node traffic and
disk reads still require their own bearer credentials and grants. Browser Origin
checks remain active, and other routes retain their Host checks. The proxy must
serve only the node channel, not an installation browser interface.

## Docker on a VPS

Install Docker Engine and the Compose plugin. Clone this repository on the server,
copy `.env.example` to `.env`, and configure `CAIRN_BEACON_ORIGIN`.
Keep one manager replica per SQLite data volume. If using a prefilled claim code,
set `CAIRN_INSTALLATION_CLAIM_CODE` and `CAIRN_INSTALLATION_NAME` before first startup;
otherwise use `cairn claim` as above.

For a local build, resolve current stable CLI versions and pass them to Docker:

```sh
codex_version=$(npm view @openai/codex version)
gh_version=$(gh api repos/cli/cli/releases/latest --jq '.tag_name | ltrimstr("v")')
docker compose build --build-arg CODEX_VERSION="$codex_version" --build-arg GH_VERSION="$gh_version"
docker compose up -d
```

For published images, use
`docker compose pull && docker compose up -d` and pin `CAIRN_IMAGE` to a verified
`ghcr.io/leo91000/cairn:sha-<full-commit>` tag for controlled upgrades.
Port 4310 is private machine traffic. Beacon terminates browser
access and receives the installation's outbound relay connection.

The Compose file persists:

| Volume | Container path | Contents |
| --- | --- | --- |
| `data` | `/data` | SQLite, private relay identity, run results and worktrees |
| `agent-home` | `/home/node` | Codex/GitHub authentication, Git configuration, global skills |
| `workspaces` | `/workspaces` | Project clones and project skills |
| `runner-state` | `/runner-state` | Persistent container stop markers in the runner |

The image runs as UID/GID 1000. Bind mounts need matching ownership. Do not run
`docker compose down -v` on a live installation: it deletes persistent volumes.
The Compose defaults give the application two CPUs and 4 GB of memory; adjust these
for the projects your agents build. Container logs rotate independently of run logs.

## Cairn account and CLI accounts

Complete the claim above, then open the installation from your Cairn account on the
Beacon site. Agent CLI accounts remain separate from your Cairn account. Keep
tokens and account files out of Git, images, screenshots and support logs.

The **Connections** screen provides official CLI device sign-in. If a provider
requires terminal interaction instead, run:

```sh
docker compose exec manager codex login --device-auth
docker compose exec manager gh auth login --hostname github.com --git-protocol https --web
docker compose exec manager gh auth setup-git
```

Refresh Connections afterward. Codex must report a ChatGPT subscription login.
The worker removes API-key overrides and sets `forced_login_method="chatgpt"` for
runs. Your VPS gets its own persistent login; it does not depend on your laptop.
Provider session expiry or usage limits can still cause a run to fail; inspect the
run result and reconnect when necessary. A schedule cannot guarantee that every
external task succeeds.

Set the Git identity you want agents to use before tasks create commits:

```sh
docker compose exec manager git config --global user.name 'Your name'
docker compose exec manager git config --global user.email 'your-address@example.com'
docker compose exec manager gh repo clone OWNER/REPOSITORY /workspaces/REPOSITORY
```

Register `/workspaces/REPOSITORY` in Projects. Its base branch must already exist
locally. Isolated runs branch from that local ref; tasks that need current remote
state should fetch and update their isolated branch according to their instructions.
The manager does not reset or update your primary checkout automatically.
For a partial clone, workspace preparation downloads missing Git objects from its
configured promisor remote before completing the independent run clone. That remote
must be reachable using the manager's Git credentials. Register only trusted source
checkouts: Git's lazy fetch uses the source repository's configuration and hooks.
This exception is scoped to workspace cloning; it does not enable lazy fetching
globally or run Git commands against a completed agent's clone during cleanup.

The image includes Node 24, pnpm 12, Git, GitHub CLI, Codex CLI, Python, and native
build tools. It also includes the OS libraries required by Chromium, Firefox, and
WebKit. Projects install browser binaries using their own pinned Playwright version
in the persistent worker home. Container CI launches and renders with all three
engines as the normal worker user, without custom library paths. This dependency
layer is cached independently of application code; validated tags reuse the image.
Install project-specific toolchains such as Rust in a derived image
or in the persistent worker home before scheduling projects that require them.
YOLO is the default inside each private Firecracker microVM. All production agents,
including Main, use the VM runner. It requires a Linux x86-64 host with KVM and
TUN; it does not mount the host Docker socket. See [agent scope](AGENT-ACCESS.md)
and [microVM deployment](MICROVMS.md) for privileges, storage and network rules.

## Compressed host swap

VMs share one RAM limit. Without swap, a peak above it stalls every VM for minutes
instead of slowing them down, and their attempts are then interrupted. The node and
installation installers enable zram on apt-based hosts (zstd, a quarter of RAM,
priority 100, `vm.swappiness=100`) and leave an existing zram swap unchanged. On
hosts deployed by hand, such as Docker on a VPS or Coolify, enable it once as root:

```sh
apt-get install -y zram-tools
printf 'ALGO=zstd\nPERCENT=25\nPRIORITY=100\n' > /etc/default/zramswap
echo 'vm.swappiness=100' > /etc/sysctl.d/99-cairn-zram.conf
sysctl -p /etc/sysctl.d/99-cairn-zram.conf
systemctl enable zramswap && systemctl restart zramswap
swapon --show
```

`swapon --show` must list `/dev/zram0`. The runner container must allow swap: keep
the default `memswap_limit` rather than setting it equal to `mem_limit`.

## Coolify

Create a Compose service from `compose.yaml`, using the published GHCR image for
both manager and runner. Configure the beacon origin and claim the manager as
above. Preserve the persistent mounts, one replica and a 60-second shutdown grace
period. Use `/health` only for the private readiness check. Do not configure a
public browser domain for the installation; serve the beacon application on its
own HTTPS origin. Additional nodes still need a direct private route to port 4310.

### Deploy version tags through GitHub Actions

The production workflow targets the **beacon Compose service**, never the old
public installation manager. The full [cairn.build runbook](PRODUCTION-CAIRN.md)
lists environment setup, DNS, Resend, OAuth, co-location and rollback steps.

Main and same-repository PRs build both installation and beacon images. All
existing quality, browser, network and installation/VM smoke checks remain
publication prerequisites, alongside the exact beacon-image smoke with
Postgres, bundled SPA, sign-in, persistence and readiness checks. Both images
carry SBOM/provenance and share schema-3 evidence for one validated Git tree.
Older single-image evidence cannot be reused. Fork images remain local.

A `v*` tag promotes both immutable digests without rebuilding if a trusted run
validated that exact tree and current installation CLI versions. Otherwise the
full pipeline runs. A tag waits for concurrent main validation for up to 40
minutes; if validation is still pending, retry after it completes. No second
build starts at that deadline. Production deployment jobs remain serialized.
Main, PR and manual workflows never deploy.

The GitHub `production` environment requires the secret `COOLIFY_TOKEN` and
variables `COOLIFY_URL`, `COOLIFY_SERVICE_UUID` (**new Beacon UUID**)
and `CAIRN_BEACON_ORIGIN=https://cairn.build`. Require Léo’s environment approval.
The obsolete `CAIRN_PUBLIC_URL` and standalone-manager deployment target are no
longer used by the CLI. The script refuses a manager/runner Compose target.

The deployment sends `CAIRN_BEACON_IMAGE` and `CAIRN_INSTALLATION_IMAGE` together
to Coolify’s bulk environment endpoint, re-reads both literal values, and repairs
a partial write before restarting. If repair fails, it restores the previous
pair and fails without restart; an unrecoverable API failure requires freezing
manual restarts and repairing both values. The token needs `read:sensitive` to
verify the pair. Only beacon is restarted. It waits up to ten minutes for
`/health` to report the tested image's commit/runtime identity and
`/install/release` to approve the paired installation image. Reused PR images
report the tested merge commit with the released tree. Existing installations
update through their host timer; the check does not wait for every installation.
A failed beacon rollout needs operator rollback of the previous digest and
matching Postgres backup; see [rollback](PRODUCTION-CAIRN.md#7-backups-and-rollback).

Retained VM disks keep their original guest OS and tools. Only approve an
installation image if the previous release can still read its database, or
prepare matching stopped backups before approving it.

## Backups

Back up **all four volumes together**. Stop the manager first so SQLite and Git
worktrees are consistent, make encrypted volume backups with your server backup
system, then restart it. Credentials in the home volume make those backups secrets.
For a live SQLite-only snapshot, Node's `node:sqlite` backup API can create a
consistent database backup, but that does not include working directories or CLI
accounts and is not a complete recovery point.

Restore into fresh volumes while the manager is stopped, preserve UID/GID 1000,
and start the same image version that created the backup. Check beacon claim/relay access, profiles,
tasks, global/project skills, and worktree paths. Database schema versions newer
than the application are rejected rather than silently downgraded. Roll back the
image and its matching backup together when a future migration requires it.

Finished-run events expire after 30 days and audit entries after 90 days. Expired
OAuth/session records are removed hourly. Run summaries and preserved worktrees
are not automatically deleted. Review worktree storage in the run's Task brief;
cleanup refuses changes, including ignored/untracked files, and keeps Git branches.

## Troubleshooting

- **Unexpected host/origin:** check the internal manager origin for node traffic, and the beacon origin for browser access.
- **Project outside workspace root:** use a directory under `WORKSPACE_ROOTS` (colon-separated on Linux), then register its canonical path.
- **Worktree preparation failed:** check that the project is a Git repository and the configured local base branch exists.
- **Recovering:** active conversations resume after the previous process/container stops. If the runner is unavailable, recovery waits and retains project/account locks. **Interrupted:** older runs without a checkpoint need review and an explicit retry. See [restart recovery](RESTART-RECOVERY.md).
- **No CLI installed/signed in:** use Connections and the commands above; provider account credentials are separate from the manager password.
- **MCP rejects initialization:** verify the client uses Streamable HTTP and the deployed image includes stateless compatibility. Both 2026-07-28 and older 2025 clients are supported; standalone HTTP+SSE and stateful sessions are not.
- **Installation detached or owner account deleted:** run `cairn claim` on the machine, approve its device code on the beacon site, then restart the manager. Old local passwords and sessions cannot restore access.

See [Docker's volume documentation](https://docs.docker.com/engine/storage/volumes/)
and [Codex authentication](https://learn.chatgpt.com/docs/auth) for the underlying tools.

## Automatic CLI updates

The historical [standalone CLI update workflow](CLI-UPDATES.md) is explicitly
disabled. Installations update tools through fully validated paired image
releases and their host timer, with runtime verification and rollback. CI
application images also resolve current stable
Codex/GitHub CLI versions before building and validating them.

Codex `initialize` and retained-session `thread/resume` get separate, bounded
two-minute deadlines for cold executable loading and native context recovery.
Other Codex and MCP requests retain
their 20-second deadline. Closing the session or canceling the conversation
interrupts this wait immediately. Slow and failed RPC calls log the method,
elapsed time and deadline, without request parameters or response contents.
This does not change disk validation, synchronization or VM suspension deadlines.

### Local Intel qualification

The main pipeline builds an immutable image and runs the normal backend, browser,
Android and Firecracker checks on GitHub; those checks alone gate publication and
deployment. The nested Android test needs an Intel Linux host with nested KVM, which
GitHub does not provide, so it is not part of CI. Run it locally before a release
that changes Android or the runner. The GitHub-hosted AMD diagnostic
remains available through **Nested Android probe**; it is not release evidence for
Intel. No persistent GitHub runner or GitHub credential is installed on the Intel
host.

Run the checked-out candidate's probe against the **exact digest** printed by the
image job, using a disposable directory and the existing bounded runner container:

```sh
ANDROID_TEST_COMMIT=<full-commit-sha> \
ANDROID_TEST_SYSTEM=aosp \
ANDROID_EVIDENCE=/absolute/private/path/intel.json \
ANDROID_SCREENSHOTS=/absolute/private/path/screenshots \
node tests/android-runner-smoke.mjs ghcr.io/leo91000/cairn@sha256:<digest>
```

This command requires Node and Docker on the test controller; the controller may
itself run in a disposable container. It checks the Intel CPU and the image's
revision label, requires KVM inside Firecracker, and writes evidence only after
both the first interaction and the persistent-device restart pass and cleanup
succeeds. It limits the broker to 7 GiB and three CPUs. Do not expose production
volumes to the test or leave the temporary controller running.

Publish the JSON, logs and screenshots to durable private artifact storage. Then,
from the trusted operator environment (not the Intel host), record the result:

```sh
node scripts/nested-android-validation.mjs record /path/to/intel.json https://<durable-report-url>
```

The recorded commit status documents the result; CI does not wait for it.

Image promotion requires the ordinary CI checks. Tag releases reuse the successful
main pipeline's exact image.

### Task-author security upgrade (#59)

Deploy Beacon before upgrading installations for the task-author
checks. Startup applies the additive `202610072155_task_author_access` migration
with the existing SQLx migrator; historical migration numbers and checksums stay
unchanged. Existing memberships receive access identifiers automatically.
An upgraded manager cannot reconnect to an older beacon binary that lacks
`/api/relay/{installation}/task-authors`: its 404 defers scheduled admissions
and keeps the relay offline. Keep the upgraded Beacon while rolling
back an installation. Rolling back Beacon requires its matching
Postgres backup and compatible managers; older managers also restore the previous
task-author/public-link rules, so suspend shared access during that rollback.

Upgrade every shared installation: older binaries do not enforce the task-author
policy. Upgraded installations require the beacon task-author endpoint before
reconnecting and before admitting scheduled work. During an beacon outage,
new scheduled admissions wait and already admitted work continues. On first
reconciliation, old tasks without authors belong to the current owner. Removed
members' tasks and admitted runs remain available; re-invitation does not resume
old schedules. Public links remain owner-managed until explicitly revoked.

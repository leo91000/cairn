# Deployment and recovery

## Docker on a VPS

Install Docker Engine and the Compose plugin. Clone this repository on the server,
copy `.env.example` to `.env`, and set `PUBLIC_URL=https://agents.example.com`.
The URL must be an origin without a path and must match the address used by browsers
and MCP clients. Keep one manager replica per SQLite data volume.

For a local build, resolve current stable CLI versions and pass them to Docker:

```sh
codex_version=$(npm view @openai/codex version)
gh_version=$(gh api repos/cli/cli/releases/latest --jq '.tag_name | ltrimstr("v")')
docker compose build --build-arg CODEX_VERSION="$codex_version" --build-arg GH_VERSION="$gh_version"
docker compose up -d
```

For published images, use
`docker compose pull && docker compose up -d` and pin `LEO_IMAGE` to a verified
`ghcr.io/leo91000/leo-agent-manager:sha-<full-commit>` tag for controlled upgrades.
Only expose port 4310 through your HTTPS reverse proxy. Preserve the public Host
header; forwarded headers are not trusted as authentication evidence.

The Compose file persists:

| Volume | Container path | Contents |
| --- | --- | --- |
| `data` | `/data` | SQLite, bootstrap token, run results and worktrees |
| `agent-home` | `/home/node` | Codex/GitHub authentication, Git configuration, global skills |
| `workspaces` | `/workspaces` | Project clones and project skills |
| `runner-state` | `/runner-state` | Persistent container stop markers in the runner |

The image runs as UID/GID 1000. Bind mounts need matching ownership. Do not run
`docker compose down -v` on a live installation: it deletes persistent volumes.
The Compose defaults give the application two CPUs and 4 GB of memory; adjust these
for the projects your agents build. Container logs rotate independently of run logs.

For local access through SSH before configuring a domain:

```sh
ssh -L 4310:127.0.0.1:4310 your-server
```

Use `PUBLIC_URL=http://localhost:4310` for that setup route, then change it to the
final HTTPS origin and restart before adding OAuth clients. Existing OAuth grants
are audience-bound and must be reconnected after changing origins.

## First login and CLI accounts

Read `/data/setup-token` through your server terminal and enter it on the setup
screen. The application creates an administrator password, then disables setup.
Keep tokens and account files out of Git, images, screenshots, and support logs.

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

## Coolify

Create a Compose service from `compose.yaml`, using the published GHCR image for
both manager and runner. Set container port **4310**, `PUBLIC_URL` to the
chosen HTTPS domain, and persistent storage mounts for all paths above.
Use the healthcheck path `/health`, one replica, and a shutdown grace period of
60 seconds. Point the domain's DNS record at the selected server and enable TLS.
Do not add an interactive proxy login in front of `/mcp`, `/oauth/*`, or the
well-known metadata endpoints: MCP clients use the application's OAuth flow.

After deployment, check HTTPS, bootstrap/login, Connections, a small task, restart
persistence, and MCP discovery from outside the server. A working local container
does not establish that DNS, TLS, reverse-proxy routing, or cloud connectors work.

### Deploy version tags through GitHub Actions

The `Quality and container` workflow deploys pushes of `v*` tags after quality,
browser, and container smoke tests pass. Main branch pushes validate and publish
images without deploying. A tag at a successful main commit promotes that exact
image digest without rebuilding or repeating the tests. If main CI is still
running, the tag waits for it; missing, failed, expired, or mismatched validation
falls back to the full pipeline. Deployment jobs are serialized.
The wait covers the main image job's time limit. If validation is still pending
after 40 minutes, rerun the tag workflow after main finishes; it does not start a
second full build at that deadline.

For the shortest tag-to-live time, tag a commit whose main CI has already passed.
Pushing main and its tag together also works and shares the validation work.
See [CI performance](CI-PERFORMANCE.md) for measurements and the evidence checks.

The GitHub `production` environment needs:

| Setting | Kind | Value |
| --- | --- | --- |
| `COOLIFY_TOKEN` | Secret | Dedicated Coolify API token with `read`, `write`, and `deploy` abilities |
| `COOLIFY_URL` | Variable | `https://coolify.leo-coletta.fr` |
| `COOLIFY_SERVICE_UUID` | Variable | UUID of the Leo Compose service |
| `LEO_PUBLIC_URL` | Variable | `https://agents.webdns.leo-coletta.fr` |

In the Coolify service's raw Compose, set both `services.manager.image` and `services.runner.image` to
`${LEO_IMAGE}` and create the `LEO_IMAGE` environment variable with the currently
deployed image reference. The workflow updates only that variable to the image's
immutable GHCR digest, requests a service restart, then waits up to ten minutes
for public `/health` to return the tagged commit. It fails if the previous version
is still running, even when that version is healthy. Volumes, domain and CLI
credentials persist across deployments.

The deployment script also adds the persistent `runner-state` volume to older
service Compose definitions before restarting, then verifies it was saved. The
manager data mount remains read-only in the runner. Unsupported custom Compose
layouts stop deployment with an error instead of silently losing stop markers.
For an existing Firecracker runner, deployments preserve the configured memory,
CPU and process budgets, along with the manager's global concurrency setting.
It also sets the manager's `LEO_NODE_IMAGE` from `LEO_IMAGE` in older Compose
definitions and checks that the new master advertises the deployed digest to
remote nodes. Each remote supervisor then downloads that image, drains its runs,
and updates independently; the deployment check does not wait for every remote
node to finish. Production still needs working S3 credentials and `/dev/fuse`
on each node host.

Retained VM disks keep their original kernel, guest OS and installed toolchains.
At each attempt the controller imports its current Leo runner into guest `/run`
and launches that program, so chat adapter fixes also reach old conversations.
This does not replace the saved filesystem or provider session. Custom probe
commands remain unchanged. The controller and retained guest must use compatible
binary dependencies; the published images share the Debian Bookworm runtime.

Automatic placement can move a queued conversation when its original node lacks
CPU or memory and another authorized node has room. The destination is reserved,
the source execution is fenced, and its current disk is published before restore.
Failure retains the source disk; a fixed placement waits for its chosen node.

To release the current main commit, choose an unused version tag:

```sh
git switch main
git pull --ff-only
git tag -a v0.1.1 -m 'Release v0.1.1'
git push origin v0.1.1
```

When the tagged tree was already validated, the tag only promotes and deploys that
image. That validation can come from main or from a pull request of this repository
whose branch contained main's tip when its CI passed. Such an image reports the
pull request's tested merge commit, whose tree is identical to the tag's.
The workflow run's deployment summary records the exact image and verified commit.
If deployment fails, inspect that run and the Coolify service logs; there is no
automatic rollback. For recovery, set `LEO_IMAGE` back to the previous verified
digest in Coolify and restart, accounting for any database migration as described
below. A successful tag run can also be rerun to deploy that version again.

## Backups

Back up **all four volumes together**. Stop the manager first so SQLite and Git
worktrees are consistent, make encrypted volume backups with your server backup
system, then restart it. Credentials in the home volume make those backups secrets.
For a live SQLite-only snapshot, Node's `node:sqlite` backup API can create a
consistent database backup, but that does not include working directories or CLI
accounts and is not a complete recovery point.

Restore into fresh volumes while the manager is stopped, preserve UID/GID 1000,
and start the same image version that created the backup. Check login, profiles,
tasks, global/project skills, and worktree paths. Database schema versions newer
than the application are rejected rather than silently downgraded. Roll back the
image and its matching backup together when a future migration requires it.

Finished-run events expire after 30 days and audit entries after 90 days. Expired
OAuth/session records are removed hourly. Run summaries and preserved worktrees
are not automatically deleted. Review worktree storage in the run's Task brief;
cleanup refuses changes, including ignored/untracked files, and keeps Git branches.

## Troubleshooting

- **Unexpected host/origin:** correct `PUBLIC_URL`, proxy Host preservation, and the browser URL.
- **Project outside workspace root:** use a directory under `WORKSPACE_ROOTS` (colon-separated on Linux), then register its canonical path.
- **Worktree preparation failed:** check that the project is a Git repository and the configured local base branch exists.
- **Recovering:** active conversations resume after the previous process/container stops. If the runner is unavailable, recovery waits and retains project/account locks. **Interrupted:** older runs without a checkpoint need review and an explicit retry. See [restart recovery](RESTART-RECOVERY.md).
- **No CLI installed/signed in:** use Connections and the commands above; provider account credentials are separate from the manager password.
- **MCP rejects initialization:** verify the client uses Streamable HTTP and the deployed image includes stateless compatibility. Both 2026-07-28 and older 2025 clients are supported; standalone HTTP+SSE and stateful sessions are not.
- **Lost administrator password:** restore a known backup or stop the service and remove only the `admin` and `session:*` keys from SQLite through a trusted server terminal, then restart and repeat bootstrap. OAuth grants remain unless separately revoked; preserve a backup first.

See [Docker's volume documentation](https://docs.docker.com/engine/storage/volumes/)
and [Codex authentication](https://learn.chatgpt.com/docs/auth) for the underlying tools.

## Automatic CLI updates

[Daily CLI updates](CLI-UPDATES.md) build and test new Codex/GitHub CLI versions,
then replace both services, pausing and recovering active work, with runtime
verification and rollback. CI application images also resolve current stable
Codex/GitHub CLI versions before building and validating them.
The VPS timer runs independently of your computer and does not change app versions.

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
node tests/android-runner-smoke.mjs ghcr.io/leo91000/leo-agent-manager@sha256:<digest>
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

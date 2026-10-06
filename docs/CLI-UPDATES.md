# Automatic Codex and GitHub CLI updates

The VPS checks daily at 04:23 UTC, with up to five minutes of jitter. It dispatches
the **Update agent tools** workflow using the manager's existing GitHub
login. The computer used to configure it can be off. A systemd timer is used
because GitHub disables schedules in inactive public repositories after 60 days.

The workflow compares installed versions against npm's stable `@openai/codex`
release and the latest stable `cli/cli` GitHub release. It skips unchanged versions,
prereleases, and downgrades. A changed version creates an image from the original
deployed application image. Application code, database version, and UI assets do
not change. Deriving from the original base prevents update layers accumulating.
The manager and isolated agents use the same updated tools.

Candidates retain exact CLI versions. GitHub CLI downloads are checked against
its release checksums; Codex is installed from its versioned official npm package.
Container CI checks actual CLI versions against runtime metadata, launches all
three browser engines, and exercises runner authentication, isolation, sandbox
write denial, output streaming, and cancellation. Failed candidates are not deployed.
Candidate images have `cli-RUN-ATTEMPT` tags; these are not application releases.

Before deployment, a short authenticated lease pauses new task starts. Active
conversations pause during the container restart and resume automatically after
the lease is released, using their saved workspace and session. An active agent
does not defer an update. The workflow still defers if an application release
changed production during the build. Newly queued tasks wait during deployment.
The lease is stored in SQLite, survives container replacement, and expires after
20 minutes if the updater
crashes. An interrupted shell command may need repair after recovery; see
[restart recovery](RESTART-RECOVERY.md).

The updater verifies both the application commit and a unique runtime ID, so an
old container with the same app version cannot falsely satisfy the health check.
It restores and verifies the previous image if deployment fails. If someone has
changed the image externally meanwhile, it does not overwrite that deployment.
Candidate verification and rollback each have a five-minute health deadline,
leaving time within the job timeout and worker lease to restore service.
A provider outage or incompatible release leaves the previous working tools in use;
inspect the failed workflow before retrying.

The update workflow also maintains mise, global runtimes and the agent toolbox.
See [Agent toolkit](TOOLKIT.md) for the catalogue, project overrides and update policy.

## Setup

First deploy an application version that supports runtime metadata and deployment
leases (0.2.1 or newer). The manager creates `/data/maintenance-token`, mode 0600.
Store this value as GitHub production-environment secret `LEO_MAINTENANCE_TOKEN`
without printing it or placing it in logs. This token can only pause/resume task
starts. The updater also uses the existing Coolify deployment secret and variables.
The Coolify token needs `read`, `read:sensitive`, `write`, and `deploy` permissions:
reading the configured image is required for concurrency checks and rollback.
Allow the `main` branch in the GitHub production environment's deployment policy,
alongside the existing `v*` tag rule, because update dispatches run on `main`.

Install `deploy/leo-cli-update.service` and `deploy/leo-cli-update.timer` under
`/etc/systemd/system` on the Docker host. In a service override, set
`LEO_MANAGER_CONTAINER` to the actual manager container name. For forks, also set
`LEO_REPOSITORY`. The manager's GitHub login must be allowed to dispatch Actions
workflows in that repository.

```sh
sudo systemctl daemon-reload
sudo systemctl enable --now leo-cli-update.timer
sudo systemctl start leo-cli-update.service
sudo systemctl list-timers leo-cli-update.timer
sudo journalctl -u leo-cli-update.service
```

The manual start checks immediately. The timer catches up after host downtime.
For local Compose the default container name is `leo-manager`. Coolify installations
use the generated name, such as `manager-SERVICE_UUID`.

To pause automatic updates, run `sudo systemctl disable --now leo-cli-update.timer`.
The workflow can still be triggered manually from Actions. Normal app version tags
continue to deploy their own validated image. Every CI run, including every tag
release, resolves the latest stable Codex and GitHub CLI versions before deciding
whether to reuse an image. There are no default Codex/GitHub CLI version numbers
in the Dockerfile. The resolved versions are passed to the build and recorded in
its validation evidence. Main or a tag reuses the tested image for its exact source
tree only when both tool versions match the latest discovery; otherwise it builds
and tests a fresh image. Tools never change after image validation.
Direct Docker builds must pass the resolved `CODEX_VERSION` and `GH_VERSION` build
arguments. For local Compose, see [deployment setup](DEPLOYMENT.md).
Application version tags remain immutable.

Sources: [Codex CLI](https://learn.chatgpt.com/docs/codex/cli),
[GitHub CLI installation](https://github.com/cli/cli#installation),
[GitHub schedule behavior](https://docs.github.com/en/actions/reference/workflows-and-actions/events-that-trigger-workflows#schedule).

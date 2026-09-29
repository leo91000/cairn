# Leo Agent Manager

A self-hosted Vue control room with a native Rust backend for Codex and Claude Code agents, recurring work, and reusable skills.
Run it on a VPS so schedules keep working when your laptop is off.

![Workspace overview](docs/screenshots/overview-desktop.png)

## What it does

- Choose Light, Dark, or System appearance, with a saved browser preference and automatic device changes.
- Create agent profiles with model, reasoning, instructions and time limits.
- Register project directories; run agents in private Firecracker microVMs with persistent workspaces.
- Schedule daily, weekly, or custom cron tasks with timezone previews and overlap protection.
- Follow runs, inspect results and original instructions, cancel, retry, archive tasks, and clean up reviewed worktrees.
- Edit global and project `.agents/skills`, including supporting files and Markdown previews. In chats, type `$` to pick a skill with autocomplete on web and Android; both Codex and Claude Code apply the invoked skill.
- Connect multiple ChatGPT accounts through Codex device sign-in. Runs select available capacity automatically, resume across account exhaustion, and show live usage and reset windows. GitHub keeps its own device sign-in.
- Store multiple 1Password service account tokens and explicitly grant each agent read access on web or Android. See [1Password setup and access](docs/ONEPASSWORD.md).
- Add remote and command MCP servers from the UI, sign in with OAuth on any device, discover tools, and choose each agent’s connections and tool access.
- Expose scoped tools through a **stateless MCP 2026-07-28** endpoint, with stateless compatibility for older Streamable HTTP clients, OAuth, PKCE, refresh rotation, and revocation.

The application serves one private owner workspace. The repository is public;
your credentials, projects, task instructions, and run history stay on your server.
There is no external telemetry or font/CDN dependency. Storage and VM performance
timings stay in local service logs; disk I/O counters are available in storage status.

Appearance is available from the top bar, the sign-in screen, and **Settings → Appearance**.
[Theme implementation and browser QA](docs/THEMES.md) cover the palette and checks.

## Run with Docker

The production runner requires a Linux x86-64 host with KVM. Docker deploys the
manager and trusted VM controller; agents execute inside Firecracker microVMs.
See [microVM requirements and architecture](docs/MICROVMS.md).

```sh
cp .env.example .env
# Set PUBLIC_URL to your HTTPS origin when deploying behind a reverse proxy.
docker compose up -d --build
# Read the generated bootstrap token locally; enter it in the setup screen.
docker compose exec manager cat /data/setup-token
```

Open `http://localhost:4310`, create your administrator password, then visit
**Atelier → Connections, Agents and Projects**, then **Missions**. The Compose port is bound to localhost.
For a remote server, use an HTTPS reverse proxy or an SSH tunnel for initial setup.

[Deployment, CLI login, project setup, backups, and Coolify](docs/DEPLOYMENT.md)

[Agent toolkit, mise project versions, and automatic updates](docs/TOOLKIT.md)
provide the complete installation procedure. [MCP and OAuth](docs/MCP.md) explain
ChatGPT/Claude connection setup and the supported protocol boundary.

[Coding-agent accounts](docs/AGENT-ACCOUNTS.md) explains how Codex and Claude Code accounts are added, selected by remaining usage, run in parallel, and hand sessions over when one runs out.

[Claude Code](docs/CLAUDE-CODE.md) covers provider selection, hosted sessions, and permissions.

Chat supports images and files through the attachment button, drag-and-drop, or
pasting an image. Preview images before sending and in the conversation. Attach
up to eight files per message (10 MB each, 40 MB combined; 200 MB per chat).
Attachments work with image-only messages, queue editing, steering, and restart
recovery. PNG, JPEG, WebP, and GIF images go directly to Codex as image inputs;
other files are available for the agent to inspect with its tools. Downloads
require sign-in, and sandboxed runs receive private copies through their existing
private guest inbox.

[MCP connections for agents](docs/MCP-CONNECTIONS.md) covers outbound servers,
OAuth setup, tool permissions, and credential backups.

## Develop with pnpm 12

Use Node.js 24.12+ and **pnpm 12.3.4** (pinned in `package.json`).
Install Rust through rustup; `rust-toolchain.toml` pins the compiler and checks.

```sh
pnpm install --frozen-lockfile
pnpm dev
```

`pnpm install` installs a pre-commit hook that runs `pnpm lint:fix` across the
full project, including `cargo fmt --all` for Rust. If auto-fixes change tracked
files, review and stage those fixes, then retry the commit; the hook does not
stage files automatically. It then runs `cargo check` and Clippy with the locked
dependencies across every workspace member and target, including tests and
examples. Lint errors, compiler errors, and Clippy warnings block the commit.
Run `pnpm prepare` to reinstall the hook.

The UI is at `http://localhost:5178`; the backend is at `http://localhost:4310`.
The UI uses Tailwind CSS and Egoist's Iconify plugin. See the
[styling conventions](docs/UI-STYLING.md) for shared controls, theme tokens, icons,
and responsive layout rules.
By default, the worker uses your home directory and existing CLI accounts. Set
`AGENT_HOME`, `DATA_DIR`, and `WORKSPACE_ROOTS` for a separate environment. See
[.env.example](.env.example); environment variables must be exported for local CLI
runs (`.env` is used by Compose, not loaded automatically by the development server).

```sh
pnpm check                       # ESLint, rustfmt, types, JS tests and frontend build
pnpm test:backend                # Native backend integration and migration tests
cargo clippy --all-targets -- -D warnings
cargo build --bin leo            # Native binary used by every browser fixture
pnpm exec playwright install chromium
pnpm test:e2e                    # Full browser journeys against the Rust backend
pnpm build:backend               # Optimized native production binary
node --import tsx scripts/benchmark-backend.mjs # Node/Rust comparison
pnpm lint:fix                    # ESLint fixes and Rust formatting
```

Use `pnpm test:backend` inside a coding-agent environment: the test launcher clears
inherited authentication sockets and credential overrides. Vitest and Playwright
apply the same isolation before starting their fixtures.

Browser tests start a separate application, isolated home, and fixture project;
they never use your Codex credentials. The fixture runner is only in the test suite
and is absent from the production image.

[Rust backend architecture and migration](docs/RUST-BACKEND.md) describes the
runtime, database compatibility, execution supervision and performance evidence.

## Operations and boundaries

- Run **one application process per data volume**. SQLite persists the queue and sessions.
- One active run per task, one executing run per project, and 1–4 global workers.
- After downtime, schedules catch up once. Active conversations resume automatically with their saved workspace and remaining timeout. Explicitly stopped runs stay stopped and can be resumed from the run view. See [restart recovery](docs/RESTART-RECOVERY.md).
- Retry uses the task's current configuration; the previous run's snapshot stays intact.
- Worktrees preserve changes for review. Cleanup rejects dirty/untracked files and preserves Git branches.
- Event output is bounded per run and expires after 30 days for finished runs; audit entries expire after 90 days. Run summaries and worktrees remain until deliberately managed.
- Tasks choose an agent. The built-in Main agent sees all registered resources; other agents can restrict projects, skills, and GitHub connections. YOLO remains the default, with optional workspace-write and read-only execution. See [agent access](docs/AGENT-ACCESS.md) for isolation and setup. MCP `run` grants can trigger task-authorized external actions.
- Providers other than Codex and Claude Code, multi-owner tenancy, stateful MCP sessions, and standalone HTTP+SSE transports are outside this version.

[Detailed delivery plan](docs/PLAN.md) · [Validation record](docs/QA.md) ·
[Performance measurements](docs/PERFORMANCE.md) · [Security](SECURITY.md)

CI runs quality checks and isolated browser suites alongside a container build and
non-root smoke test. Image tags are published only after all checks pass. Release
tags reuse the exact image already validated on main, then verify the deployed
commit. See the [CI measurements and release paths](docs/CI-PERFORMANCE.md).

## Chats

Start a conversation from **Chats**, an agent, or a project. Project chats use Main agent by default. Steer a live response, queue and edit follow-ups, and resume conversations after a restart. See [chat behavior and architecture](docs/chats.md).

## Public artifact links

Files stay private unless explicitly shared. In the web or Android file viewer,
open sharing to enable a public link, copy it, or disable it. Anyone with that
link can read the selected file version without signing in. Other files, versions,
and the conversation stay private. Disabling and re-enabling creates a new link;
downloaded copies cannot be revoked.

Agents can publish with `visibility: "public"` or call `set_artifact_visibility`
with an artifact ID and `visibility: "public"` / `"private"`. These tools are
scoped to the current run and should only make files public on explicit request.
Use the returned `publicUrl` for anonymous access; the normal artifact URL remains
authenticated. Sharing state persists across server restarts.

## Native Android client

The Kotlin / Jetpack Compose client lives in [`android/`](android/README.md).
It uses native Material 3 controls with the web theme’s current light/dark colors
and saved appearance preference. Open `android/` in Android Studio.
It includes chats, streaming activity, attachments and artifacts, MCP management,
Codex and Claude Code accounts and optional periodic notifications without Firebase.
[Android coverage](android/README.md#coverage) and [validation](android/VALIDATION.md)
document the implemented workflows and device/deployment gates. Android builds
run in their own CI workflow. Native MCP OAuth uses the accompanying server bridge.

### Add a GitHub project

In Projects → Add project, choose **GitHub** to browse repositories accessible to
the shared connection configured in Connections. Repositories are listed by
recent activity with their owner, visibility, language, stars, default branch
and last push. Search highlights matches and fetches further pages until results
appear; Private/Public filters, infinite scrolling and arrow keys plus Enter
help choose quickly. Selecting a repository fills its name, description and
default branch, and collapses the list into a summary that can be changed. Saving clones that branch into a managed directory inside the first
configured workspace root. Existing registered repositories are marked as added;
failed imports remove their temporary checkout. The server-directory option is
still available. Web and Android 0.31.0 use the same API (server 0.31.0).

Repository browsing uses GitHub's [authenticated repository listing](https://docs.github.com/en/rest/repos/repos#list-repositories-for-the-authenticated-user).

### Execution time limits

Agents run without a time limit by default. In the agent editor on web or Android,
turn off **No time limit** / **Sans limite de temps** to set a budget of 1–720 minutes.
The API represents unlimited execution as `timeoutMinutes: 0`. Waiting for tools or
account capacity counts toward an optional finite budget; a worker restart preserves
the remaining budget and an explicit resume starts a fresh budget.

On upgrade, the main agent's former default of 120 minutes becomes unlimited once.
Other configured limits are preserved, as are settings captured by existing runs.
Manual cancellation and tool-access revocation still apply to unlimited runs.

Codex transport messages have no fixed byte-size limit. Large tool results and
conversation history no longer fail at 32 MB per message; reading and decoding
each message still require memory proportional to its size. MCP server responses
retain their separate 2 MB limit.

## Agent portraits

New persistent agents can receive an illustrated portrait automatically, regardless
of whether they use Codex or Claude Code. Connect and enable a Codex account in
**Connections** to use [Codex's built-in image generation](https://learn.chatgpt.com/docs/image-generation?surface=cli)
through your ChatGPT subscription. Image generation consumes that account's included usage and depends
on its plan and image quota. No OpenAI API key or separate API billing is used;
Claude subscriptions do not provide this image generation path.
Only the agent's name and description are sent; instructions, conversations and
connected resources are not included. Generation uses an isolated, ephemeral
Codex session with the built-in image tool and no project, shell or MCP tools.
Authentication uses the existing account broker; refresh credentials stay in Leo.
The orchestration model is explicitly `gpt-6-astra`, both when selecting a compatible
account with available quota and when starting the Codex thread and turn.

The generated image is normalized to a 256 × 256 PNG and stored in the application's
database. At most two requests run at once, within the connected accounts' shared
quota and concurrency limits. Creation never waits for generation; initials remain
visible until the image is ready. Existing agents are not regenerated on upgrade.
Editing an agent's name, role or instructions preserves its portrait.

Portraits and conversation titles share an isolated Codex execution module.
Queued Codex conversations and deployment maintenance take priority: an auxiliary
request does not start, or is interrupted and releases its account. An interrupted
portrait retains the previous image and requires an explicit retry; already consumed
quota is not restored. Server shutdown waits for portrait processes, account leases
and pending portrait metadata to finish cleaning up.

In the web or Android agent editor, **Generate / Regenerate** requests a new
portrait and **Upload** replaces it with a PNG, JPEG or WebP (up to 5 MB and
4096 × 4096 pixels, cropped to a square). Portrait changes save immediately.
Uploads also work without a connected Codex account. Uploading while generation is running wins
over its eventual result. Failed or interrupted requests retain the previous
portrait, if any, and can be retried explicitly; server restarts do not automatically
consume quota again. Portraits are served only to signed-in users and are deleted
with the agent. Temporary sub-agents do not get generated portraits.

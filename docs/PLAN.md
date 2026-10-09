# Cairn — delivery plan

## Product promise

A private, self-hosted control room for coding agents. Create an agent profile,
connect a project and CLI accounts, attach reusable skills, then run work now or on
a schedule. Inspect what happened, stop a run, retry it deliberately, and expose
the same capabilities to authenticated MCP clients. The public repository contains
the product; credentials, tasks, prompts, and execution history remain private.

The UI uses Vue 3 and TypeScript throughout. pnpm is the package manager and
`@antfu/eslint-config` owns linting/formatting, including Vue and TypeScript.
Skills live in `.agents/skills`:
global skills under the worker user's home and project skills under each project.
Codex subscription authentication stays with the official CLI. GitHub CI remains
responsible for project tests and releases requested by an agent.

## Architecture and boundaries

- One Node 24+ service: Fastify API, scheduler, bounded process worker, static Vue
  build, and stateless MCP HTTP endpoint. SQLite WAL is the durable store.
- Single active application process per data volume. Concurrent requests are
  supported; horizontally scaling workers is not advertised until a distributed
  queue exists. Database constraints enforce duplicate enqueue protection.
- A task is a reusable instruction; a run is an immutable snapshot of that task,
  agent settings, and selected skills. Editing a task cannot rewrite running work.
- A project is an explicitly registered local directory. Run worktrees isolate
  changes; project paths and skill paths are canonicalized and bounded.
- A named agent defines model override, reasoning, instructions,
  and maximum runtime. Codex is the first execution engine. Multiple agent profiles
  are supported; arbitrary shell templates and unimplemented providers are absent.
- OAuth scopes separate read, run, and manage capabilities. Browser sessions and
  MCP access tokens are separate credentials. Tokens are hashed at rest.
- MCP uses SDK v2 and protocol 2026-07-28 per-request HTTP handlers, with stateless
  compatibility for older Streamable HTTP clients and no retained sessions or SSE
  reconnection state. App run polling is independent of
  MCP transport state.

## Feature inventory and acceptance criteria

### 1. First-run setup and account security

- Bootstrap with a server-provided setup token; create an administrator password.
- Sign in/out, persistent HttpOnly/SameSite cookies, expiring sessions, CSRF/origin
  checks, login rate limits, secure cookies when served over HTTPS.
- No passwords, bearer tokens, device auth caches, or arbitrary environment values
  in API responses/logs. Structured audit entries record state-changing actions.
- Public health endpoint reveals readiness only; everything operational requires
  authentication. Persist config and accounts across process/container restarts.
- Acceptance: unauthorized mutations fail; setup cannot be repeated; bad Origin,
  expired session, password errors, and logout are tested through HTTP.

### 2. Dashboard and navigation

- Clear overview: running/queued work, next scheduled task, recent outcomes,
  connection health, and direct new-task action.
- Sidebar: Overview, Tasks, Runs, Agents, Projects, Skills, Connections, Settings.
- Search and status filters, useful empty states, loading and failure states,
  keyboard-accessible dialogs, visible focus, responsive mobile navigation.
- Calm visual system: warm neutral background, deep ink navigation, restrained
  green accents, readable typography, compact but comfortable spacing.
- Acceptance: a new user can create and run a task without reading documentation;
  browser screenshots at desktop/mobile confirm no clipped controls or overflow.

### 3. Projects and workspaces

- Register existing directories under configured workspace roots, with a friendly
  name and Git origin/base branch details. Check accessibility before saving.
- Option to run in an isolated Git worktree or explicitly in the project directory.
- Link the project from tasks and run details; preserve worktrees after failures
  and successful runs for review; explicit cleanup refuses dirty workspaces.
- Acceptance: missing/out-of-root/symlink-escape paths fail; a run in a worktree
  cannot alter the original checkout through the selected working directory.

### 4. Agent profiles

- Create/edit/delete profiles with name, description, model (CLI default allowed),
  reasoning effort, additional instructions, YOLO execution inside Docker, and runtime ceiling.
- All agents use YOLO mode with full container access and no approval prompts.
  Docker is the isolation boundary; the UI makes this execution model explicit.
- Tasks reference profiles; deleting a referenced profile fails with a useful error.
- Acceptance: exact argument-vector construction, no shell interpolation, and
  immutable profile snapshots are tested.

### 5. Tasks and scheduling

- Create/edit/duplicate/archive tasks with name, prompt, project, profile, skills,
  tags, schedule, timezone, enabled state, and worktree preference.
- One-off Run now; recurring daily/weekly presets plus advanced cron; show next
  occurrences in the chosen timezone before save, including DST behavior.
- Pause/resume schedules without deleting instructions. Missed execution policy
  catches up once rather than replaying a storm of old jobs after downtime.
- One active run per task, project execution lock, bounded global concurrency,
  and duplicate-key protection for scheduled occurrences.
- Acceptance: scheduling tests use a controlled clock; concurrent enqueue attempts,
  restart recovery, DST, invalid cron, pause/resume, and overlap are covered.

### 6. Execution and run history

- Durable states: queued, running, succeeded, failed, cancelled, interrupted.
- Worker launches beacon `codex exec --json` using stdin instructions, captures
  progress and final response, records session ID and usage when available.
- Paginated run history and incremental log polling; readable event timeline,
  final answer, duration, trigger, workspace, and task/settings snapshot.
- Cancel queued/running work, terminate process groups with bounded escalation,
  enforce timeouts and resume checkpointed conversations after worker restart.
  Preserve cancelled state and require explicit resumption of failed or cancelled work.
  Resume instructions require checking external effects before repeating actions.
- Bounded output/event size and retention prevent a verbose child from exhausting
  memory or storage. Store summaries separately from event pages.
- Acceptance: real fixture subprocesses exercise streaming, malformed JSON,
  nonzero exit, cancellation, timeout, redaction, and restart behavior.

### 7. Codex and GitHub connections

- Status cards show installed CLI/version and authenticated account status without
  exposing credentials. Refresh explicitly and cache expensive subprocess checks.
- Guided device login through official CLIs: show only intended verification URL
  and code, track pending/completed/failed state, and support cancellation.
- Existing local CLI auth works on the development host. VPS/container auth uses
  a separate persistent home directory. Never bake credentials into images.
- Acceptance: actual local account status is verified; device-flow parser/process
  lifecycle tests use recorded nonsecret fixtures; no fake connected indicators.

### 8. Global and project skills

- List/search/view/create/edit/delete skills using `.agents/skills/<name>/SKILL.md`.
- Validate YAML frontmatter/name and offer a starter template and Markdown preview.
- Manage supporting text files beneath each skill with traversal/symlink protection;
  show scope and paths. Skills can be selected on tasks and are snapshotted for runs.
- Show disabled/invalid skills with actionable errors instead of silently hiding them.
- Acceptance: edit persists to actual filesystem, a runner receives selected content,
  invalid input/path traversal fail, and project/global scopes remain distinct.

### 9. Stateless MCP and OAuth

- `/mcp` exposes discoverable tools for listing agents/projects/skills/tasks/runs,
  reading run output, creating/updating tasks, enqueue/cancel, and skill management.
- Tool schemas, concise descriptions, read-only/destructive annotations, useful
  domain errors, pagination, and the same service layer as the web API.
- OAuth discovery, protected-resource metadata, dynamic client registration,
  authorization code + S256 PKCE, explicit browser consent, audience-bound opaque
  access tokens, expiring one-use codes, refresh rotation/reuse protection, revocation.
- Admin-created scoped personal tokens for direct CLI clients, displayed once;
  connected OAuth grants can be viewed/revoked in Settings.
- Acceptance: latest official MCP client lists/calls tools over real HTTP; independent
  requests need no session; missing/wrong/expired credentials and insufficient scopes
  fail. OAuth replay, bad PKCE, redirect mismatch, audience mismatch are tested.
- Document ChatGPT/Claude custom connector setup and submission assets/checklist.
  Marketplace listing approval belongs to those platforms; compatibility is verified
  separately from any claim that the app is listed.

### 10. Operations and delivery

- Docker multi-stage image with non-root worker, official Codex/GitHub CLIs and
  required build tools; persistent data, CLI home, and workspace volumes.
- Compose defaults bind localhost; HTTPS reverse proxy/Coolify handles public access.
- Healthcheck, graceful shutdown, log retention, SQLite backup/restore instructions,
  environment example with no secrets, image version/commit in diagnostics.
- GitHub CI: lockfile install, types, unit/integration tests, browser e2e, production
  build, Docker build and smoke; GHCR images tagged by immutable commit and release.
- Public GitHub repo `leo91000/leo-agent-manager`; meaningful commits and no tool
  attribution. Wait for actual CI results and image availability after pushing.
- Bonus: deploy to Coolify when server/domain credentials are available; verify HTTPS,
  login, data persistence, task execution, and MCP externally before claiming live.

## Delivery passes

1. **Foundation and implementation:** typed domain contracts, migrations, auth,
   service layer, worker, API, Vue screens, OAuth/MCP, and operations files.
2. **UI/UX critique:** inspect actual rendered desktop/mobile screenshots; evaluate
   hierarchy, creation flow, discoverability, forms/errors, empty states, focus,
   run monitoring and destructive actions; implement improvements and recapture.
3. **Manual exploratory QA:** operate the real UI separately from scripted e2e.
   Complete project/profile/skill/task/run flows, try invalid inputs, reload while
   running, cancel/retry, inspect connection statuses, and exercise OAuth consent.
   Record steps, observations, screenshots, defects, and retests.
4. **Performance:** seed realistic history, measure list/detail/log endpoints and
   production page payloads; inspect indexes/query plans, polling volume, event
   bounds and process concurrency. Improve measured bottlenecks and record before/
   after evidence. State measured workload and limits, not absolute optimality.
5. **Packaging and release:** run the full checks, container smoke and persistence
   test, push public repo, wait for CI/GHCR, then attempt optional Coolify deployment.

## Evidence to retain

`docs/QA.md` will map every feature above to passing tests and manual observations.
`docs/PERFORMANCE.md` will contain reproducible measurements. Screenshots go in
`docs/screenshots/` without credentials. `docs/DEPLOYMENT.md` and `docs/MCP.md` will
cover installation, recovery, authentication, connector setup and external limits.
This plan remains the completion contract; incomplete work is marked explicitly.

## Sources verified on 2026-09-09

- Vue 3 + Vite: https://vuejs.org/guide/quick-start.html
- MCP SDK v2 / 2026-07-28: https://ts.sdk.modelcontextprotocol.io/v2/
- Stateless HTTP: https://ts.sdk.modelcontextprotocol.io/v2/serving/http.html
- Fastify adapter: https://ts.sdk.modelcontextprotocol.io/v2/serving/fastify.html
- OAuth: https://ts.sdk.modelcontextprotocol.io/v2/serving/authorization.html
- Codex CLI and subscription auth: https://learn.chatgpt.com/docs/auth
- Docker named volumes: https://docs.docker.com/engine/storage/volumes/

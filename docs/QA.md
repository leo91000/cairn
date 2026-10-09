# Validation record

Validation performed on 2026-09-09 and 2026-09-10. Screenshots use an isolated fixture workspace,
not production projects or credentials. Browser journeys exercise a real Fastify
service, SQLite database, filesystem, and subprocess; their model runner is a
small explicit fixture. Actual Codex/GitHub checks are recorded separately below.

## Automated coverage

`pnpm check` runs Antfu ESLint, TypeScript checks, 48 unit/integration tests, and the
production Vue build. `pnpm test:e2e` runs three complete Chromium journeys plus
mobile layout/navigation passes in Chromium and WebKit.

| Area | Evidence |
| --- | --- |
| Setup, password login/logout, session expiry, CSRF, Origin/Host checks, concurrent setup | `scripts/tests/auth.test.ts` |
| OAuth client/redirect/PKCE/resource binding, one-use/expired codes, refresh rotation/reuse, scope checks, revocation | `scripts/tests/auth.test.ts` |
| Immutable snapshots, concurrent enqueue, task reference protection, sanitized Git origin metadata | `scripts/tests/service.test.ts` |
| Cron validation, pause, catch-up once, overlap, spring/autumn DST | `scripts/tests/service.test.ts` |
| Actual global/project skill files, frontmatter validation, nested supporting files, traversal and symlink escapes | `scripts/tests/service.test.ts` |
| Actual subprocess output, malformed JSON, usage/session capture, redaction, failures, cancellation, timeout, legacy restart interruption | `scripts/tests/worker.test.ts` |
| Project execution serialization, isolated Git worktree, dirty primary preservation, guarded cleanup | `scripts/tests/worker.test.ts` |
| Device-code parsing and process completion | `scripts/tests/worker.test.ts` |
| Official MCP v2 client over TCP, independent requests, no session ID, tools, scopes, stateless 2025-06-18/2025-11-25 compatibility, revoked access | `scripts/tests/mcp.test.ts` |
| Durable reopen, transaction rollback, expired records, schema downgrade refusal, compact pagination, archival restrictions | `scripts/tests/store.test.ts` |
| Setup → profile → project → skill → scheduled task → real fixture run → result → reload → pause → mobile navigation → logout | Browser journey 1 |
| Supporting-file write/read/preview, running cancellation, archive/restore, retained history | Browser journey 2 |
| OAuth consent through login, redirect callback, code exchange, settings revocation, denied MCP access | Browser journey 3 |
| Nine workspace pages, all run tabs, five editors, workspace search, drawer scrolling/focus at changing viewport heights | Browser journeys 4–5, Chromium and WebKit |
| Non-root container, CLI binaries/pnpm 12, authenticated API, Vue build, persistent session/data across restart | `tests/container-smoke.mjs` |

## UI/UX critique and exploratory checks

The initial desktop layout had overly small/light supporting text. Font sizes and
contrast were increased while keeping the compact layout. Fonts were moved into
the bundle; dialogs now have accessible names and restore focus. Mobile navigation
and a 390-pixel viewport were inspected after transitions settled. Final screenshots:

- [Desktop overview](screenshots/overview-desktop.png)
- [Mobile overview](screenshots/overview-mobile.png)
- [Agent editor with YOLO execution](screenshots/agent-yolo.png)
- [Initial contrast comparison](screenshots/overview-before-contrast.png)

Exploratory operation through the isolated browser, separately from the scripted
journeys, covered creating an owner workspace, checking connection states, guided
device login, invalid skill frontmatter, correction and Markdown preview, keyboard
search/Escape, creating an agent/project/task, selecting a skill, running the task,
and reading the original task/workspace/access snapshot. The exercise found:

1. Empty POST/DELETE requests incorrectly advertised JSON bodies; Run now/cancel/logout
   could fail before reaching the route. Headers now reflect whether a body exists.
2. Supporting-file reads returned plain text to a JSON client. The API now consistently
   returns `{ content }`; browser tests verify persisted content.
3. Changing a timezone after preview could render stale occurrences with an invalid
   zone. Preview results reset on schedule/zone edits.
4. Event type `status` collided with badge CSS. Event types now use a data attribute.
5. Skill names used an invalid HTML pattern under the browser's Unicode `v` grammar.
   The hyphen is escaped; server-side validation remains authoritative.
6. A small bundled font subset was inlined as a data URL but rejected by CSP. Font CSP
   now allows local/data fonts while external font origins remain blocked.
7. Workspace search omitted skills; they are now included and were found through the
   search dialog during the final manual retest.

The final agent editor was inspected after removing the access selector. Saving an
agent preserved its settings and showed YOLO mode. A fresh page load emitted no
console errors.

The browser suite checks unexpected console errors as well as page exceptions.
Intentional invalid-input HTTP 400 responses are excluded from that console check.

### Mobile pass for v0.1.1

The screenshot matrix covers 320×568, 390×664, 430×932, and 844×390 viewports in
Chromium and WebKit. It checks horizontal overflow, search icon alignment,
separation of Activity controls from tabs, and reachable dialog actions. Drawer
checks resize the viewport to 360, 568, and 844 pixels high, scroll to Sign out,
and exercise focus trapping, Escape, focus restoration, and navigation. Screenshots
are saved in the workflow's Playwright artifact; these are browser emulations,
not physical-device captures.

An independent isolated-browser review also covered login and OAuth approval at
320 pixels wide. The pass fixed the unscrollable navigation, dynamic viewport and
safe-area sizing, stacked search icons, cramped Activity tabs, overflowing run
history, small mobile form controls, and editor height constraints. Selecting the
current navigation destination now closes the mobile drawer as well.

Reviewed WebKit captures: [Tasks](screenshots/mobile-tasks.png),
[Activity](screenshots/mobile-activity.png), [Runs](screenshots/mobile-runs.png),
and [scrolled short-screen drawer](screenshots/mobile-drawer-scrolled.png).

## Conversation activity in v0.1.2

Activity renders assistant Markdown between collapsible groups of terminal,
file-change, search, MCP tool, plan, and session cards. Syntax highlighting is
bundled locally for code fences, commands, JSON, and diffs; unrecognized languages
remain escaped plain text. Raw event details and copy controls remain available.
Tool lifecycle updates replace the existing card in its original position.

The worker now saves redacted structured event payloads alongside the existing
text. The additive version 2 SQLite upgrade preserves old event history and is
covered by an upgrade/reopen test. Old records use the text and lifecycle events
available at the time; metadata discarded by earlier versions cannot be restored.

The Chromium/WebKit mobile matrix opens grouped file and command cards, verifies
highlighted diffs, enters fullscreen at all four viewport sizes, then checks
Escape, focus restoration, and preserved expanded content. The fullscreen dialog
uses dynamic viewport height, safe-area padding, and its own scroll container.

Reviewed screenshots: [mobile conversation](screenshots/activity-conversation-mobile.png),
[fullscreen on mobile](screenshots/activity-fullscreen-mobile.png),
[file changes on mobile](screenshots/activity-files-mobile.png), and
[desktop details](screenshots/activity-details-desktop.png).

## Shared virtual selects

All eight native selects now use the [shared combobox](SELECT.md), with searchable
labels/descriptions, option icons, groups, clearable filters, keyboard navigation,
and required/disabled states. The menu uses the native top layer and follows the
visual viewport so it remains usable inside scrolling dialogs.

Unit checks cover multiword/accent-insensitive search and virtual row geometry.
Both browser engines select the last of 10,000 options while fewer than 20 options
are mounted, search for “Équipe sécurité” using “equipe”, preserve the selection
on Escape, show empty results, and restore tab navigation. The viewport matrix
opens task, agent, and skill menus at 320×568, 390×664, 430×932, and 844×390.
An existing skill editor is included to catch cramped file-picker controls.

The manual isolated-browser review also verified clearing a run status filter,
disabled scope on existing skills, and native required-field validation focusing
the visible combobox with an empty-state message. It found and fixed the skill
file input collapsing to zero width at 320 pixels: the file picker and editor
tabs now occupy separate rows on mobile.

Reviewed captures: [desktop agent menu](screenshots/select-agent-desktop.png),
[mobile agent menu](screenshots/select-agent-mobile.png),
[grouped schedule menu](screenshots/select-schedule-mobile.png), and
[320-pixel skill file picker](screenshots/select-skill-mobile.png).

## Structured activity results in v0.1.3

Assistant text and tool output now recognize complete JSON objects/arrays, including
prefixed output and JSON code fences. Workflow results show explicit statuses,
check counts and compact job rows; pull request results show fields and file lists.
Other structures use labeled fields and expandable sections. Long arrays load in
batches, nested sections mount on expansion, and the highlighted JSON source mounts
only when opened. Copy JSON preserves the original JSON text.

The historical screenshot cases are reproduced in the activity fixture. Parsing
tests cover mixed prose, quoted braces, code fences, Markdown links, multiple
results, and truncated/malformed data. Incomplete JSON and other code remain intact.
This is a presentation change: existing persisted activity is rendered through the
same component and does not need to be rerun or migrated.

The Chromium/WebKit viewport matrix opens checks and file lists in fullscreen,
expands long lists, checks syntax highlighting and copy controls, and verifies
horizontal containment. Manual desktop/mobile screenshot review confirmed the
layout. Completed execution is neutral; only explicit successful conclusions use
the success badge.

Reviewed captures: [desktop workflow results](screenshots/activity-json-desktop.png),
[mobile workflow results](screenshots/activity-json-mobile.png), and
[mobile pull request files](screenshots/activity-json-files-mobile.png).

## Operation cards in v0.1.4

Activity uses distinct cards for terminal commands, file reads, edits, searches,
workspace browsing, tools, plans, and session notices. Cards show retained command
and file metadata, explicit exit codes, lifecycle duration when both events exist,
output line counts, and additions/removals when a diff is available. File reads
highlight the file language; Markdown reads offer a rendered preview, collapsible
frontmatter, and the original source. Raw output and copy controls remain available.

The reported “Working” cards reproduced with an old item-start/item-complete pair:
completion updated the output but left the generic start title. Historical pairs
now become neutral “Recorded output” cards with a preview. Commands, file identities,
exit codes and diffs discarded by old workers cannot be reconstructed from output;
the UI says when those details were not recorded. A regression also preserves the
pair when the completed output happens to be a JSON object.

Read/search/browse classification is conservative and based on saved commands.
A small parser recognizes simple shell commands and quoted shell wrappers without
executing them; compound commands, redirections and substitutions keep terminal
presentation. Tests cover reads, search, directory browsing, commands, mutation
ambiguity, lifecycle identity and the historical event cases. Both browser engines
exercise source highlighting, Markdown preview/source switching, failed command
status and historical output across mobile viewports.

Reviewed screenshots: [desktop operations](screenshots/artifacts-desktop.png),
[code read](screenshots/artifacts-read-mobile.png),
[Markdown preview](screenshots/artifacts-markdown-mobile.png),
[file edits](screenshots/artifacts-edit-mobile.png),
[terminal output](screenshots/artifacts-command-mobile.png), and
[historical output](screenshots/artifacts-legacy-mobile.png).

## Appearance in v0.1.5

Light, Dark, and System modes now cover the full workspace, authentication,
editors, selects, and activity artifacts. Both Chromium and WebKit exercise
dark desktop/mobile matrices alongside existing light-mode coverage. Preference
tests cover OS changes, persistence, cross-tab updates, storage restrictions,
and the pre-application background. [Theme design and reviewed screenshots](THEMES.md)
record the palette, behavior, and detailed coverage.

## Actual provider and container execution

The application's connection checker detected local `codex-cli 0.153.4` logged in
with a ChatGPT subscription and `gh 2.100.0` logged in as the repository owner.
A minimal task through the actual worker returned `CAIRN_MANAGER_SMOKE_OK` with a
successful CLI exit and usage data, without modifying a project.

A live container test ran Codex with `--dangerously-bypass-approvals-and-sandbox`.
Its shell tool ran `pwd`, created/read/removed `/tmp/cairn-yolo-probe`, and exited 0;
the final response was `CAIRN_CONTAINER_YOLO_OK /app`. The existing subscription auth
file was mounted read-only for this ephemeral test. No credentials were copied
into the image. Docker used its normal isolation without privileged mode.

## Performance and delivery boundaries

The [performance record](PERFORMANCE.md) includes the workload, before/after numbers,
and remaining limits. GitHub's `Quality and container` workflow reruns checks,
browser journeys, image build, and container smoke before publishing GHCR images.
The latest remote result is available in the repository's Actions tab.

Live ChatGPT/Claude linking, marketplace listing, and Coolify deployment require the
final reachable deployment/account configuration and are not established by local
SDK/browser tests. Platform submission requirements are documented in [MCP.md](MCP.md).

Restart recovery is covered by `scripts/tests/restart.test.ts` (real SIGTERM/SIGKILL process loss, session retention/discovery, cancellation, timeout, policy changes, account selection, runner fencing, and the real isolated-run client with a controlled broker outage) and `scripts/tests/runner-lifecycle.test.ts` (in-flight/delayed container starts). `tests/e2e/task-focus.spec.ts` checks automatic browser reconnection and manual conversation resume, with mobile dark and desktop light screenshots.

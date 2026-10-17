# Android 0.6.0 / web parity

Reference: web commit `2e9868233295af713eede804c7d70b9f514df04b` (v0.21.12).
This audit compares user actions and information, with native Android presentation.
It does not assert pixel equality between a browser and Material components.

## Feature map

| Area | Native Android implementation | Validation |
| --- | --- | --- |
| Navigation | Since 0.32.0: Fil / Missions / Atelier floating dock with an always-available new-conversation button (rail on large windows), universal search with direct actions; no workspace-owner decorations | Workspace and Signal journeys; device review |
| Conversations | Fil home grouped by what needs the user, live work, conversations waiting to resume and recent conversations; one card per scheduled wait in the conversation; conversations and missions opened from it read in one pager that swipes in the Fil's order, a mission run adding a Mission tab; compact chooser from the conversation title with search and date groups; agent/project chosen on screen for a new conversation | Chat and Signal journeys; device switching/search |
| Composer | Native expanding text field, attachments, model/reasoning controls, preserved drafts when switching chats, send/queue/steer/stop, IME insets | Chat journey; device keyboard and enlarged text |
| OpenCode model access | Web and Android keep the current conversation’s model and reasoning after opening/reloading; catalog explains provider access checks and labels provider defaults. A regional Privacy refusal offers OpenCode settings, retry in the same session, or a new conversation while preserving the failed one. Android uses French native controls and Custom Tabs. | OpenCode browser journeys; shared ChatProvider cases on JVM/device; pinned-native session resume and startup deadline/cancellation tests |
| Delivery | Initial send and initiating run message appear in transcript; only waiting follow-ups appear in queue, held behind a scheduled wait; acknowledgements deduplicate optimistic messages; question responses use server privacy redaction | Six presentation regressions; existing message/network cases |
| Queue and questions | Queue strip next to the composer (first follow-up visible, expandable), pause/resume, edit/remove/steer, option/free-text/private answers | Chat journey; network/Compose suite |
| Subagents | Subagents started together form a team card in the conversation and the run log (state, current step or report, elapsed time); each opens its full activity (prompt, steps, report, nested teams) in a sheet with one tab per teammate; a bar above the composer keeps working subagents in sight and replaces the working indicator when the agent only waits for them | `SubagentsTest` mirrors the web subagent cases; `SubagentViewsTest` |
| Completion | Quiet task outcome below final reply, expandable reason/evidence, explicit blocked/input state, attribution; routine session rows hidden in chat; failures remain visible | Presentation/activity cases; device evidence expansion |
| Task workspace | Missions list merging tasks and activity: readable schedules, recent run history, direct run, filters and search; mission sheet (detail pane on wide windows) with next dates, success rate and history opening each run; the full run log stays under Atelier › Journal | Native UI, workspace and Signal journeys; device mission screens |
| Task management | Create/edit/duplicate/delete/archive/restore, schedules/timezones/tags, agent/project/model/limits, pause/resume/run, original brief | Workspace journey; device editor; existing task form |
| Runs | Conversation/result/files, history, fullscreen, stop/retry/resume, errors/account waiting, usage, snapshots, project source revision, workspace cleanup guard | Workspace journey; history/scroll regressions |
| Agents | Existing configuration and access controls, project-scoped GitHub behavior, dedicated token on creation, permitted skills and MCP tools | Form/API comparison; existing account/MCP suite |
| Projects | Remote branch or committed local snapshot, path and base branch, starting revision in run details | Contract decoding; form/API comparison |
| Skills and MCPs | Skill content/files, scopes and validation, MCP configuration/authentication/discovery/tool restrictions | Existing API and MCP/Compose cases; form/API comparison |
| Connections and access | OAuth, GitHub, Codex and Claude Code accounts/usage, token grants, authorization, logout and encrypted sessions | Existing network/authorization/cache suite |
| Plugins | Catalogue, instances, manifest-driven settings and secrets, agent grants, version/file review and approval, declarative panels/actions/tests, native Discord setup and delegation/access/activity, 1Password instances | `PluginsJourneyTest`, shared `PluginsCases` / `PluginsDeviceTest`; browser plugin journeys |
| Plugin setup and resources | Owner can save/resume/discard an inactive 24-hour setup, edit root fields and masked secrets, prepare related resources, then activate the same instance with no default agent grants. Active resources support creation, editing, parent selection, pagination and confirmed deletion. | Browser setup/resource journey against the installation; shared setup/resource Compose journey. Owner resource OAuth is available below; resource grants are covered below; routes remain a separate unfinished delivery slice. |
| Personal plugin accounts | Members and owners open their own accounts from Atelier, select explicit provider scopes, open consent, check pending/completed authorization and confirm disconnection. Paused resources retain access to disconnect an existing authorization. | Public HTTP/native OAuth tests; browser consent-popup journey; shared Compose member navigation and personal consent journey. The Android journey answers provider completion at the HTTP boundary; it does not exercise a real provider or Custom Tab handoff. Owner resource consent uses the separate owner management interface below. |
| Owner resource authorization | Owners connect shared providers on prepared or active resources, select explicit scopes, use the configured callback or a direct HTTPS override, see the effective return URL, check consent and confirm disconnection. Providers on the same resource serialize consent and disconnection; different resources remain independent. Every pending resource consent gates root saving/activation; OAuth changes refresh resource revisions. Personal providers stay in the personal directory and are unavailable before activation. | Browser and shared Compose journeys cover simultaneous consents across resource pages, manual/automatic refresh, pending checks, direct/default callbacks, revision-bound draft requests and active disconnection. Provider completion is synthetic at the public HTTP boundary; native OAuth tests cover actual exchange/provenance/promotion. No real provider or Android Custom Tab handoff is claimed. |
| Resource agent access | Owners explicitly choose an agent, optional cumulative profile, operation exceptions and scope limits on an active resource. A per-rule preview explains the additive grant semantics; saved rules can be revoked with confirmation. No grants are exposed for inert setups. | Browser journeys exercise real native grant CRUD and uncertain-response recovery; the shared Compose journey checks the same profile/exception/scope payload and revocation. Server-side tool/command enforcement remains covered by native resource grant tests. Android uses its full-screen editor; web uses a modal. |
| Artifacts | Authenticated previews, versions, groups, images/PDF/Markdown, save/share and deep links from replies | Chat journey; artifact and link gesture regressions |
| Reading | Wrapped long text/URLs, selectable native Markdown, history pagination, stable reading anchors during drag/streaming, jump to latest | Markdown/history suite; real device gesture tests |
| Live catch-up | Following the latest output, a resume past a backlog longer than one page reopens on the recent window; older messages load on scroll. Reading older messages keeps the complete replay and the reading position. Stored backlog pages bypass the live coalescing delay | Backend recent-window, cached-resume, reconnect and history/reset tests; web connection test; Android `LiveTest`, `ScrollResumeTest` and `RecentWindowResumeTest` |
| Appearance | Android-specific « Signal » palette since 0.32.0 (paper/ink with the Cairn blue accent, coral for attention), agent/project identity colours, light/dark/system themes, font scaling, touch-sized labelled actions | Native captures; large-text/keyboard device checks |

Validation results are recorded in `../VALIDATION.md`; entries above identify the
coverage used, not a claim that every production account/provider was exercised.

## Platform equivalents

- Navigation uses Android system Back, bottom sheets and a navigation rail on wide
  windows. Chat and run readers hide the global phone navigation to maximize room.
- A subagent's activity opens in a bottom sheet on every window size, where the wide web
  layout shows a side panel. A team row opens that sheet directly instead of expanding inline.
- Android document/photo pickers and system share/save/selection replace browser
  upload/download/clipboard controls. External OAuth uses Custom Tabs.
- Installing a plugin package uses « Continuer sur le web ». Reviewing files and
  versions, approving packages, editing instances and configuring Discord are native.
- Plugin setup uses a full-screen native editor and resource dialogs on Android;
  the web uses a setup modal and inline resource forms. Both keep typed secrets
  only in memory and require saved root settings before resource changes or activation.
- Personal plugin consent opens an explicit new browser page on the web and a
  Custom Tab on Android. Both require returning to check completion, keep consent
  state only in screen memory and never receive provider credentials.
- Foreground updates use SSE. Background question notifications retain the
  existing Firebase-free WorkManager service: Android schedules periodic checks
  (approximately 15 minutes, potentially delayed by the OS), rather than browser
  Web Push. This is an explicit delivery-timing difference.
- Drafts survive in-app conversation switching; sensitive attachments remain in
  app storage and are cleared with session logout/forget. No draft is uploaded
  before the user sends it.

## Test data and privacy

Device UI captures use a local MockWebServer fixture, synthetic messages and
example.test links. They do not contain production conversations or credentials.

# Android 0.6.0 / web parity

Reference: web commit `2e9868233295af713eede804c7d70b9f514df04b` (v0.21.12).
This audit compares user actions and information, with native Android presentation.
It does not assert pixel equality between a browser and Material components.

## Feature map

| Area | Native Android implementation | Validation |
| --- | --- | --- |
| Navigation | Since 0.32.0: Fil / Missions / Atelier floating dock with an always-available new-conversation button (rail on large windows), universal search with direct actions; no workspace-owner decorations | Workspace and Signal journeys; device review |
| Restored session restart | Both clients consume installation-provided `canRequestNewSession`, including current run restorations and historical chat restorations. The offer remains available on later failures after a successful turn, preserving the historical lifetime; accepting pauses the queue and cancels its wakeup. Web keeps its confirmation panel; Android keeps its native button/dialog. | Lifecycle/continuation regressions; fresh-session browser journey; `ConversationLifecycleCases` on JVM/device |
| Conversation retention | Owner setting shared with web: 7 days by default, custom whole days or 0 to disable, confirmation explaining permanent deletion of existing conversations. Android uses French native number input and dialog; web uses Settings → Installation. | Backend lifecycle tests; settings browser journey; `ConversationRetentionTest` / `ConversationRetentionDeviceTest` |
| Conversations | Fil home grouped by what needs the user, live work, conversations waiting to resume and recent conversations; one card per scheduled wait in the conversation, placed at the end of the turn that scheduled it, its reason clamped to three lines until expanded; conversations and missions opened from it read in one pager that swipes in the Fil's order, a mission run adding a Mission tab; compact chooser from the conversation title with search and date groups; agent/project chosen on screen for a new conversation | Chat and Signal journeys; device switching/search |
| Composer | Native expanding text field, attachments, model/reasoning controls, preserved drafts when switching chats, send/queue/steer/stop, IME insets | Chat journey; device keyboard and enlarged text |
| Attachments | ＋ opens the attach sheet (recent photos, camera, photo picker, files). The draft shows a horizontal tray: image thumbnails and file cards with a coloured type badge and the name shortened in the middle, upload progress per file, upload failures retried on the next send, and refused files (over 10 MB, or 40 MB per message) that block sending until removed. Sent and queued messages show their files above the bubble: an image mosaic (+N beyond four), document cards picturing a PDF's first page or a text file's opening lines, and one-line cards with page or line counts. Tapping opens a full-screen viewer that pages through the message's files with a filmstrip; a long press saves, shares, copies an image or attaches the file again. The file families, their colours and the server `?preview` / `?summary` endpoints are shared with the web (`apps/web/src/attachments.ts`) | `AttachmentPresentationTest` mirrors the web family table; `AttachmentViewsTest`; `ImagePasteCases` |
| Claude Code default model | Both clients name the default model in its choice when known, using the resolved identifier if the CLI cache and description are absent. The saved choice stays empty (follow Claude Code), including after selecting another model and returning to default. Web uses an English menu and Android a French native sheet. | Claude catalog metadata regression; Claude browser journeys; shared `ModelPickerCases` on JVM/device |
| OpenCode model access | Web and Android keep the current conversation’s model and reasoning after opening/reloading; catalog explains provider access checks and labels provider defaults. A regional Privacy refusal offers OpenCode settings, retry in the same session, or a new conversation while preserving the failed one. Android uses French native controls and Custom Tabs. | OpenCode browser journeys; shared ChatProvider cases on JVM/device; pinned-native session resume and startup deadline/cancellation tests |
| Delivery | Initial send and initiating run message appear in transcript; only waiting follow-ups appear in queue, held behind a scheduled wait; acknowledgements deduplicate optimistic messages; submission retries retain their UUID only for the same chat, queued-message edit and content, and starting a new conversation clears the attempt and its error (Android already scopes its key by chat/edit; its errors are an app-wide dismissible notice cleared by the next action); question responses use server privacy redaction | Six presentation regressions; existing message/network cases |
| Queue and questions | Withdraw an obsolete question without answering (web: “No longer needed”; Android: “Ce n’est plus utile”); provider closures, replaced sessions and completed task outcomes remove pending prompts on both clients. Agent tools list and withdraw questions in the current conversation. Queue strip next to the composer (first follow-up visible, expandable), pause/resume, edit/remove/steer, option/free-text/private answers | Chat journey; `QuestionWithdrawalTest` / `QuestionWithdrawalDeviceTest`; network/Compose suite |
| Subagents | Subagents started together form a team card in the conversation and the run log (state, current step or report, elapsed time); each opens its full activity (prompt, steps, report, nested teams) in a sheet with one tab per teammate; a bar above the composer keeps working subagents in sight and replaces the working indicator when the agent only waits for them | `SubagentsTest` mirrors the web subagent cases; `SubagentViewsTest` |
| Agent plan | The plan Codex, Claude Code and OpenCode keep while they work (one `todo_list` item whatever its source) shows as a card where the agent first wrote it, kept current and folded once finished; each later change leaves a line naming the steps finished, started, added or dropped; while the agent follows the current request's plan, a bar above the composer shows its progress and the step in progress, and opens the whole plan with its explanation | `AgentPlanTest` mirrors `agent-plan.test.ts`; `PlanViewsTest`; browser plan journeys in `chats.spec.ts` |
| Completion | Quiet task outcome below final reply, expandable reason/evidence, explicit blocked/input state, attribution; routine session rows hidden in chat; failures remain visible | Presentation/activity cases; device evidence expansion |
| Task workspace | Missions list merging tasks and activity: readable schedules, recent run history, direct run, filters and search; mission sheet (detail pane on wide windows) with next dates, success rate and history opening each run; the full run log stays under Atelier › Journal | Native UI, workspace and Signal journeys; device mission screens |
| Creation retries | Agents, local projects and missions retain the acquired ID before any refresh or GitHub-token step. Creation forms keep one UUID and the original POST payload across an uncertain response; retries recover the created record before applying edited fields with PUT. Save, uncertain-creation, latest-changes, token and reload failures have distinct feedback (English on web, French on Android); an edit rejected on a resource opened for editing reads as a plain save failure. A failed save brings its message into view: web scrolls the alert above the sticky actions, Android scrolls the editor back to its top message. GitHub import retains its existing repository identity and stores the returned project before refresh. | Shared `ResourceSaveCases` on JVM/device; browser creation retry journeys; backend creation replay tests |
| Task management | Create/edit/duplicate/delete/archive/restore, schedules/timezones/tags, agent/project/model/limits, pause/resume/run, original brief | Workspace journey; device editor; existing task form |
| Runs | Conversation/result/files, history, fullscreen, stop/retry/resume, errors/account waiting, usage, snapshots, project source revision, workspace cleanup guard | Workspace journey; history/scroll regressions |
| Agents | Existing configuration and access controls, project-scoped GitHub behavior, dedicated token on creation, permitted skills and MCP tools. Web installation/node access checkboxes use horizontal, centered label rows; Android already presents these controls in centered native `Toggle` rows (`ResourcesScreen.kt`, `Components.kt`), with French labels and switches. | Form/API comparison; existing account/MCP suite; Claude browser review of web access rows across desktop, phone and enlarged text |
| Projects | Remote branch or committed local snapshot, path and base branch, starting revision in run details | Contract decoding; form/API comparison |
| Skills and MCPs | Skill content/files, scopes and validation, MCP configuration/authentication/discovery/tool restrictions | Existing API and MCP/Compose cases; form/API comparison |
| Account/installation scope cleanup | Android centralizes streams, read markers, drafts, notification scope and session cleanup in a private ViewModel transition. Same account/installation/role/CSRF preserves cached history and files; changing any clears them. Beacon outages retain a restorable session. When the last access is removed, the « Aucune installation » page now hosts the snackbar, so the access-change and account notices appear there (web reports an inaccessible installation inline); web behavior is unchanged. | `ScopeTransitionTest`; shared `CairnAccountCases` HTTP 403 journey in JVM/device wrappers; `OlderPagesScopeTest` API/page cancellation; web `api-connection.test.ts` |
| Connections and access | OAuth, GitHub, Codex and Claude Code accounts/usage, token grants, authorization, logout and encrypted sessions | Existing network/authorization/cache suite |
| Plugins | Instance and panel forms share defaults on opening, typed numbers, omitted blank text/numbers, explicit false/empty choices, exact choice identifiers (including admitted whitespace), hidden declared values and no secret prefilling. Text trimming and secret blankness on both clients follow existing Kotlin `Char.isWhitespace` (Java WhiteSpace/SpaceChar union): U+001C–U+001F are blank; U+0085 and U+FEFF remain meaningful. Instance secrets are trimmed; nonblank action secrets retain raw bytes. Shared boundary corpus checks the actual Kotlin/JDK runtime and both production helpers; server instance control rejection remains independent. Required selects use null/empty missingness, including whitespace-only option IDs, while required text still uses blankness. Unchanged returned forms retain drafts; changed form definitions reset their controls on both clients. Web centralizes these rules in `plugin-form-values.ts`; Android retains native controls and the existing `formValue`/`pluginSettings` contract, with a shared JSON scenario corpus (`PluginFormValuesTest`). Shared server admission rejects inconsistent manifest constraints/defaults (text cap 10,000; secret cap 16,384); package upload stays on web and Android reviews the same validated manifest. A package with no usable version shows its stored version errors in review, and its page offers that review on both clients. Catalogue, instances, manifest-driven settings and secrets, agent grants, version/file review and approval, declarative panels/actions/tests, native Discord setup and delegation/access/activity, 1Password instances; JSON/TOML manifest review and package-owned program check/install/restore (file uploads remain on web) | `PluginsJourneyTest`, shared `PluginsCases` / `PluginsDeviceTest`; browser plugin journeys |
| Artifacts | Authenticated previews, versions, groups, images/PDF/Markdown, save/share and deep links from replies | Chat journey; artifact and link gesture regressions |
| Reading | Wrapped long text/URLs, selectable native Markdown, history pagination, stable reading anchors during drag/streaming, jump to latest | Markdown/history suite; real device gesture tests |
| Live catch-up | Following the latest output, a resume past a backlog longer than one page reopens on the recent window; older messages load on scroll. Reading older messages keeps the complete replay and the reading position. Stored backlog pages bypass the live coalescing delay | Backend recent-window, cached-resume, reconnect and history/reset tests; web connection test; Android `LiveTest`, `ScrollResumeTest` and `RecentWindowResumeTest` |
| Connection loss | A lost connection is a quiet notice that leaves on its own. Android probes the selected installation, reloads after any current action finishes and cancels old recovery when switching installations. The web shares recovery probes separately for Beacon account calls and installation calls, and retries safe HTTP reads; failed writes get a short toast and are never replayed. Local file failures and genuine application errors keep an actionable notice. Web stream errors also clear on recovery. Connection and failed-action feedback stay visible in fixed areas of active dialogs on both platforms | Android shared `ConnectionLostCases` on JVM/device, API transport/local-I/O regression; web API recovery suite and settings browser journey |
| Appearance | Android-specific « Signal » palette since 0.32.0 (paper/ink with the Cairn blue accent, coral for attention), agent/project identity colours, light/dark/system themes, font scaling, touch-sized labelled actions | Native captures; large-text/keyboard device checks |

Validation results are recorded in `../VALIDATION.md`; entries above identify the
coverage used, not a claim that every production account/provider was exercised.

## Platform equivalents

- Navigation uses Android system Back, bottom sheets and a navigation rail on wide
  windows. Chat and run readers hide the global phone navigation to maximize room.
- A subagent's activity opens in a bottom sheet on every window size, where the wide web
  layout shows a side panel. A team row opens that sheet directly instead of expanding inline.
- The plan bar opens the whole plan in a bottom sheet, where the web shows it in a popover
  above the bar.
- Android document/photo pickers and system share/save/selection replace browser
  upload/download/clipboard controls. External OAuth uses Custom Tabs.
- Attachments: the attach sheet lists recent photos from the device (asked for only when the
  user taps the tile; Android 14 limited access is supported), which browsers cannot offer; the
  web menu has Photos, Camera and Files. File actions use a long press on Android and a ⋯ menu
  on the web. Message images keep a fixed mosaic size on Android so the history does not move
  while pictures load. Both viewers zoom images: pinch and pan on Android; an
  actual-size toggle with scrolling on the web. Web sharing prepares the original
  while the menu or viewer is open, then enables Share for a fresh user gesture.
  Both apps fetch original attachments through the authenticated binary stream
  route, including files up to the 10 MiB attachment limit when using Beacon.
  They fall back to the original download route on older installations.
  Thumbnail formats follow platform decoders: SVGs have a placeholder on Android
  and open in its full-screen viewer; browsers may show placeholders for HEIC.
- Installing a plugin package uses « Continuer sur le web ». Reviewing files and
  versions, approving packages, editing instances and configuring Discord are native.
- Foreground updates use SSE. Background question and execution notifications use
  account-level native push through Firebase on Android and Web Push in the browser.
  Android rechecks the account and installation access before displaying a push.
  Legacy WorkManager polling is cancelled; its retained `QuestionWorker` class only
  completes persisted work without fetching data or displaying notifications.
- Drafts survive in-app conversation switching; sensitive attachments remain in
  app storage and are cleared with session logout/forget. No draft is uploaded
  before the user sends it.

## Test data and privacy

Device UI captures use a local MockWebServer fixture, synthetic messages and
example.test links. They do not contain production conversations or credentials.


### Immediate wait results (#255)

Web and Android read the explicit `chat.wait_ready.payload.immediate` boolean
before the historical “Wait condition already completed” marker. An explicit
`false` keeps the result attached to the scheduled wait, even with the historical
text. Both clients also decode observations saved as JSON in `text` and replay
`tests/fixtures/activity-events.json` (the original 15 activity events plus six
wait-result cases). The event marker and reason are additive; the `wait_for`
response, stored schema and compatibility epoch stay unchanged. Older clients
can still read the event and its historical text. Cards retain their existing
native presentation and language on each platform; no layout changes.

### Older history pages (#263)

Both clients reject pages from a replaced history or stream epoch, keep accepted
pages through subsequent emissions/reconnects, and persist the combined displayed
snapshot. Android extracts these rules into the pure `OlderPages` class while the
web keeps them in `useLiveRun`.
Android also cancels a page request when its path/API Compose scope is disposed;
the web guards application with its connection generation. The lifecycle stream
resume adapters remain in place. `OlderPagesTest`, the `LiveTest` reconnect probe
and `OlderPagesScopeTest` cover the Android rules and scope/cache interactions.
See `docs/research/263-android-older-pages.md` for the discriminating experiment,
locking decision and limits; no performance gain is asserted.

The paging status matches the web's `role="status"` banner on small screens: like
its 90% width, the Android pill keeps a 12 dp margin from the screen edges, wraps
the error beside a whole « Réessayer » label, and becomes a rounded rectangle when
large text wraps. It is a polite live region for accessibility services. At
normal text size the pills keep their single-line capsule look and heights.
`pagingErrorFitsANarrowScreenWithLargeText` checks a 320 dp width at 150% text.

# Leo for Android

Version 0.36.0 shows the agent’s live step with the « Souffle » indicator and presents agent actions as a one-sentence summary that expands into a timeline, with full step details in a sheet.
Version 0.35.0 lets you paste images, or insert them from the keyboard, into the chat composer as attachments.
Version 0.34.0 introduces the « Signal » interface: Fil, Missions and Atelier tabs with an always-available new-conversation button, on-screen agent/project choice, missions with readable schedules and run history, a connection-first Atelier, universal search and a paper/ink palette with the Leo blue accent. See [release notes](RELEASE-NOTES.md).
Version 0.33.4 preserves assistant messages after verbose tool output and restores missing saved summaries on the server. Existing Android clients benefit without an app update.

Version 0.33.3 explains when `$` has no available skills or no matching results, alongside skill autocomplete and highlighted mentions (server 0.33.0).
Version 0.31.0 redesigns the GitHub repository picker: cards with language, stars and recent activity, visibility filters, highlighted search and automatic loading while scrolling (server 0.31.0).
Version 0.30.1 shows waiting reasons in conversations and links to Connections when Claude Code needs to reconnect.

Version 0.30.0 adds in-app APK updates from stable Git tags. See [update distribution](docs/UPDATES.md).

Native Kotlin / Jetpack Compose client for the Leo Agent Manager API in this repository.
Swipe left or right across a conversation to slide between active chats in the
selector's most-recent-first order. The full screens follow the finger, with the
neighboring chat's real content prepared ahead of time. Only the settled chat keeps
a live stream: neighbors show their cached history, or fetch one snapshot when
nothing is cached, then disconnect. Releasing snaps to a page,
and reversing the gesture lets you stay in the current chat. Drafts and reading
positions stay with each chat. The composer retains its editing gestures. List
updates wait until a swipe settles, and navigation stops at either end. New
conversations and archives do not participate.

Artifact previews also slide under the finger, revealing the neighboring file
before release. The gallery opens from Files, inline files and artifact links;
Files follows the displayed group/filter/version order, while links use the latest
versions and retain the exact linked version when it is older. The browsing list
stays fixed until the viewer is closed. Arrows animate the same pager, and a
position counter indicates the current file. Only immediate neighbors are kept
ready; files with a known size up to 10 MiB are prefetched, except audio/video.
Larger files, unknown sizes and media load when selected. Images pan while zoomed
and resume page swiping at their original scale. Media playback stops when its
page is left. No additional live connection is opened for this gallery.

Version 0.15.0 adds a configurable limit for simultaneous Claude conversations in Connections (server 0.29.0). The default is four; lowering the limit lets active conversations finish.
Version 0.14.0 displays Claude Code usage limits and reset dates in Connections (server 0.28.0).
Version 0.13.0 adds the GitHub repository picker and automatic project imports (server 0.27.0).
Version 0.11.0 adds Codex/Claude selection directly to the chat composer and preferences, with provider-aware drafts and queued messages. Server 0.25.0 preserves the chat and workspace when changing providers.
Version 0.10.2 refreshes model catalogs when opening the picker, returning to it, or choosing **Actualiser les modèles**. Server 0.24.2 includes Codex 0.156.1 for GPT-6 Sol and Luna discovery.
Version 0.10.1 allows account parallel-run limits above four with server 0.24.1.
Version 0.10.0 adds Claude Code sign-in and provider-aware agent/chat model selection.
Version 0.9.0 exposes model selection and a discrete reasoning slider directly in
the chat composer and shares these controls with agent settings.
Version 0.8.0 adds 1Password service accounts with explicit agent grants.
Version 0.7.0 introduced revocable public links for individual artifact versions, with
native copying and Android link sharing from the file viewer. Files stay private
by default; the server must include the public-artifact API introduced alongside
this release. It retains the Fil design to the native web-parity port: compact conversation
chrome, flat task rows and compact inline files, with the web palette and bundled
DM Sans / Manrope fonts. Body text stays at 16 sp / 24 sp and respects Android font scaling.
The latest-message arrow floats at the top right and fades away while scrolling.
It retains stable history loading and reading position during an active drag.
See the [parity audit](docs/WEB-PARITY.md) for the reference and validation scope.
Tool activity has native views: commands, reads, searches, diffs,
plans, MCP results and session notices. Structured results and JSON artifacts are
readable without opening the raw JSON. Chats use a compact composer and explicit conversation selector.
See [release notes](RELEASE-NOTES.md). The inventory below distinguishes implemented
paths from deployment and device validation.

Open **this `android/` directory** in Android Studio. The web application and server
keep their existing build and dependencies. The native MCP OAuth bridge is included
in the backend; older deployments use the browser fallback. No database migration is required.

## Build and connect

Requirements: JDK 21 (including Android 16 UI tests), Android SDK platform 37.0, build-tools 36.0.0. The checked-in
Gradle wrapper pins Gradle 9.4.1. AGP 9.2.1 supplies Kotlin 2.3.10; Compose compiler
and serialization plugins match it. Compose libraries use BOM 2026.09.00.
Minimum Android version: Android 8 (API 26). Compile/target API: 37.

```sh
# From android/, with ANDROID_HOME pointing to your Android SDK (or local.properties):
./gradlew testDebugUnitTest lintDebug assembleDebug assembleRelease
# Install the development APK on a connected phone:
adb install -r app/build/outputs/apk/debug/app-debug.apk
```

The app connects only to the official service. Sign in with a code received by
email, then choose an installation belonging to or shared with your Leo account.
It reopens the last installation used by that account. Members can read agents,
projects and skills and work in conversations; owner management is hidden.
Appearance, app updates and notification permissions remain available to members.
The selector shows each installation’s role and availability. With no installation,
the app links to the official claim page to add or associate one.
If the official service is temporarily unreachable, the app preserves the stored
session and offers a retry; an explicit rejection returns to email sign-in.
There is no server-address, owner-password or bootstrap-token form.

Set `LEO_OFFICIAL_ORIGIN` to the fixed official HTTPS origin when building the
app. The repository does not choose a production hostname; without this build
setting the app reports that the official service is not configured. Signed
distribution requires this setting and HTTPS. The Android workflow reads the
repository variable of the same name. This value is an origin, not a credential.

For local development, use `adb reverse tcp:4310 tcp:4310` and
`LEO_OFFICIAL_ORIGIN=http://127.0.0.1:4310` when building a **debug** app. Run the
official service on that port. HTTP is limited to localhost,
127.0.0.1 and the emulator host alias 10.0.2.2; the server still validates Host
against PUBLIC_URL. Prefer adb reverse so the existing localhost server config
works unchanged. Release builds require HTTPS and normal certificate validation.

The APK produced by `assembleDebug` is installable and development-signed. Local release APKs are unsigned unless the release signing environment variables
are configured; tag CI uses a persistent release key. Never commit a signing key or local SDK paths.

## Product and architecture

- Compose Material 3 components, system back navigation, edge-to-edge insets,
  keyboard handling, bottom navigation in the main phone sections and a rail from 700 dp.
  Chat/run details use their own compact header and system back navigation. Touching
  the activity pauses following; scrolling toward older text disables it. Scrolling
  to the end or tapping the down arrow resumes following, including bottom padding.
- Semantic colors match `src/styles/theme.css` at `207ca4f`: blue-violet accent
  `#4545ef`, ink `#28283c`, canvas `#fdfcfe`; dark accent `#b8b2ff`, canvas `#1b1b20`,
  surface `#222228`. Material roles also map borders, muted text and error colors.
  Settings → Appearance offers System, Light and Dark, saved on the device.
  Status/navigation bar contrast follows the selected appearance. Native typography,
  controls and touch targets retain Android behavior. UI text is French. Conversations use native sheets, focus modes and typography.
- An AndroidViewModel exposes immutable workspace state through StateFlow;
  Compose observes it with lifecycle awareness. Forms save drafts through
  recreation. Email verification codes and newly issued access tokens are never
  placed in saved instance state.
- Kotlin serialization models mirror `shared/contracts.ts` and the API response
  shapes. OkHttp requests support cancellation, enforce a 30-second timeout,
  refuse redirects, and send the existing cookie plus CSRF token.
- Appearance and the last installation per Leo account are stored in DataStore. The official origin is fixed at build time. The session cookie is encrypted
  with an AES-GCM Android Keystore key, bound to the origin with authenticated
  associated data, and written atomically outside Android backups. Cookies are
  checked against scheme, host, port, path and expiry. No password is persisted.
- Chats and run details consume the server’s SSE batches only while STARTED.
  Reconnection resumes from the accepted cursor, folds compressed assistant deltas,
  resets on run changes and replays malformed batches without skipping events.
  Logout cancels open streams before revoking the session. Other lists poll while visible.
- Chat hides routine session lifecycle notices and recovered connection retries.
  Failures and unresolved interruptions appear inline without an expandable session
  group. Run activity retains the full diagnostic history.
- Documents use the Android picker, bounded streaming uploads, authenticated file
  downloads and scoped FileProvider sharing. Image, PDF, Markdown, code and media
  previews use native Android components; no arbitrary artifact URL is fetched.
- Optional WorkManager checks notify about pending questions approximately every
  15 minutes with a network constraint. No Firebase service or keys are required.
  Android battery management can delay checks; force-stopping the app prevents
  background work until it is opened again. The preference and permission are
  managed in Settings, and notification taps open the corresponding conversation.
- Markdown uses native Android text rendering, including tables and
  strikethrough. There is no WebView. Provider device login and OAuth callbacks
  open a browser Custom Tab, as these pages belong to the external provider.
- Android is a client: scheduled work and agent processes keep running on the
  existing server when the phone is offline. There is no offline mutation queue.

## Coverage

This inventory was rechecked against web/backend commit `207ca4f` (2026-09-13).
The initial checkout, `1e0d71b`, had an obsolete green theme and fewer features.
The palette and API contracts follow this baseline. “Implemented” describes the
client path, not certification against a production deployment.
The 0.6.0 delta is tracked against v0.21.12 in the [parity audit](docs/WEB-PARITY.md).
See [validation](VALIDATION.md).

| Web section | Android coverage |
| --- | --- |
| Account / installations | Email code login, encrypted session restore, expiry handling, logout, installation selection and restoration, relayed requests and streams, member role masking |
| Overview (under Plus) | Running/completed/queued counts, agent and schedule counts, recent activity, upcoming tasks |
| Tasks | Create/edit/delete, name and tag search, scheduled/paused/one-off/archived filters, latest execution and status, run now, pause/resume, archive/restore, duplicate disabled |
| Task editor | Agent selection, optional project focus (all authorized projects), instructions, isolated worktree, tags, all authorized or individually selected skills, once/daily/weekly/custom cron, timezone, server schedule preview |
| Runs | Status and task filters, 30-item pagination, current status, results, sharing/copy through Android selection, duration and metadata |
| Run detail | SSE activity with follow/reconnection, original brief and snapshot, skills, usage, cancellation, retry, resume, recovery/account waiting states, multiple workspaces and guarded cleanup; chat runs link back to their conversation |
| Agents | List/search, create/edit/delete, server model/reasoning catalog, instructions, timeout, project/skill/GitHub access, dedicated GitHub token, sandbox, MCP server and per-tool restrictions |
| Projects | List/search, GitHub repository picker with filtering/pagination and automatic cloning, register/edit/delete, server path, description, base branch and origin |
| Skills | Search, global/project filters, create/edit/delete, validation errors, Markdown preview, supporting file list/read/create/edit |
| Connections | GitHub status/version and device login; Codex multi-account creation, renaming, reconnect, removal, pause, concurrency, quota windows, refresh, reset-credit status and active run links |
| Settings | Saved light/dark/system appearance, server/MCP details, grant list, scoped personal token creation, one-time token copying, revocation, 100 most recent audit entries |
| OAuth authorization | Share/paste an authorization link, verify its origin, preview client/scopes, explicit allow/deny, return to client |
| Chats | Native list/search, creation from agent/project, model/effort selection, streamed messages, queue editing/removal, steering, pause/resume/stop, blocking and optional questions, choices/custom/private answers |
| Attachments | Android file/photo picker, pasted images and keyboard image insertion, up to 8 files, 10 MiB per file / 40 MiB per message, stable upload/message identifiers for explicit retries |
| Artifacts | Authenticated file previews, downloads through Android’s save picker, sharing/open-with, native opening of Markdown artifact links, latest/all versions, groups and search |
| MCP servers | HTTP/stdio configuration, test/discovery, enabled tool selection, bearer/client secrets, environment variables, OAuth, removal/disconnect |
| Notifications | Opt-in Android permission, periodic pending-question checks, deduplication, cancellation of resolved alerts and opening the related chat; no Firebase |

Destructive actions use native confirmation dialogs and remain subject to the
same server checks as the web. No live task is launched as part of validation.

Self-hosted domains cannot be automatically verified as Android App Links by a
generic APK. OAuth requests can be shared from the browser to Leo, or pasted in
**Plus → Autoriser un assistant**. Domain-specific verified links can be added
when the deployment domain and signing certificate are fixed.

## Verification and distribution

The Android workflow runs JVM/network tests, Compose UI tests with Robolectric,
Android lint and both build variants. It uploads the debug APK and reports to the
workflow run. Stable `v*` tags also publish a signed APK and update manifest after checks pass.
The Android workflow does not deploy the server or publish to Google Play.

Optional native UI test captures (fixture data only):

```sh
./gradlew testDebugUnitTest -PleoScreenshotsDir=/tmp/leo-android-captures
```

## MCP OAuth and deployment

Provider browser tabs do not share the Android session cookie. The accompanying
Rust patch advertises `nativeMcpOauth` in `/api/settings` and supports:

1. Authenticated `POST /api/mcps/:id/connect` with `{ "native": true }`.
2. The provider returns to the existing public HTTPS callback. The server captures
   only the allowlisted response fields in the original ten-minute pending record.
   This browser step does not exchange credentials or reveal them in HTML.
3. The initiating Android session finishes through CSRF-protected
   `POST /api/mcps/:id/callback`. Session binding, issuer, PKCE, revision and replay
   checks remain enforced by the existing exchange. Other sessions cannot finish it.

The normal web OAuth flow remains supported. On a deployment without this patch,
Android opens the existing web MCP page to complete provider authentication there.
No deployment is performed by the Android build or CI.

The current web API has no conversation rename/delete endpoints; those actions
are not invented by the native client. Scheduled jobs continue on the server;
there is no offline mutation queue. Notifications deliberately use periodic
checks rather than the browser’s Web Push subscriptions, following the chosen
Firebase-free approach.

Release gates: physical-device validation against the intended HTTPS server,
deployment of the optional native MCP OAuth bridge, release signing identity and
distribution channel. See [validation](VALIDATION.md) for actual checks and limits.

Reference documentation used to select compatible Android versions:
[AGP 9.2](https://developer.android.com/build/releases/agp-9-2-0-release-notes),
[Compose BOM](https://developer.android.com/develop/ui/compose/bom),
[built-in Kotlin](https://developer.android.com/build/migrate-to-built-in-kotlin),
[Activity](https://developer.android.com/jetpack/androidx/releases/activity),
[Lifecycle](https://developer.android.com/jetpack/androidx/releases/lifecycle),
[WorkManager](https://developer.android.com/jetpack/androidx/releases/work),
[periodic work](https://developer.android.com/develop/background-work/background-tasks/persistent/getting-started/define-work).

## Conversation cache and paginated history

Chats and run activity restore a bounded local snapshot before reconnecting the
SSE stream. The snapshot includes the accepted cursor, decoded messages, tool
activity, artifact metadata and reading position. Files are loaded on demand.
Android encrypts disk records with an Android Keystore AES-GCM key, excludes them
from backups, and scopes them to the server and authenticated session. Signing
out, switching installations or losing access clears the cache and downloaded files. Storage failure falls back to the
normal stream. Limits: 12 histories, 4 MiB per history, 20 MiB total, seven days.
The cache retains up to 200 recent decoded events within a 3 MiB payload budget,
instead of discarding the whole conversation when it grows. The backwards cursor
is retained separately from the live cursor. A single event exceeding the cache
budget remains available from the server through the older-history control.
Reading positions are saved only when their content is still in the cached window.

The web client follows the same protocol using session-scoped IndexedDB records.
The backend emits a `history` revision (run identity + earliest retained event)
and validates it with `after`. Unchanged histories transfer only new events;
changed or pruned histories send a reset. Existing clients without a revision
remain supported.

New clients request `window=1` on individual chat/run streams. A cold start or
reset returns the newest page (up to 100 events / 256 KiB, allowing one larger
event), with `oldest` and `hasOlder`. `GET /api/{chats|runs}/{id}/history?before=…&history=…`
loads strictly older events in chronological order, using full text snapshots so
pages do not depend on another connection's deltas. Superseded assistant-text
snapshots are skipped, including across page boundaries; reused item IDs in
different turns remain separate. Partial SQLite indexes support these lookups.
A changed revision returns
409; it never mixes histories. Scrolling near the top loads an older page while
preserving the visible content; the explicit control also allows retrying errors.
Delivered chat messages are omitted from live metadata because their content is
already in the event history. Attachments and pending messages remain available.

This does not add an offline mutation queue or eliminate the
initial session check; it lets previously read content appear while the stream
reconnects.


## Streaming rendering (Android 0.5.2)

Assistant rows retain their first presentation ID while the wire event ID and
resume cursor continue advancing. The presentation ID is persisted with the
cached snapshot and survives reconnects; reused item IDs in later turns remain
independent.

Markdown is parsed as a complete document on a background dispatcher, preserving
reference links and unfinished Markdown syntax. Groups of eight top-level AST
nodes are fingerprinted and rendered separately. Unchanged groups retain their
spans and TextViews, so appending a response normally only lays out its last group.
The canonical HTML fingerprint is never displayed or loaded into a WebView.

Only fully decoded snapshots and complete source texts are conflated. Wire deltas
are always applied in order. Display work runs sequentially with an 80 ms pause
between completed renders, so bursts coalesce without indefinitely postponing
the last update. Following the conversation tracks measured view growth; restoring
a reading offset waits for the initial asynchronous Markdown layout.

Selection works within a rendered group. Its selection toolbar also offers
**Copier tout le texte** to copy the whole Markdown source. A single very large
paragraph, code block or table is deliberately not split across views, preserving
its formatting; such a block can still cost more to lay out. These changes do not
alter the backend or web client.

## Following a conversation (Android 0.5.3)

The down arrow reaches the measured end of the list, including its content padding.
Touching the conversation immediately suspends automatic scrolling. A deliberate
movement toward older text disables follow; a downward-to-end gesture, including
overscroll at the bottom, preserves it. Reaching the end manually resumes follow.
A stationary touch resumes the previous mode on release. Stream updates and viewport
resizing never re-enable follow themselves. Chat and run activity share this behavior.

The same Compose gesture regression cases run as JVM tests and device instrumentation:

```sh
./gradlew testDebugUnitTest --tests '*HistoryFollowTest'
./gradlew connectedDebugAndroidTest -Pandroid.testInstrumentationRunnerArguments.class=dev.leo.manager.ui.HistoryFollowDeviceTest
```

## 1Password

Connections includes service account creation, token rotation, connection testing,
disabling/deleting accounts, and explicit per-agent grants. No agent receives
access by default. Tokens use masked input and are not saved in instance state.
See [server setup and access](../docs/ONEPASSWORD.md).

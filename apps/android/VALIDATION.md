# Android 0.39.2 — inverted infinite history scroll

`HistoryPagingCases` (Robolectric and Android 16) prepends real pages to the production lazy list
with async Markdown: the visible paragraph keeps its pixel offset while reading, at the very start
of the loaded history, during a held drag (the drag keeps scrolling), and for a short history
that does not fill the screen. Folded pages chain until three screens are buffered, then stop;
a failure stops automatic loading until « Réessayer ». `TimelineTest` checks that an older page
extending the first activity group, or folding its only command, keeps the row key.

`HistoryScrollDeviceTest` drives the real chat screen on a device against a mock server: 60
tool-heavy turns, 100-event pages with a 700 ms round trip. Swiping from the last to the first
question requests each of the five pages once, and no visible question moves while a page lands.
On an API 36 emulator it passes; frame traces showed 0 px movement on every page arrival and no
row resizing once Markdown renders synchronously (before, each agent answer entering at the top
grew from 169 to 211 px and pushed the view by up to 42 px). The recorded run is the demo video.
Positions are compared on screen: the mock stream ends every few seconds, and the resulting
« Reconnexion… » status used to shift the conversation by 29 px; it is now an overlay pill.

## Previous validation: Android 0.36.0 — live step and agent actions

`WorkingIndicatorTest` covers the step choice (running step with its command, last finished
step otherwise, earlier turns and session notices ignored, failed steps never shown as
running, time from the latest message), per-second elapsed time, the polite live-region
announcement and both themes. `AgentStepsTest` covers the summary sentence, folding of
consecutive reads, named edited files, failures kept apart, hiding the running step while
the agent works, the expanded timeline and the step detail sheet. The chat, workspace and
Signal journeys now open steps from the timeline and read their output in the sheet.

Local validation (Robolectric, not an emulator): the complete unit and UI suite passes,
`lintDebug` reports no issue, and debug, instrumentation and release builds succeed.
The Android workflow runs the Android 16 device journeys before publishing.

## Previous validation: Android 0.35.0 — pasting images into the composer

`ImagePasteCases` is shared by the Robolectric and Android 16 device suites. It puts a
FileProvider PNG on the clipboard, pastes it into the composer through the text field's
paste action, and checks that the typed text is unchanged, the image appears as a
removable attachment, the upload carries the file name and size, and the sent message
references the attachment id. A plain-text paste afterwards still lands in the message.
`ScrollResumeTest` and `CacheMissReproductionTest` now target `conversation-history`,
because the state-based composer is scrollable too.

Local validation (Robolectric, not an emulator): 146 unit and UI tests run; the only
failures were intermittent `ChatJourneyTest` / `ChatProviderTest` Robolectric timing
errors that pass on rerun (`ChatJourneyTest` fails the same way on the previous commit
on this machine). `lintDebug` reports no error, and `assembleDebug`,
`assembleDebugAndroidTest` and `assembleRelease` succeed. The local emulator did not
boot (software emulation, 3 GB RAM); the Android workflow runs `ImagePasteDeviceTest`
with the other Android 16 device journeys.

## Previous validation: Android 0.34.0 — « Signal » interface

`SignalPresentationTest` covers the pure presentation logic: Fil grouping (questions,
Claude reconnection, failed conversations, failed or blocked missions, live work,
paused and recent conversations, archived and chat runs excluded), summaries and
greetings, French cron wording with verbatim fallback, mission filters and ordering,
universal search ranking and highlighting, past and upcoming date stamps, elapsed
times, dock selection per route and the Atelier connection summary.

`SignalJourneyTest` drives the real app against a mock server: Fil sections, retrying
a failed mission and answering a question; search results and launching a mission;
choosing agent and project on screen, with the agent's project policy applied and
the agent, project and provider checked in the HTTP bodies; the mission sheet with
schedule, history and success rate, pausing (PUT body) and running; the Atelier with
Claude disconnected, Codex capacity, 1Password and the run journal.

Existing journeys were moved to the new navigation rather than weakened:
`WorkspaceJourneyTest` (sign-in, mission creation and run, settings, theme),
`ChatJourneyTest` (queue strip edit, files badge, intervention, drafts, chooser),
`NativeUiTest` (mission filters and archived missions) and `NativeParityPreviewTest`
(phone, 360 dp, 130 % text and tablet layouts; the composer send face is now 40 dp
inside a 48 dp target). `ParityDeviceTest` was updated for the device suite.

Local validation (Robolectric, not an emulator): 139 unit and UI tests pass,
`lintDebug` reports no issue, and `assembleDebug`, `assembleDebugAndroidTest` and
`assembleRelease` succeed. The Android workflow runs the Android 16 device journeys
before publishing the signed release APK.

## Previous validation: Android 0.33.3 — empty skill suggestions

`SkillMentionCases` is shared by the Robolectric and Android 16 device suites.
It covers `$` with no available skills, a search with no matching skill,
Back dismissal without losing the draft, ordinary `$HOME` and `$5` text,
and sending the unchanged message while the empty state is visible.
The existing suggestion selection and highlighted history journey also remains covered.

Local validation: both UI journeys and the four skill logic tests pass, as does
`lintDebug`. The Android workflow validates the complete suite and the device
journeys before publishing the signed release APK.

## Previous validation: Android 0.9.0 — model and reasoning controls

`ModelPickerCases` runs in Robolectric and Android 16 instrumentation. It covers
slider touch and accessibility actions, inherited defaults, model search and
hidden models, resetting reasoning when switching models, saved-state restoration,
unknown models, unsupported saved efforts and 160% font scaling.
Both environments exercise slider touch directly and inside the modal sheet.
`ChatJourneyTest` checks the model and reasoning in the actual HTTP message body.
The Android workflow includes the new device suite alongside conversation checks.

Development APK: **0.9.0 / code 20**. Debug APKs are development-signed;
optimized release APKs remain unsigned. Current CI results are authoritative.

## Previous validation: Android 0.8.0 — 1Password service accounts

`OnePasswordJourneyTest` covers masked token entry, no grants by default,
explicit agent selection, CSRF-protected writes and preserving the saved token
when only access is edited. These are Robolectric tests, separate from the
existing Android 16 instrumentation gate. Backend tests cover encrypted storage,
revocation during a read, disabled accounts, token rotation, deletion and owner
session/CSRF requirements. Browser journeys cover desktop and narrow screens,
persistence after restart and access removal in Chromium and WebKit.

The development APK is **0.8.0 / code 19**. The current commit's Android and
Quality workflows are authoritative for the complete regression suites, lint,
optimized release build and Android 16 device checks.

## Previous validation: Android 0.7.0 — public artifact links

The artifact link UI regression now opens the sharing dialog, fetches current
visibility, enables a link, copies it to the native clipboard, verifies the
system-share action, and revokes the link. Mutation requests include the CSRF
token. The dialog scrolls for smaller displays and larger text. Server integration
and web browser tests separately cover anonymous download and revoked-link rejection.
The shared history suites and artifact sharing test use the Compose v2 test rule
(StandardTestDispatcher), preventing unconfined frame callbacks from resuming on
the Markdown worker during measurement or dialog disposal.
The development APK is **0.7.0 / code 18**. Current CI results are authoritative;
Robolectric tests are separate from the Android 16 instrumentation gate.

## Previous validation: Android 0.6.2 — floating latest-message control

The return-to-bottom control overlays the top right of chat and task transcripts,
fades out during a drag/fling, and fades back in after scrolling stops while newer
content remains below. It reserves no transcript/composer space and uses a 36 dp
face inside a 48 dp touch target. The existing ScrollResumeTest now checks overlay
placement, unchanged composer position, disappearance during a held drag,
reappearance after release and reaching the latest message on tap. The existing
93-test suite and Android 16 CI gate remain the validation baseline.
The development APK is **0.6.2 / code 17**. The current PR checks are authoritative.

## Previous validation: Android 0.6.1 — Fil density

The selected Fil design retains the web palette and bundles DM Sans / Manrope,
with a compact header/composer, flat task rows and a horizontal inline file rail.
Two additional native UI cases and an accessibility-scroll regression bring the
JVM/Robolectric suite to 93 tests. The UI cases
measure the header, composer and file rail on a 360 dp phone at normal and 130%
font scale, then scroll to the third file and open its native viewer. Existing
phone/tablet, long-link, keyboard, history and task-management cases remain.
The shared device suite also verifies explicit accessibility scrolling and
deferred native text relocation after a stationary touch (13 cases total).

Run `./gradlew testDebugUnitTest lintDebug assembleDebug assembleRelease` from
`apps/android/`. Add `-PcairnScreenshotsDir=/absolute/path` for native-graphics previews.
These images use Robolectric and fixture data, not a physical phone. Android 16
instrumentation remains a separate CI gate; the current commit's checks on
[PR #1](https://github.com/leo91000/cairn/pull/1) are authoritative.
The development APK is version **0.6.1 / code 16**. Font licenses are bundled in
its assets. Release optimization is checked separately from development signing.

## Previous validation: Android 0.6.0 — native web parity

The port targets web v0.21.12. The action-by-action feature map and deliberate
platform equivalents are in [WEB-PARITY.md](docs/WEB-PARITY.md).

The implementation passed the complete 87-test JVM/Robolectric suite, lint,
debug and optimized release builds in
[Android CI](https://github.com/leo91000/cairn/actions/runs/35584279965).
Lint reports zero errors and eight warnings. The same run passed 11 actual
Android 16 instrumentation tests, followed by the separate one-test pinned-scroll
capture. These include native chat switching, preserved drafts, evidence,
keyboard/enlarged text, fullscreen, task management and adaptive layout alongside
the existing history and gesture checks. The repository's
[quality, browser and container checks](https://github.com/leo91000/cairn/actions/runs/35584279943)
also passed.

Two additional native-preview tests cover phone and tablet layouts, long URLs,
completion evidence, task selection, the tablet chooser and selecting the main
agent by default when another agent appears first. They bring the complete suite
to 89 tests. An additional same-frame paging case brings the final suite to
90; it is also included in the device selection (12 cases). The final commit's PR checks are the authoritative validation of
these final additions.

Preview images generated with `-PcairnScreenshotsDir=...` use Robolectric native
graphics and synthetic data. They are not emulator or physical-phone screenshots.
Local software emulators did not finish booting reliably; the successful device
evidence above comes from the accelerated Android 16 CI emulator. Physical-phone
feel, screen-reader review and frame-time measurement have not been certified.

A race discovered by the device suite is fixed: the initial asynchronous Markdown
render can finish only once, including when its view is disposed during layout.
Two dedicated tests prevent the pending-render count from going negative.
A later held-finger device run exposed a missed reading anchor when a history
response overtook the scroll observer. Paging captures the visible old message synchronously at the response boundary,
before inserting rows, and releases loading in that same UI turn before caching.
The same-frame case checks the anchor and absence of cascading page requests;
the existing held-finger case continues checking real pointer input.

Package `build.cairn.app`; minSdk 26; targetSdk 37; version **0.6.0 / code 15**.
The downloadable APK is development-signed; the optimized release build is
validated separately. Background notifications retain Android's existing
WorkManager scheduling (roughly 15-minute checks, subject to OS delays), which
has different delivery timing from browser Web Push. Foreground updates use SSE.

## Previous validation: Android 0.5.6 — cascading history loads and held gestures

Two regression cases reproduce the 0.5.5 failure with the older-history control
visible and more pages available: with an idle reader and with a finger still
held in a drag. Both originally requested two pages instead of one. The corrected
cases assert one request and an unchanged visible text offset; the held-finger
case also continues the same gesture after the page arrives.

The tests run under Robolectric and are included in the Android 16 CI
instrumentation selection through `HistoryFollowDeviceTest`. Physical-phone
validation remains separate from these automated checks.

## Previous validation: Android 0.5.5 — older-history reading position

The local JVM/Robolectric suite passes 73 tests. Six new paging cases use the
production lazy list, asynchronous Markdown renderer and paging hook: stable text
position, movement during loading, removal of the visible loading control, long
first messages, consecutive pages, and returning to the latest message while a
page is pending. The existing cache pagination test also covers a fast response
through a simulated HTTP server. Before/after captures of the stable-position
case are byte-identical. Lint reports zero errors and seven warnings.

These paging checks simulate list movement through its scroll APIs; they are not
physical-device gesture evidence. The tactile feel of this correction on a phone
remains to be confirmed.

## Previous validation: Android 0.5.4 — message links

Targeted checks pass for a first tap opening exactly once, long-press selection,
dragging without opening a link, and authenticated metadata/file loading for an
artifact absent from the current conversation. The gesture case is shared with
the Android 16 instrumentation suite. Robolectric stubs only the selection
magnifier surface, which it cannot render; the device test uses the real magnifier.

The web deliverables journey passes on Chromium and WebKit: relative and absolute
artifact links, Markdown/image previews, external browser navigation, a missing
file, and links in both chats and run activity. Shared origin validation tests pass.
No backend change or database migration is required.

## Previous validation: Android 0.5.3

## Touch and follow behavior

Chat and run activity use `HistoryFollowGesture` and `FollowHistoryTail`. The list's
normal touch slop determines when a movement starts; no extra unpin threshold is
added. Pointer contact pauses following before the first scroll delta. Only real
user scrolling toward older content disables following. Reaching the end through
user scrolling enables it; layout growth and programmatic scrolling cannot.

The shared `HistoryFollowCases` runs with Robolectric and Android instrumentation.
It checks actual Compose pointer input with native Markdown, not just calls into
the gesture state machine: bottom padding, downward overscroll, a held finger while
text grows, small upward-history movement during growth, manual return with a fling,
direction reversal, stationary contact, viewport resizing and an oversized final
message. The local suite passed all 63 tests (including the five gesture cases); lint
reported zero errors, and debug, test and optimized release APKs built successfully.
The local Android 16 software emulator exceeded its ten-minute boot timeout before
application installation. The same gesture cases can be run on a connected Android device with
`connectedDebugAndroidTest`; the instrumentation test also saves a screenshot in
`files/scroll-validation/pinned-stream.png` within the app's private test data.

## Retained streaming optimization (0.5.2)

- Assistant presentation IDs remain stable across delta updates, cache serialization
  and reconnects. New turns isolate reused provider item IDs. Wire IDs/cursors still
  advance normally.
- The native Markdown views reuse unchanged groups of eight top-level AST nodes.
  Full-document parsing and changed-group rendering run on a background dispatcher.
  Reference definitions added or edited later invalidate earlier affected links.
  No raw Markdown is split at arbitrary character or newline boundaries.
- Completed source snapshots coalesce while rendering; every wire delta is applied.
  A deliberately stalled display test receives all 100 fragments in the final answer.
- Native Compose tests check stable TextView/text identities during a burst, final
  text delivery, corrected/empty documents, asynchronous bottom following, pausing
  follow to read earlier text, and restoring a 400-pixel reading offset after initial
  Markdown measurement. Existing cache/pagination and foreground-resume regressions
  are included in the Android suite.
- Plain tool output bypasses JSON parsing. Activity groups accumulate linearly
  instead of repeatedly copying their entire prefix.

## Measurements

`StreamingCostTest` compares the previous full-text view/renderer recreation path
with the new reusable-block renderer on the same synthetic inputs. It takes 15
samples after three warmup updates. These are component timings on a 2-CPU Linux VM
under Robolectric native graphics, **not phone frame rates or end-to-end latency**.
Both modes use the updated accumulator/timeline; only rendering uses the old path
in the baseline. There are no timing thresholds in the test.

Local medians in milliseconds per update:

| Initial characters | Prior events | Old Markdown/bind | Old text layout | New parse/render (background) | New bind + text layout |
| ---: | ---: | ---: | ---: | ---: | ---: |
| 2,000 | 80 | 8.30 | 60.60 | 1.17 | 2.49 |
| 20,000 | 80 | 29.69 | 110.88 | 7.43 | 5.74 |
| 100,000 | 80 | 336.78 | 1000.78 | 16.86 | 5.83 |
| 100,000 | 2,000 | 322.80 | 878.69 | 16.05 | 3.37 |

For the 100,000-character / 80-event case, the old text-layout median is about
1,001 ms versus 5.83 ms for the new binding-plus-layout measurement. Background
parse/render takes 16.86 ms. These columns do not measure identical components:
the new UI column includes binding as well as layout, while the old binding is
included in its Markdown column. Medians must not be summed into an asserted
end-to-end median. Renderer/view construction is also recorded separately.

Each benchmark update calls bind/measure on all retained views, including unchanged
ones. The new view binding skips setText when its block object is unchanged. The
benchmark excludes initial cold rendering, Compose frame overhead, the network,
provider and production proxy. It retains the old cache timing fixture, whose JSON
and disk I/O are real but whose codec excludes hardware Keystore encryption.

An old diagnostic measured roughly 122 ms to build a timeline with 2,000 plain
output events; the optimized implementation measured 0.45 ms in this run. This
cross-run observation is indicative rather than a controlled old/new comparison.

## Commands and delivery

```sh
mise exec java@temurin-21 -- apps/android/gradlew -p android --no-daemon testDebugUnitTest lintDebug assembleDebug
mise exec java@temurin-21 -- apps/android/gradlew -p android --no-daemon assembleRelease
```

The GitHub Android workflow runs the complete suite, lint, debug and optimized
release builds on the published commit. See that commit's checks for final CI
results. Local screenshots, when enabled with `-PcairnScreenshotsDir=...`, use
Robolectric native graphics. They are not emulator or physical-device evidence.

Package `build.cairn.app`; minSdk 26; targetSdk 37; version **0.5.3 / code 10**.
The development certificate is the same as the preceding APKs, permitting an
in-place update. Certificate SHA-256:
`b56038aa26883922efdf917ff45300b9d5a6f59cc7901c5e4097664a2abbbf99`.

## Limits and preserved cache behavior

A single very large paragraph, fenced code block or table remains one AST node and
can still take longer to lay out. Initial opening lays out the visible message's
groups; the benchmark concerns subsequent appends. Selection is native within a
group, with **Copier tout le texte** in the selection toolbar for the full Markdown
source. Device-level frame timing, keyboard transitions and TalkBack remain to be
checked on an Android device.

The 0.5.1 cache fix is retained: up to 200 recent events within a 3 MiB payload
budget, backwards pages for older events, and separate backwards/live cursors.
Server history is preserved. Android encrypts its snapshots; cache isolation,
logout invalidation and generation checks remain. Limits are 12 histories,
4 MiB per encoded record, 20 MiB total and seven days. Artifact files load on demand.

This change is Android-only. It requires no new backend API or web-client change.
The Markdown integration uses Markwon's separate parse/render/setParsedMarkdown
stages, as described in its [plugin lifecycle documentation](https://noties.io/Markwon/docs/v4/core/plugins.html).

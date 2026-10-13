# « Signal » web interface

Since 0.35.0 the web app follows the Android « Signal » design: three places, one
accent and motion that only means an agent is working.

## Places

- **Fil** (`/`): what needs you (questions, failed conversations, failed missions,
  missions blocked or waiting for input, then finished work you have not read), live work (running and queued
  conversations and missions) and recent conversations. `apps/web/src/signal.ts` groups
  them; only the latest run of each mission counts and chat runs stay with their
  conversation. On wide screens the Fil stays beside the open conversation, like a
  mail client. A mission opens the same way (`/fil/runs/:id`): its latest run beside the
  Fil, with a **Mission** tab holding the mission's schedule, history and actions. Runs it
  starts stay beside the Fil; `/runs/:id` remains the Atelier's run page.
- **Missions** (`/tasks`): scheduled and one-off tasks with their runs.
- **Atelier** (`/atelier`): agents, projects, skills, MCP servers, connections,
  the run journal and settings. Its pages share a section bar.

Desktop and tablet use a rail with a sliding indicator, badges (coral count of
what needs you, a live dot while a mission runs), the new-conversation button,
search, shortcuts, appearance and sign-out. Phones use a floating dock that hides
while reading a conversation or a run; conversations and missions opened from the Fil go
back to it.

## Keyboard

| Keys | Action |
| --- | --- |
| ⌘K / Ctrl+K | Search and commands: conversations, missions (open or run now), agents, projects, skills, places, theme |
| C | New conversation |
| G then F / M / A | Fil / Missions / Atelier |
| J / K, Enter, E, / | Move in the Fil, open, mark finished work as read, filter |
| R, Esc | Reply in the open conversation, leave the field |
| Enter, Shift+Enter, Alt+Enter | Send or queue, new line, steer |
| ? | Shortcut sheet |

Single keys never fire while typing or while a dialog is open (`apps/web/src/shortcuts.ts`).

## Unread work

A conversation turn or a mission run that succeeded stays **unread** for each Cairn account until
that account opens it. It waits in For you, below what needs an answer, as a card with an accent
edge, an unread dot, a « Finished » or « Mission finished » label and the opening of the agent's
final message. « Read » opens it; « Mark as read », E on the selection and « Mark all read » in the
For you header clear it without opening. The rail badge stays coral while something needs an
answer and turns to the accent when work only waits to be read.

Read marks live on the installation, per account (`crates/installation/src/reads.rs`): opening work
on the phone clears it on the web, and each member reads on their own. `POST /api/reads` records
`chat:<id>` or `task:<id>` items; the conversation list, its live stream and `/api/tasks/activity`
carry `unread` and `preview` for the caller. An account's first read starts its history, so earlier
work is not unread. Failed, stopped and paused work is never unread, nor are plugin
conversations, which answer in their own channel; a turn that finishes while its conversation is
open and visible is read as it happens. Finished work opened from For you stays
there, read, until the user moves on, then joins Recent. Android shows the same rows in « Pour
vous », with « Tout lire », and reads the page settled in the Fil pager while the app is in the
foreground.

## Motion

`apps/web/src/styles/signal.css` holds the shared primitives:

- **Souffle** (`WorkingIndicator.vue`): a breathing halo, a light sweep across the
  step in progress (with its command) and the elapsed time to the second. It is
  announced politely. `workingStep` ignores earlier turns and session notices and
  never presents a failed step as running.
- **Veille** (`WorkingIndicator.vue`): when the agent finished responding and only
  waits for its background tasks (`turn.waiting`), a slow dashed ring replaces the
  breath, the tasks appear as pills, the time counts from the start of the wait and
  the conversation header reads « Background task ». `backgroundWait` ends the wait
  as soon as the agent acts again, the tasks finish or the user writes.
- **Live avatars** (`AgentAvatar.vue`): a comet arc orbits working agents; it
  overflows the avatar so rows stay aligned. "Working" is followed by three dots
  rising in a wave. Queued and paused work keeps a static badge.
- Fil rows enter, leave and reorder with FLIP transitions; places rise in; the
  palette and shortcut sheet spring open; cards lift on hover.

`prefers-reduced-motion` stops every animation and transition.

## Evidence

`apps/web/tests/signal.test.ts` covers grouping, unread work, working steps, elapsed times and the
identity colours shared with Android. `crates/installation/tests/reads.rs` covers read marks per
account, previews and the live list. `tests/e2e/unread.spec.ts` reads finished work on the desktop
and the phone; `FilUnreadJourneyTest` covers the Android Fil. `tests/e2e/signal.spec.ts` drives the real
server: Fil sections and badges, J/Enter, G chords, the shortcut sheet, the
palette running a mission, the conversation beside the Fil with its live step,
R/Escape/C, and the phone dock with full-screen reading.

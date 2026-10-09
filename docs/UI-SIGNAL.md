# « Signal » web interface

Since 0.35.0 the web app follows the Android « Signal » design: three places, one
accent and motion that only means an agent is working.

## Places

- **Fil** (`/`): what needs you (questions, failed conversations, failed missions,
  missions blocked or waiting for input), live work (running and queued
  conversations and missions) and recent conversations. `apps/web/src/signal.ts` groups
  them; only the latest run of each mission counts and chat runs stay with their
  conversation. On wide screens the Fil stays beside the open conversation, like a
  mail client.
- **Missions** (`/tasks`): scheduled and one-off tasks with their runs.
- **Atelier** (`/atelier`): agents, projects, skills, MCP servers, connections,
  the run journal and settings. Its pages share a section bar.

Desktop and tablet use a rail with a sliding indicator, badges (coral count of
what needs you, a live dot while a mission runs), the new-conversation button,
search, shortcuts, appearance and sign-out. Phones use a floating dock that hides
while reading a conversation or a run; conversations go back to the Fil.

## Keyboard

| Keys | Action |
| --- | --- |
| ⌘K / Ctrl+K | Search and commands: conversations, missions (open or run now), agents, projects, skills, places, theme |
| C | New conversation |
| G then F / M / A | Fil / Missions / Atelier |
| J / K, Enter, / | Move in the Fil, open, filter |
| R, Esc | Reply in the open conversation, leave the field |
| Enter, Shift+Enter, Alt+Enter | Send or queue, new line, steer |
| ? | Shortcut sheet |

Single keys never fire while typing or while a dialog is open (`apps/web/src/shortcuts.ts`).

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

`tests/signal.test.ts` covers grouping, working steps, elapsed times and the
identity colours shared with Android. `tests/e2e/signal.spec.ts` drives the real
server: Fil sections and badges, J/Enter, G chords, the shortcut sheet, the
palette running a mission, the conversation beside the Fil with its live step,
R/Escape/C, and the phone dock with full-screen reading.

# « Signal » web interface

The web and Android apps share three places, a common accent, and motion for
agents at work.

## Places

- **Fil** (`/`) groups questions, failures, blocked outcomes, unread results and
  live or recent conversations. On wide screens it stays beside the open conversation.
- **Rules** (`/rules`) contains hook rules and their histories. **Schedule an agent**
  creates a rule that uses the Clock to start a conversation.
- **Atelier** (`/atelier`) contains agents, projects, skills, MCP servers,
  connections, the run journal and settings. Its pages share a section bar.

Desktop and tablet use a rail with a sliding indicator, badges (coral count of
what needs you, a live dot while a conversation runs), the new-conversation button,
search, shortcuts, appearance and sign-out. Phones use a floating dock that hides
while reading a conversation or a run. Conversations opened from the Fil return to it.

## Keyboard

| Keys | Action |
| --- | --- |
| ⌘K / Ctrl+K | Search for conversations, agents, projects, skills, places, and commands |
| C | New conversation |
| G then F / R / A | Fil / Rules / Atelier |
| J / K, Enter, E, / | Move in the Fil, open, mark finished work as read, filter |
| R, Esc | Reply in the open conversation, leave the field |
| Enter, Shift+Enter, Alt+Enter | Send (steering while the agent works), new line, queue for the next turn |
| ? | Shortcut sheet |

Single keys never fire while typing or while a dialog is open (`apps/web/src/shortcuts.ts`).

## Unread work

The **À lire** view shows unread results, errors, blocked outcomes and pending
questions for the signed-in account. A conversation started by a rule stays quiet
until someone participates or the conversation needs attention. **Mark as read**,
E on the selection and **Mark all as read** acknowledge the displayed activity.

Read marks live on the installation per account. The generated `mark_read`
operation at `POST /api/reads` takes items shaped as
`{ target: { kind: "chat", chatId }, finishedAt }`, up to 200 at a time. The
conversation list and its stream carry the account's `unread`, `readAt` and
`preview` values. Public MCP uses the account that authorized the grant.

**Snooze until…** gives a conversation a per-account reminder. Its deadline
or **Restore** returns it to attention. Later activity clears a snooze tied
to an older observation. Android follows the same behavior with native
controls. Both clients mark a synchronized conversation read when it is open
and visible.

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
  overflows the avatar so rows stay aligned. In the conversation header the ring
  replaces any "Working" label. Queued and paused work keeps a static badge.
- Fil rows enter, leave and reorder with FLIP transitions; places rise in; the
  palette and shortcut sheet spring open; cards lift on hover.

`prefers-reduced-motion` stops every animation and transition.

## Evidence

`apps/web/tests/signal.test.ts` covers grouping, unread work, working steps, elapsed times and the
identity colours shared with Android. `crates/installation/tests/reads.rs` covers read marks per
account, previews and the live list. `tests/e2e/unread.spec.ts` reads finished work on the desktop
and the phone; `FilUnreadJourneyTest` covers the Android Fil. `tests/e2e/signal.spec.ts` drives the real
server across Fil sections and badges, keyboard shortcuts, the palette,
conversation reminders, and full-screen phone reading. The schedule journeys
in `tests/e2e/task-focus.spec.ts` cover rule creation and Run now.

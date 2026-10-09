# Workspace scrolling

The authenticated shell fills `100dvh`. The document never scrolls behind it.
Ordinary screens scroll their main content area; Tasks and individual runs use
the remaining viewport space for the selected panel instead.

In a run, Activity, Result, and Task brief each own their content scrolling.
The title, tabs, and controls stay available. Run metadata lives in Task brief
instead of taking up conversation space. Activity is keyboard focusable;
Page Down and other native scrolling keys work in its focused region.
Expanded command output, file previews, and structured artifacts grow inside
that conversation instead of opening additional vertical scroll panes.

On narrow screens, the task selector replaces the detail panel while choosing a
task. Short screens show the selected task and status in the selector and omit
the repeated heading. Landscape layouts keep the list and detail side by side
when there is room. Fullscreen Activity remains available at every size.

Dialogs use their existing scroll container. Tool lists and skill previews grow
inside it; editable text controls and floating select popups retain their native
interaction. The sidebar can scroll independently from the content.

Tailwind layout utilities in `App.vue`, `Tasks.vue`, `RunWorkspace.vue`, and
`ActivityFeed.vue` own the shrinking viewport chain. Contextual and short-screen
rules live in `apps/web/src/styles/shell.css`, `tasks.css`, and `runs.css`. Keep `min-h-0`
throughout shrinking flex/grid chains, including the Activity mount used for
fullscreen teleportation. Do not give a nested content region its own `dvh`
height: it cannot account for the title, controls, or mobile navigation above it.

The regression test initially reproduced 282 pixels of unwanted document
scrolling alongside Activity. `tests/e2e/scrolling.spec.ts` checks actual content
visibility, keyboard scrolling, action-menu visibility, and tab padding at
1440×900, 390×844, 390×664, 320×568, and 844×390 in both themes and browser engines.
The global layout suite also checks for nested vertical scroll containers across
screens, dialogs, expanded artifacts, and fullscreen views. Screenshot evidence
is generated with each browser run.

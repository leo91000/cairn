# Shared select

`src/components/VirtualSelect.vue` is the single-select combobox used for task
agents, projects and schedules, agent reasoning, run status, skill scopes, and
supporting files, installations (including MCP consent), execution nodes and
conversation views. New select fields should reuse it.

```vue
<VirtualSelect
  v-model="agentId"
  label="Agent"
  :options="agents"
  placeholder="Choose an agent"
  required
/>
```

Each option has a unique string `value` and a `label`. Optional `description`,
`group`, `keywords`, `icon` (a Vue component), and `disabled` enrich its display and
search. Search is case/accent insensitive and matches every entered word across
these fields. An empty string can represent a valid filter option such as “All
outcomes”; required fields must use a nonempty value.

The component also accepts `icon`, `searchPlaceholder`, `emptyText`, `disabled`,
`loading`, `clearable`, `compact`, and `hideLabel`. Clearing emits an empty string.
Even when the visible label is hidden, `label` supplies the accessible name.
An optional `status` supplies a label and a semantic `success`, `warning`, or
`muted` tone. Options show a dot and badge; the closed control shows a dot and
announces the selected status. Keep status search terms in `keywords` when needed.
Selected names and option labels are truncated with a full-name tooltip.
An optional `actionLabel` adds a separated final action after the options and
emits `action` without changing the selection. The installation picker uses it
for “Add an installation”. Tab reaches the action; Escape restores focus to the
combobox. Single-installation headers remain plain text; adding an installation
is also available in Settings → Installation.
Required fields participate in native form validation and focus the visible
combobox when missing a selection.

Options are rendered in a virtual window with overscan. Described options are
62 pixels tall, simple options 44, and group headings 30. Keep these dimensions
aligned with `src/styles/select.css`; descriptions are single-line and truncated
visually, while the full description remains available to assistive technology.

Arrow keys, Home/End, Page Up/Down, Enter, Escape, and Tab support keyboard use.
Disabled options are skipped. Searching does not change the selected value until
an option is chosen; Escape restores its label. The list exposes its total size
and each rendered option's position through ARIA.

The native Popover API puts the menu above dialogs without moving it outside the
form's DOM subtree. Positioning responds to scrolling, resizing, and the visual
viewport, with upward opening where space is limited. This application targets
modern browsers with native popover support.

`tests/select.test.ts` checks search and window geometry. The Chromium/WebKit
journeys check every editor's menu at four viewport sizes, keyboard selection
from 10,000 options with fewer than 20 mounted rows, accent-insensitive search,
empty results, and focus restoration.

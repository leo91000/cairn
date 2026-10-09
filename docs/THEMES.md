# Appearance

Cairn defaults to the device's preferred color scheme. Choose **System**, **Light**,
or **Dark** from the navigation rail, on the sign-in/setup screen, or in
**Atelier → Appearance** and **Settings → Appearance**. The preference belongs to this browser and origin;
it does not change another person's device or workspace settings.

System mode responds immediately when the OS changes its appearance. An explicit
Light or Dark choice survives reloads and stays selected across OS changes. Tabs
on the same origin synchronize. Clearing the preference restores System. If
browser privacy settings block storage, selection still works for the current page.

## Implementation

`apps/web/public/theme.js` is a small, same-origin, blocking head script. It resolves the
preference before Vue starts, sets the document's theme and native `color-scheme`,
and updates the browser's theme-color metadata. The initial document also provides
a matching background before the main styles arrive. The bootstrap is revalidated
along with the HTML, so an old cached controller cannot outlive a new deployment.
No inline JavaScript, additional dependency, or CSP relaxation is required.

`apps/web/src/theme.ts` connects that controller to Vue. `ThemeControl.vue` uses native radio
semantics, a keyboard-accessible popover, focus restoration, and viewport bounds.
The same control renders as three preview cards in Settings.

The « Signal » palette in `apps/web/src/styles/theme.css` (paper and ink with the Cairn blue as
the single accent and coral for what needs attention, see [Signal](UI-SIGNAL.md))
defines canvas, panels, raised surfaces,
inset code/inputs, borders, text, and semantic colors. Tailwind utilities such as
`bg-surface`, `text-ink`, and `border-line` resolve through `light-dark()` pairs.
Dark mode covers cards, tables, forms, validation, toasts,
selects, overlays, authentication/consent, and all activity artifacts, Markdown,
JSON, code highlighting, and diff additions/deletions. Native controls and
scrollbars inherit the selected color scheme. Theme previews intentionally depict both
light and dark surfaces.

When adding a component, use the semantic Tailwind colors. Add new color pairs
to the shared theme when a distinct meaning is necessary. Test both modes;
avoid hard-coded white surfaces. Keep focus,
selected, error, warning, and disabled states distinguishable.

## Browser evidence

`pnpm check` covers lint, TypeScript, unit tests, and the production build.
`pnpm test:e2e` exercises the real production server and fixture worker.

The screenshot matrix covers the Fil, Missions, Runs, Agents, Projects, Skills,
Connections, Settings, and run Result/Activity/Brief. It opens all six editors,
search, virtual selects, fullscreen activity, source/Markdown views, operation
cards, structured data, and historical output. Chromium and WebKit both run dark
mode at 1440×1000, 320×568, 390×664, 430×932, and 844×390. Their existing light
mobile matrices remain enabled. Geometry checks reject page overflow, clipped
dialogs/popovers, and unexpected bright surfaces in dark mode.

Additional captures cover setup, sign-in, OAuth consent, validation feedback,
empty tasks, pending/failed connection sign-in, and the appearance menu. Connection
codes in these captures are synthetic. Preference tests verify persistence, OS
changes, cross-tab synchronization, keyboard focus, and a dark background even
when the application modules are blocked. Unit tests also cover invalid saved
preferences and unavailable storage.

Representative reviewed captures:

- [Desktop screens](screenshots/dark-pages-desktop.jpg)
- [Mobile screens](screenshots/dark-pages-mobile.jpg)
- [Mobile editors and select](screenshots/dark-editors-mobile.jpg)
- [Activity and search](screenshots/dark-activity-mobile.jpg)
- [WebKit mobile screens](screenshots/dark-webkit-mobile.jpg)
- [Empty and connection states](screenshots/dark-feedback.jpg)
- [Settings](screenshots/dark-settings-desktop.png)
- [Theme menu](screenshots/dark-theme-menu-mobile.png)

The complete per-screen captures are retained in the `browser-evidence` CI artifact
for 14 days. Screenshots and automated layout checks are review evidence, not a
pixel-diff baseline or a claim to have tested every possible user-generated value.

## Cairn · Balise palette

The approved B identity uses Prussian blue `#14365A`, a light-mode primary
button with white text, and `#8DB8E8` with dark ink `#0F0F14` in dark mode.
Links use the same primary pair. The ocher flame `#F2A93B` belongs only to the
logo; attention, warning and success retain their existing semantic colors.

WCAG AA contrast ratios: white on Prussian blue **12.32:1**; Prussian blue on
paper `#F4F2EC` **11.00:1**; dark ink on `#8DB8E8` **9.24:1**. The dark icon
outline `#346AA3` distinguishes the mark from the dark canvas. Sources live in
`apps/web/public/brand`; `uv run scripts/generate-brand-icons.py` regenerates
PNG icons and social/email images. Icons at 16/32 px use the optical mark without
arcs. Android uses the same geometry within the adaptive-icon safe zone.

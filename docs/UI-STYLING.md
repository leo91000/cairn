# UI styling

The UI uses Tailwind CSS 4 through its Vite plugin. `apps/web/src/styles/index.css` is the
single stylesheet entry point. Vite's existing Docker build copies all of `src`,
so the same theme, utilities, and icons compile locally and in the release image.

## Working conventions

- Put layout, spacing, and straightforward presentation in the Vue template.
  Prefer ordinary utilities and semantic tokens over new CSS selectors. Use
  arbitrary values for genuinely specific geometry, such as safe-area insets
  and virtualized rows, rather than adding one-off theme tokens.
- Reuse behavior and visual states through `UiButton`, `UiSegments`, `UiAlert`,
  `Status`, `Modal`, and `VirtualSelect`. Do not make a wrapper for every repeated
  flex row. Native fields share baseline typography and focus styles.
- `apps/web/src/ui.ts` owns button variants and icon-button styles. `UiButton` and native
  icon buttons merge caller utilities with `tailwind-merge`, so local sizing or
  responsive visibility replaces defaults deterministically. Links retain their
  native navigation semantics and use the shared button styles when appropriate.
- Theme colors live in `apps/web/src/styles/theme.css`. Use `bg-surface`, `bg-inset`,
  `text-ink`, `text-muted`, `text-accent`, and `border-line`. The existing theme
  controller and `light-dark()` support system, saved light, and saved dark
  preferences without maintaining a second palette in each component.
- Keep CSS for generated Markdown/highlight markup, browser primitives, and
  relationships across components such as embedded/fullscreen run layouts.
  These rules are grouped by feature in `apps/web/src/styles`; do not add another global
  override stylesheet. `@apply` is limited to baseline element styling, rather
  than recreating the old class system with utility aliases.
- Descriptive classes such as `activity-scroll` remain useful for structural
  selectors and browser checks. They are not a second utility API.

The `phone`, `tablet`, `compact`, and `short` variants describe the existing
responsive breakpoints. Preserve `min-h-0` and `min-w-0` through shrinking layouts.
See [workspace scrolling](UI-SCROLLING.md) before changing panel heights.

## Icons

`@egoist/tailwindcss-icons` discovers the installed Lucide, Tabler, and Simple
Icons collections. `apps/web/src/icons.ts` contains complete literal utility names, which
Tailwind can discover at build time. Add an exported constant there and pass it
to `Icon`, or use it in a typed select option. Never concatenate icon utility
names from user input or fetch icons at runtime.

Lucide supplies the main UI vocabulary, Tabler supplies the agent navigation
mark, and Simple Icons identifies Codex/OpenAI, GitHub, and MCP. Collections are
development dependencies; only the referenced icon rules enter the compiled CSS.
`Icon` centralizes dimensions and marks decorative glyphs as hidden from assistive
technology. Give icon-only controls an accessible label.

## Verification

Run `pnpm check` and `pnpm test:e2e`. The browser matrix checks rendered icons,
theme persistence, desktop/mobile screens, editors, search/select controls, OAuth,
artifact previews, fullscreen activity, and scroll ownership in Chromium and
WebKit. It generates screenshots from synthetic fixtures. The icon unit test
rejects names missing from their installed collections.

References: [Tailwind utilities](https://tailwindcss.com/docs/styling-with-utility-classes),
[theme variables](https://tailwindcss.com/docs/theme), and
[Egoist icons](https://github.com/hyoban/tailwindcss-icons).

# Naming exceptions after the Cairn / Beacon cutover

The audit command is `git grep -iE '\bleo\b|leo-|leo_|LEO_|dev\.leo'`.
Its remaining matches are intentional:

- `docs/adr/0001-*` through `docs/adr/0033-*`: immutable historical decisions,
  incident logs, commands, paths and measured evidence. Their original names are
  preserved so earlier release evidence remains intelligible. The current
  naming decision is ADR-0034.
- This file's audit expression describes the audit itself; it is not an active
  runtime identifier.

The GitHub owner `leo91000` remains unchanged. The repository was renamed to
`leo91000/cairn` on 2026-10-09 with Léo's approval; every reference outside ADRs
now uses the new name, and GitHub redirects links that still use the old one.

The complementary audit is `git grep -niE 'official'`. Remaining occurrences
refer to third-party official tools or documentation, rather than Beacon:

- `.env.example`, `crates/installation/src/accounts/{claude,mod}.rs`,
  `crates/installation/src/claude.rs`, `apps/android/RELEASE-NOTES.md`,
  `tests/container-smoke.mjs`, `tests/fixtures/claude.mjs`: Claude Code/Codex
  official CLI or sign-in flow; the release note is historical evidence.
- `crates/installation/tests/mcps.rs`, `docs/MCP.md`, `docs/PLAN.md`,
  `docs/QA.md`, `docs/RUST-BACKEND.md`: official third-party MCP SDK clients.
- `docs/AGENT-ACCOUNTS.md`, `docs/CLAUDE-CODE.md`, `docs/DEPLOYMENT.md`,
  `docs/ONEPASSWORD.md`, `docs/PLAN.md`: provider-owned CLIs, sign-in and protocols.
- `docs/DELIVERABLES.md`: Mozilla's official PDF.js examples.
- `docs/NESTED-KVM-RESEARCH.md`, `docs/RUN-READINESS.md`: official AOSP images
  and Android documentation.
- `apps/android/gradle.properties`: Kotlin tooling's required `official` style name.
- Android's bundled `webrtc-150.7871.01-NOTICES.txt`: immutable upstream license
  notices, including the word “officials”.
- This file's complementary audit and exception explanations describe the audit.

No active Cairn identifier retains this former hosted-service name. Earlier ADRs
remain historical exceptions if either audit matches them. The chat desktop-light
capture has been regenerated with Cairn; its prior binary match is not retained.

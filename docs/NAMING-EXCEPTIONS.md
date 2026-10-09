# Naming exceptions after the Cairn / Beacon cutover

The audit command is `git grep -iE '\bleo\b|leo-|leo_|LEO_|dev\.leo'`.
Its remaining matches are intentional:

- `leo91000/leo-agent-manager`: the existing GitHub repository, which only Léo
  may rename. This includes the issue tracker (`AGENTS.md`, `docs/agents/`),
  historical source/PR/CI/release links in docs and browser fixtures, the Android
  release base, release/validation test repository fixtures, the disabled CLI
  timer repository setting, and the Android workflow's GitHub API endpoint.
  GHCR images use the new product names independently.
- `docs/adr/0001-*` through `docs/adr/0033-*`: immutable historical decisions,
  incident logs, commands, paths and measured evidence. Their original names are
  preserved so earlier release evidence remains intelligible. The current
  naming decision is ADR-0034.
- This file's audit expression and explicit old repository name describe the
  audit itself; they are not active runtime identifiers.

The GitHub owner `leo91000` remains unchanged. After Léo renames the repository,
active GitHub repository references can be updated in a separate focused change;
older evidence links and ADRs remain historical.

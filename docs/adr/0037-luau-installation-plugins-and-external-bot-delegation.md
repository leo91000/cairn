# Luau installation plugins and explicit external bot delegation

Date: 2026-10-12.

## Decision

Installation plugins use a versioned host contract and source-only Luau handlers
executed in dedicated worker processes. Rust retains credentials, permissions,
conversation execution and durable delivery. The initial packages are bundled:
1Password and a Discord mention bot. Package installation, marketplace distribution,
general OAuth and arbitrary native execution are deferred until these packages have
been dogfooded. Plugin API types, process host and Luau runtime are separate crates;
installation application adapters implement authorized host operations.

MCP and the run-local CLI are plugin interfaces. 1Password metadata stays on MCP;
secret values are available only through CLI stdout for direct script/pipe consumption.
The installation still runs `op` with encrypted service-account credentials and live
agent grants. This changes the secret-read interface, not its authorization.

The owner may delegate a configured Discord channel to explicit external user IDs
(listed up front or approved after they ask by mentioning the bot),
using a dedicated agent restricted to the selected project and without installation
management access. These identities need no Cairn account and are not installation
members. Grants are bound to the current owner access and the agent's recorded
permission policy; policy changes require review and resaving the connection.

This extends ADR-0028's rule that a Cairn account is the only way to access an
installation: access through Cairn's UI/API still requires that account, while a
plugin can provide narrowly delegated external ingress and replies. It does not
change ADR-0029's shared-conversation model or grant external identities installation
browsing. Bot outputs are deliberately published to the configured external channel.
The owner authorizes the agent's credentials and audience together.

ADR-0013 remains in force: Cairn packages do not enable native Codex plugins or
catalog acquisition. The CLI extends ADR-0035's private channel with a run-bound
plugin command route, including its cross-run checks and remote-node forwarding.

## Consequences

A public marketplace needs additional isolation review, explicit package/capability
installation grants and update/migration policy. Separate processes and Luau's
restricted standard library provide useful dogfood boundaries but do not alone
constitute a complete hostile-code isolation guarantee.

Bot services run at installation scope between agent turns. Durable inbox admission,
message deduplication, conversation routing and reply progress belong to Cairn.
Platform transport is currently implemented in the bundled adapter and can move
behind generic host networking capabilities without changing the conversation API.

See [plugin implementation and live-test procedure](../PLUGINS.md) for current
capabilities and delivery limits.

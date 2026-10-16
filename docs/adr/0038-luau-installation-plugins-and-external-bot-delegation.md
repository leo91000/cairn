# Luau installation plugins and explicit external bot delegation

Date: 2026-10-12.

Arbitrary native execution and the built-in-only 1Password capability are
superseded by [ADR-0044](0044-declarative-plugin-contributions.md).

## Decision

Installation plugins use a versioned host contract and source-only Luau packages
executed in dedicated, long-lived worker processes, one per connection. Packages
receive durable events and act only through declared capabilities (conversation
submission, connection storage, HTTPS to declared hosts with host-injected
credentials, timers, native transports). Rust retains credentials, permissions,
conversation execution, durable queues and platform transports such as Discord's
Gateway. Bundled packages (1Password, Discord) ship in the binary; owners can install
packages from disk or by upload, and each exact version runs only after the owner
approves its manifest and source. Marketplace distribution, general OAuth and
arbitrary native execution are deferred. Plugin API types, process host and Luau
runtime are separate crates; installation adapters implement the capabilities.

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

For public channels the owner may instead let anyone in the channel use the bot. That
delegation is only available with an agent that cannot change anything: a read-only
sandbox, at most one project, and no installation, GitHub, MCP or 1Password access,
rechecked before every effect. Each person has an hourly limit, and only listed people
or a conversation's starter can answer for it or stop it.

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
Platform transports stay native and shared (heartbeats, resumes and reconnects need
long-lived sockets); platform behaviour lives in Luau. A connection is one generic
model (delegation plus manifest-declared settings), so new integrations add a
package and, at most, a transport.

See [plugin implementation and live-test procedure](../PLUGINS.md) for current
capabilities and delivery limits.

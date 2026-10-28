> Statut : les capacités `inbound` (`cairn.inbound.accept`) et `conversations.submit`, les règles d’admission, la délégation et sa partie de la page d’un plugin (personnes, demandes, agent, limites) sont remplacées par [ADR-0075](0075-plugin-hooks-rules-and-scoped-grants.md) : un transport émet des hooks, auxquels réagissent des règles.

# Plugin transports and setup forms in Luau

Date: 2026-10-16.

## Decision

Cairn keeps no code specific to a plugin. What Discord needed natively becomes
generic host APIs, and the Discord package uses them like any other package would:

- **Sockets** (`socket` capability): a package's transport worker opens a WebSocket to
  a declared host (WSS, default port, `*.domain` wildcards allowed in `hosts`), sends
  text, schedules ticks and receives `socket` events (`start`, `open`, `message`,
  `tick`, `close`). The host keeps the socket, enforces sizes, answers pings, paces
  reconnections until the package reports a healthy connection, and replaces
  `{{secret}}` in sent text with the instance's credential or a secret field, which
  scripts never see. The transport worker is separate from the worker that handles
  queued events, so slow replies never delay heartbeats or acknowledgements.
- **Inbound messages** (`inbound` capability): `cairn.inbound.accept` hands Cairn what
  a person wrote, from a socket or from a timer polling an API. Admission rules are
  the host's: unknown people are dropped, approval mode records a request without
  content, IDs deduplicate, queues are bounded, and the host's `id` and `author` always
  win over the package's fields.
- **Instance status**: packages with their own connection report `connecting` or
  `connected` with a short message; `cairn.fail` in the transport worker stops the
  instance until the owner reviews it, with the package's message.
- **Instance forms** (`instanceForm`): a `form` handler completes the owner's form
  while they type: choices for `dynamic` fields, derived values (hidden settings
  included), errors and a guidance view. It runs with the draft's own credential and
  secrets through `http` and `exec` only, before anything is saved, and once more on
  save, where its errors refuse the save. Fields marked `refresh` call it again.

The Discord Gateway session (Identify and Resume, heartbeats and acknowledgement
checks, threads, scope, button acknowledgements), setup discovery and Discord's
permission resolution, the token test and the bot's panel move to
`plugins/discord/main.luau`. The Discord setup wizard and dashboard pages, their
routes, Android's Discord branches and the hardcoded brand marks are removed: every
plugin uses the generic plugin page, whose delegation part (people, requests, agent,
limits, usage, recent activity) now serves every package with `conversations.submit`.
The `transport.discord` capability is removed.

## Consequences

Admission still happens in the host, but message scope (which channel, which
mentions) is now the package's responsibility: a buggy package can admit messages it
should have ignored, only from people the owner allowed. Content of messages that are
not admitted reaches the transport worker's memory and nothing else.

A form handler may call the service on every refresh; packages should only ask again
for fields marked `refresh` and avoid network calls on `submit`.

Sockets are text only; binary frames are ignored. Gateway resume state still lives
in memory, so events missed while the installation is offline are not recovered.

Builds on ADR-0045, which deferred native transports and long-lived sockets for
scripts; the [Luau-plugin capability model](0038-luau-installation-plugins-and-external-bot-delegation.md), approval digests and owner-only management are
unchanged.

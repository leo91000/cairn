> Statut : Remplacée par [ADR-0046](0046-plugin-transports-and-forms-in-luau.md) pour les transports/formulaires, [ADR-0047](0047-multi-file-plugin-packages.md) pour les packages et [ADR-0049](0049-plugin-program-installers-and-toml-manifests.md) pour les installateurs de programmes ; les contributions déclaratives restent applicables.

> External delegation, kept unchanged here, is removed by [ADR-0075](0075-plugin-hooks-rules-and-scoped-grants.md).

# Declarative plugin contributions and declared programs

Date: 2026-10-16.

## Decision

Plugin API v2 lets a package ship a complete integration in `manifest.json` and
`main.luau`, with no Cairn change. The manifest declares what the package adds;
Cairn renders it on the web and Android, authorizes it and calls the script:

- **Instance settings** are a declarative form: text, multiline text, number,
  boolean, choice, multiple choice, URL and secret fields, with sections,
  descriptions, placeholders, defaults, length, range and pattern rules. Secret
  fields are encrypted in the vault, never returned to clients or scripts, and only
  injected by the host (HTTP headers, program environment).
- **Agent access** (`agentAccess`): the owner grants agents per instance. Nothing is
  granted by default, including Main agent. Grants are checked before every command
  or tool call and again after a program finishes.
- **Commands** run inside conversations as `cairn plugin <plugin> <command> …`. The
  manifest declares flags and positional arguments; the host parses and validates
  them. A command's output is `json`, `text`, or `secret`: secret output goes only
  to the command's stdout, without a trailing newline, and is never persisted.
- **Agent tools** are listed by the run's Cairn MCP server for granted agents and
  call the script with validated input. Tools cannot return secret output.
- **Agent instructions** are added to the prompt of runs whose agent is granted.
- **Views and actions**: the script returns a small component tree (sections,
  text, alerts, key/value facts, lists, badges, progress, code, links, buttons and
  forms) that the web and Android render natively. Buttons and forms call the
  script's `action` handler with the instance's capabilities.
- **Instance tests**: a `test` handler backs the Test action.
- **Brand**: a colour and an SVG path for the package's mark.
- **Script helpers**: `cairn.json` encodes and decodes JSON, and `cairn.fail(message)`
  stops a handler with a message meant for people; other script errors stay private.
- **Declared programs** (`exec` capability): the manifest lists absolute paths of
  programs the package may run on the installation. The owner approves them with the
  package. Cairn runs them without a shell, with an empty environment, a private
  temporary home, bounded arguments, input, output and time, and injects declared
  secrets as environment variables. Scripts never see those secret values.

The built-in 1Password plugin becomes an ordinary v2 package: service accounts are
instances with a secret token and agent grants, `cairn plugin onepassword …` and the
`onepassword` tool are declared commands and tools, and `op` is a declared program.
`onepassword.read` and
the built-in-only rule are removed; no Cairn code is specific to 1Password.

Long-lived workers start only for packages that need them (a transport,
conversations or timers). Commands, tools, views, actions and tests use one-shot
workers under the same deadline, memory and capability gate.

## Consequences

Declared programs run outside the Luau sandbox, with the installation's network and
file access. The review screen states each program path, and approval is bound to
the package digest like every other capability. Programs must be installed by the
owner; packages cannot download or write executables.

Native transports (Discord's Gateway) and the Discord setup and dashboard pages stay
in Cairn for now. Inbound webhooks, long-lived sockets for scripts and a plugin-level
page outside instances are not part of v2. (Sockets, inbound messages and package
forms were added by ADR-0046, which removed Discord's native code.)

Only API v2 manifests load; v1 packages must be updated (ADR-0047 also changed how
approvals are computed, so installed packages are reviewed again).
Instances that delegate nothing to external people are no longer bound to the owner
who created them; external delegation still is. The [Luau-plugin capability model](0038-luau-installation-plugins-and-external-bot-delegation.md), approval digests,
external delegation and owner-only management are unchanged; this ADR supersedes its
statement that arbitrary native execution is deferred, for declared programs only.

See [plugin implementation](../PLUGINS.md#contributions) for the manifest
reference.

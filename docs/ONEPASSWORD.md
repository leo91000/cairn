# 1Password service accounts

1Password is an ordinary plugin package (see [plugins](PLUGINS.md#1password)). In
**Atelier → Plugins → 1Password** on web or Android, add a service account with a
name and an `ops_…` token, then save and test it. Grant it to agents from each
agent's page, under **Secrets & plugins**. Every account starts with **no
authorized agents**, including Main agent; the instance lists them read-only.
Multiple accounts can have independent agent lists. Each account's panel lists the
vaults it can read on demand.

Editing with an empty token keeps the existing credential. Enter a new token to
rotate it. Disable the account, remove it from an agent, or delete the account to revoke
future reads, including in active conversations. A read already in flight is
checked again before its result is returned. Secrets previously retrieved by an
agent cannot be recalled. Grants are live and do not depend on a run snapshot.

The workspace `onepassword` tool supports `accounts`, `vaults`, `items` (with a
vault name or ID) and `fields` (with a vault and item, returns references without
values). Secrets are read only with `cairn plugin onepassword read --account ID
op://vault/item/field`, piped into the consuming command. Everything is read-only. Create the service account in 1Password with access to only the
intended vaults. The service account's permissions remain the upper bound of access.
Tool results containing secrets enter the authorized agent's context and may be
stored in run history; avoid asking
agents to print secrets in messages, logs or artifacts.

The package's instructions are added to isolated tasks and chats whose agent is
granted at least one enabled account. Agents discover their current grants with
`accounts` when credentials are needed; the CLI runs on the server, not as an
authenticated CLI in the task VM.

Passkeys cannot be retrieved or used through this integration. If a sign-in
requires one, agents must stop and ask the user to enable TOTP on that website
and save its configuration in the matching 1Password item, when supported.
They wait for confirmation before retrying, rather than changing authentication
settings or bypassing the passkey requirement. If TOTP is unavailable, they ask
how to proceed; unattended tasks report the required user action. Passwords,
TOTP seeds and recovery codes must not be requested in chat.

Tokens are secret instance fields, encrypted using the application's existing
AES-GCM credential vault. They are never returned by the management API, given to
the package's script, copied into a run/VM environment, or included in audit
records. Back up `mcp-encryption-key` together with the application database. The
manager runs the plugin’s locked official CLI as the package's declared program: an empty
environment with only the token and a disabled cache, a temporary home, the
package's fixed read-only arguments and a 30-second timeout. CLI error output is not
exposed. Accounts saved before plugin API v2 were moved into instances with the same
IDs, grants and tokens. The plugin installs its own CLI on first use, on Linux
x86_64 or aarch64, from the official 1Password HTTPS hosts. It selects a stable
version >=2.39.0 <3 and records its archive/executable hashes. The archive has no
published SHA-256 sidecar; this does not verify its OpenPGP signature. Docker and
local development no longer require a global `/usr/local/bin/op`. **Installed
programs** on the plugin page lets the owner check, install or restore a release
on web and Android. Restarting keeps the installed release.

The connection test checks that the saved token can list its vaults. Saving does
not require 1Password availability; a failed test does not erase the token or
change agent grants. Token validity, expiry, account limits and permissions are
reported by a connection/read failure without exposing provider output.

References: [Service accounts with the CLI](https://developer.1password.com/docs/service-accounts/use-with-1password-cli),
[CLI installation](https://developer.1password.com/docs/cli/get-started/).

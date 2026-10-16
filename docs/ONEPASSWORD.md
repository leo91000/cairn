# 1Password service accounts

In **Atelier → Plugins → 1Password** on web or Android, add a name and an `ops_…`
service account token. Select the agents allowed to use it, then save and test
the connection. Every account starts with **no authorized agents**, including
Main agent. Multiple accounts can have independent agent lists.

Editing with an empty token keeps the existing credential. Enter a new token to
rotate it. Disable the account, uncheck an agent, or delete the account to revoke
future reads, including in active conversations. A read already in flight is
checked again before its result is returned. Secrets previously retrieved by an
agent cannot be recalled. Grants are live and do not depend on a run snapshot.

The workspace `onepassword` tool supports `accounts`, `vaults`, `items` (with a
vault name or ID), `fields` (with a vault and item, returns references without
values), and `read` (with an `op://vault/item/field` reference). It offers
read access only. Create the service account in 1Password with access to only the
intended vaults. The service account's permissions remain the upper bound of access.
Tool results containing secrets enter the authorized agent's context and may be
stored in run history; avoid asking
agents to print secrets in messages, logs or artifacts.

Agent instructions advertise the integration automatically in isolated tasks and
chats. Agents discover their current grants with `accounts` when credentials are
needed; the CLI runs on the server, not as an authenticated CLI in the task VM.
The same guidance is included in the tool description.

Passkeys cannot be retrieved or used through this integration. If a sign-in
requires one, agents must stop and ask the user to enable TOTP on that website
and save its configuration in the matching 1Password item, when supported.
They wait for confirmation before retrying, rather than changing authentication
settings or bypassing the passkey requirement. If TOTP is unavailable, they ask
how to proceed; unattended tasks report the required user action. Passwords,
TOTP seeds and recovery codes must not be requested in chat.

Tokens are encrypted using the application's existing AES-GCM credential vault.
They are never returned by the management API, copied into a run/VM environment,
or included in audit records. Back up `mcp-encryption-key` together with the
application database. The manager invokes the pinned official CLI with an isolated
environment, a temporary home, disabled cache, fixed read-only arguments and a
30-second timeout. CLI error output is not exposed. Docker includes CLI 2.39.0;
local development needs the CLI at `/usr/local/bin/op`.

The connection test checks that the saved token can list its vaults. Saving does
not require 1Password availability; a failed test does not erase the token or
change agent grants. Token validity, expiry, account limits and permissions are
reported by a connection/read failure without exposing provider output.

References: [Service accounts with the CLI](https://developer.1password.com/docs/service-accounts/use-with-1password-cli),
[CLI installation](https://developer.1password.com/docs/cli/get-started/).

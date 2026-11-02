# Native coding-agent configuration

Open **Settings → Installation → Coding agent defaults** to configure Codex,
Claude Code or OpenCode for the installation. On an existing agent's **Coding
agent & model** page, **Native configuration** opens its customizations.
Only the installation owner edits these settings in the apps. Public MCP and
`cairn_installation` expose the same read, preview and save capabilities under
their existing live authorization and read/manage scopes.

The form and text editor share one native document:

| Coding agent | Native format |
| --- | --- |
| Codex | `config.toml` (TOML) |
| Claude Code | `settings.json` (JSON) |
| OpenCode | `opencode.jsonc` (JSON with comments and trailing commas) |

Switching modes keeps advanced options. Form changes update individual native
properties, retaining comments and unrelated formatting. Native properties without
a form control remain editable in text mode. Syntax and mapped types are checked
before saving; additional native options must be supported by the installed
engine. The form exposes common options and does not reproduce every native CLI
setting. Terminal-only preferences do not change Cairn's own interface.

For example, Codex accepts these native context preferences:

```toml
model_context_window = 1000000
model_auto_compact_token_limit = 900000
```

These declare a context window and compaction threshold; the selected model must
support the requested capacity. See the [Codex configuration reference](https://learn.chatgpt.com/docs/config-file/config-reference).

## Inheritance and saving

An agent's text editor shows its effective editable configuration, including
inherited values. Changing a native property personalizes that property; other
properties continue to follow installation defaults. **Use installation value**
removes a customization. It restores inheritance even if the current default
already equals the customized value. Both apps also offer resetting all
customizations before saving.

Model and supported reasoning, instruction and speed fields share their values
with the existing Cairn agent settings. The native Codex name for Standard speed
is `service_tier = "default"`. Generated execution instructions, permissions and
credentials are supplied separately from the editable document.

The agent editor and chat picker show the resolved inherited model, reasoning and
speed without copying them into overrides. Messages send only explicit choices;
continuing with the same engine retains the conversation selection until a
corresponding configuration default changes. Naming that same engine explicitly
does not reset the model. A queued message still names its chosen engine, so a
different engine selected by an earlier queued message cannot change its intent.

Each save checks the document, installation-default and agent revisions. A
concurrent change is refused rather than overwritten; the editor retains the
local draft. Changes apply to the next admitted message of existing conversations
and to new scheduled work. Each message freezes its settings for retries and
account switches. Choosing another coding agent for a quota-blocked request freezes
the selected engine's settings for that resumed request.

An active turn is never interrupted by a configuration save. A retained Codex
process restarts between messages when its native startup settings change, using
the same saved session and workspace. Unchanged preferences keep the ready process.

## Managed settings

This configuration cannot replace connected-account credentials, native control
transport, authorized MCP tools or Cairn resource grants. Use Connections and the
agent's access settings for those. Native configuration is stored on the
installation and transported in the frozen launch plan to native nodes and VMs;
it does not depend on paths or configuration inherited from the manager's host.
`CAIRN_CONFIG` remains the separate installation startup configuration.

Native Codex and OpenCode plugins remain disabled. OpenCode automatic compaction,
pruning, sharing and automatic updates remain disabled. Native Codex profiles that
could override managed execution settings are refused. Unsupported changes fail
before saving with an explanation; they are not accepted and silently replaced.
These boundaries preserve [ADR-0013](adr/0013-managed-tools-without-native-plugin-catalog.md),
[ADR-0042](adr/0042-opencode-api-key-integration.md) and the
[configuration decision](adr/0079-native-coding-agent-configuration.md).

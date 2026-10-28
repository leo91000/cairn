# Private Slack app bot OAuth: protocol and host compatibility

Checked **2026-10-28**, using official Slack documentation and repository source.
This supplements [Q1–Q20](SLACK-PLUGIN-SPEC.md) and the broader
[token lifecycle research](SLACK-TOKEN-LIFECYCLE.md). It does not narrow the accepted
scope or establish implementation, live qualification, merge or deployment approval.
No account, installation, credential or real Slack API was accessed; no builds,
tests, formatting or generation were run.

## Manifest and consent

The generated app manifest can declare `oauth_config.redirect_urls` and separate
`oauth_config.scopes.bot` / `.user` arrays. Limits are 1,000 redirects and 255
scopes per bot/user array. Optional scopes must also appear in their corresponding
regular array. Bot/user rotation is `settings.token_rotation_enabled`;
`oauth_config.token_management_enabled` is a distinct setting and must not be
substituted. [Manifest reference](https://docs.slack.dev/reference/app-manifest/#oauth),
[settings](https://docs.slack.dev/reference/app-manifest/#settings)

The existing four-scope manifest has documented token support:

| Scope | Supported modern token kinds | Purpose in this slice |
| --- | --- | --- |
| [`app_mentions:read`](https://docs.slack.dev/reference/scopes/app_mentions.read/) | Bot | Mentions visible to the app |
| [`chat:write`](https://docs.slack.dev/reference/scopes/chat.write/) | Bot, user | Replies as the app |
| [`channels:read`](https://docs.slack.dev/reference/scopes/channels.read/) | Bot, user | Public-channel information |
| [`users:read`](https://docs.slack.dev/reference/scopes/users.read/) | Bot, user | People and bot identity lookup |

Keep these in the bot array for bot consent. Socket Mode's
[`connections:write`](https://docs.slack.dev/reference/scopes/connections.write/)
belongs to an app-level token, outside that bot/user consent.

Authorize at `https://slack.com/oauth/v2/authorize`, using the **client ID** and
comma-separated `scope` for bot rights, `user_scope` for personal rights. HTTPS
redirects cannot contain fragments; a supplied `redirect_uri` must match or descend
from a registered URL and be identical in the access exchange. Reinstallation
adds scopes; requesting fewer does not downgrade an existing token. `team` is only
a target hint. An undistributed app without org-wide support installs only in its
owning workspace; an eligible private app created inside an Enterprise org can
install across workspaces in that org. Do not promise unrelated-workspace private
distribution. [OAuth installation guide](https://docs.slack.dev/authentication/installing-with-oauth/)

**Package implication:** emit the actual host-supplied OAuth callback alongside
the scopes. Current setup already receives `urls.oauthCallback`, but emits no
redirect and requests pasted bot credentials. Client ID and app ID need separate
settings. [Current setup](../plugins/slack/setup.luau),
[form URL contract](PLUGINS.md#setup-drafts),
[URL construction](../crates/installation/src/plugins/forms.rs)

## Access exchange and identity

Use `POST https://slack.com/api/oauth.v2.access`, preferably HTTP Basic client
authentication; Slack explicitly recommends it over client credentials in request
parameters. Form encoding is supported. Send `code`, the same `redirect_uri`, and
`grant_type=authorization_code`. Renewal uses `grant_type=refresh_token` plus
`refresh_token`, still authenticated as the client. Check JSON `ok`, including
HTTP-success responses. [Method reference](https://docs.slack.dev/reference/methods/oauth.v2.access/)

The documented combined response separates root bot fields (`access_token`,
`token_type: bot`, `scope`, `bot_user_id`, `app_id`) from `authed_user.id` and
optional nested personal credential fields. Rotation adds `refresh_token` and
`expires_in` to each returned credential. `bot_user_id` is the bot's user identity,
not the installer. An installer identity is not a reason to request or retain a
personal token in this bot slice. [Access response examples](https://docs.slack.dev/reference/methods/oauth.v2.access/#response)

Workspace identity is `team.id`; org installation can return `team: null`,
`enterprise.id` and `is_enterprise_install: true`. Merely seeing `enterprise.id`
does not prove an org installation: workspaces inside an org also have enterprise
identity. Org installation does not immediately add the app to any workspace;
workspace access remains separately assigned. Methods needing workspace context
must receive it explicitly. User IDs can begin with `U` or `W`.
[Enterprise development guide](https://docs.slack.dev/enterprise/developing-for-enterprise-orgs/)

`auth.test` needs no additional scope. Its bot example returns `team_id`,
`user_id`, `bot_id`; Enterprise workspace responses additionally expose
`enterprise_id`. It does not document an `app_id` response. [Authentication test](https://docs.slack.dev/reference/methods/auth.test/)

`bots.info` needs `users:read`; pass the **B-prefixed bot ID**, not the bot's user
ID. Its returned `bot.app_id` identifies app ownership; `bot.user_id` is a
different identity. With an org token, `team_id` is required; the reference allows
a team or enterprise ID, but does not establish every org-bot lookup behavior.
[Bot information](https://docs.slack.dev/reference/methods/bots.info/)

**Binding inference:** compare provider `app_id` with configured app ID, and
provider workspace/org identity with the selected resource. Client ID authenticates
the app's OAuth client; it is not the `A…` app identifier returned by Slack. The
references expose both independently and document no conversion. Keep the current
`auth.test` → `bots.info` ownership check when credentials are manually supplied;
test org verification separately rather than inventing a workspace.
[Access reference](https://docs.slack.dev/reference/methods/oauth.v2.access/),
[current verification](../plugins/slack/setup.luau)

## Rotation, revocation and uncertain results

Without rotation, access tokens do not expire; rotation makes them expire after
12 hours and cannot subsequently be disabled. Refresh tokens are single-use;
persist the replacement pair. Slack mentions a short refresh grace period without
a duration, while adjacent text says redemption stops the previous refresh token
working immediately. Neither is a documented replay/idempotency contract. Refresh
does not immediately invalidate the previous access token; Slack limits active
tokens to two and revokes older excess values. For rotating installations,
`auth.revoke` revokes one token without uninstalling the app or stopping events.
[Rotation guide](https://docs.slack.dev/authentication/using-token-rotation/)

Migration of a long-lived bot token uses the separate `oauth.v2.exchange` method,
with client credentials and the old token; it is not authorization-code exchange.
[Exchange reference](https://docs.slack.dev/reference/methods/oauth.v2.exchange/)

Provider revocation must inspect `revoked`; test mode does not revoke. The generic
method's bot-deactivation/channel-membership description should not override the
rotation-specific contract. Removing an installation through `apps.uninstall`
requires app client credentials plus a token, and has granular-bot permission
constraints. Local Cairn disconnect must remain distinguishable from both actions.
[Revocation reference](https://docs.slack.dev/reference/methods/auth.revoke/),
[uninstall reference](https://docs.slack.dev/reference/methods/apps.uninstall/),
[local disconnect contract](PLUGINS.md#host-managed-resource-oauth)

**Recovery inference:** consume code/refresh intent durably before I/O. A timeout,
interrupted body or failure to save a successful response cannot prove Slack did
nothing. Require reconnection after uncertain exchange; do not replay a consumed
value or restore it from local state. This matches the current host's code claim,
refresh claim and authority fences. [Host implementation](../crates/installation/src/plugins/oauth.rs)

## Concrete package work versus host limitations

These are source observations and implementation constraints, not new approved
API designs. [Provider declaration](../crates/plugin-api/src/oauth.rs),
[exchange/capture/renewal](../crates/installation/src/plugins/oauth.rs)

| Area | Compatibility constraint |
| --- | --- |
| Package wiring | Current Slack manifest declares no OAuth provider. Add instance client settings, bot provider, comma scope separator, callback generation and host-managed HTTP injection; retain manual verification and current authority boundaries. [Manifest](../plugins/slack/manifest.toml), [API helper](../plugins/slack/lib/api.luau) |
| Already supported | Confidential Basic, `scopeParameter`, comma separator, root bot token mappings, nested personal mappings, separate refresh mappings, encrypted capture, inert drafts and revision checks exist. No Slack-specific native exchange is needed. |
| Capture limitation, with a package route | Metadata permits string leaves named only `id`/`name`; `/team/id` and `/enterprise/id` work, but `/app_id`, `/bot_user_id`, boolean `/is_enterprise_install` and `token_type` validation cannot be declared. Missing/null metadata is skipped, subject to at least one identity being returned. This does **not** prevent package verification: saved-resource forms can inject `{configuration = "self", oauth = "bot"}` and derive hidden `appId`/`botId` through the existing identity calls. [Form authentication](PLUGINS.md#resource-configuration-forms) |
| Replacement gate | OAuth finish stores credentials without running the resource form. Existing app/bot metadata can survive re-consent in the same workspace. A package-only route must verify the actual current managed credential before operational use, including replacements; cached fields or OAuth `authorized` status alone are insufficient. A native extension is not established as necessary for the ordinary workspace slice. |
| Rotation limitation | Generic renewal retains the previous refresh token when a successful response omits a replacement. Valid Slack rotation responses supply one, but malformed-response rejection cannot currently require it declaratively. Treat stricter provider validation as a concrete robustness question, not proof that basic OAuth needs new foundations. |
| Broader lifecycle work | Declarations lack opaque migration/revocation operations and multi-credential capture. Combined bot/user capture cannot create two managed families through one mapping; separate personal consent remains available. These full-lifecycle requirements must not be mistaken for prerequisites to bot-only authorization-code exchange. |

Fixture acceptance should cover wrong app/token class/target, null-team orgs,
independent bot/person credentials, missing replacement refresh, uncertain I/O and
late revoked-authority responses. These are proposed checks. Establish org proof
with a concrete fixture/protocol scenario before choosing extra host fields.
Actual private-app
installation, org lookup behavior and rotation remain unverified against Slack;
this research authorizes none of those external actions.

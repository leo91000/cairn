# Slack token lifecycle: protocol findings and remaining host work

Research checked on **2026-10-19**. This is a focused supplement to the accepted
[Q1–Q20 design](SLACK-PLUGIN-DESIGN.md), [specification](SLACK-PLUGIN-SPEC.md),
[personal authorization ADR](adr/0050-personal-plugin-authorizations.md) and
[implementation log](SLACK-PLUGIN-IMPLEMENTATION.md), not an implementation
checkpoint or a claim of live Slack compatibility. No credentials, workspace,
provider API calls or app settings were used.

Repository baseline: `07572fc0ee760982aa5fd5dd49d2d0eedcf66b4a`; the host observations
below describe the inspected working tree. First-party SDK comparison is
`@slack/oauth` **4.0.0**, source commit
[`70e15b8442745d215f9ae259a2c970b306aff51a`][sdk-package], already pinned by the
specification. Slack documentation is mutable; the links below were accessed on
the research date. In particular, Slack's app-level rotation announcement is
dated [2026-10-08][app-rotation-announcement].

## Documented token identities

| Credential | Authority and appropriate use |
| --- | --- |
| Bot token (`xoxb-`, rotating `xoxe.xoxb-`) | An app's installed bot, independently of the installer. Ordinary collaboration tools use this authority. |
| User token (`xoxp-`, rotating `xoxe.xoxp-`) | The authorizing member's visible resources and personal actions. It must follow the actual requester. |
| App-level token (`xapp-`, rotating `xoxe.xapp-`) | App-wide APIs across installations, including Socket Mode bootstrap; it is not a workspace bot or personal authorization. |
| App configuration token | A developer user's workspace-level authority to manage app manifests, spanning apps that user manages. It is not the app's operational token. |
| Workflow token (`xwfp-`) | Temporary function authority; expires at completion/error or after 15 minutes, whichever comes first; cannot be refreshed. |
| Service token | Slack CLI/Deno-app tooling authority; long-lived and nonrotatable. It is not a substitute for personal OAuth in Cairn. |

These distinctions and the nonrotating token prefixes are documented in
[Tokens][tokens]; rotating bot/user examples are in [`oauth.v2.access`][access].
An `xoxe` prefix alone cannot distinguish a configuration credential from an
operational credential: [`tooling.tokens.rotate`][tooling-rotate] returns a
configuration access value shaped like `xoxe.xoxp-...`.

App-level rotation has a separate app setting,
`settings.app_level_token_rotation_enabled`. It requires a granular app, excludes
workflow apps, and cannot be switched off after enabling. Earlier static tokens
keep their behavior; newly created ones rotate. Both values are shown only at
creation; losing them requires creating another token. These are documented
app-wide settings, not workspace-specific switches.
[Slack's app-level announcement][app-rotation-announcement]

The corresponding bot/user manifest setting is
`settings.token_rotation_enabled`.
[App manifest reference][manifest-reference]

## Refresh, migration and concurrency

Bot/user token refresh uses `oauth.v2.access` with `grant_type=refresh_token` and
client authentication. Initial installation can return independent bot credentials
at the response root and user credentials under `authed_user`; each has its own
refresh value and expiry. Refresh responses use root token fields. Persist the
returned pair together, rather than retaining the original refresh value.
[`oauth.v2.access`][access], [pinned SDK refresh handling][sdk-refresh]

The rotation guide documents these constraints:

- Access lifetime: 12 hours. Bot/user refresh is single-use with unspecified short
  grace. Existing access tokens overlap; a two-active-token limit revokes the
  oldest excess token.
- App-level refresh uses `oauth.v2.access` with client credentials and root
  `token_type: app_level`. Its section describes immediate refresh-token
  consumption; no app-level grace can be assumed.
- Enable rotation before migration; it cannot be disabled. Exchange each
  long-lived credential once. Slack expires the original after the first refresh;
  manually revoking it can force reinstallation.

[Token rotation guide][rotation]

`oauth.v2.exchange` requires the issuing app's client ID and secret plus the old
token. Its argument reference names `xoxb`/`xoxp`; its error list includes
`authorization_not_found`. This is migration of a valid existing authorization,
not authorization-code exchange or a generic refresh operation.
[`oauth.v2.exchange`][exchange]

**Documentation uncertainty:** the rotation guide also names app-level exchange,
while the method argument reference still names only bot/user tokens. Existing
static app-level tokens explicitly remain valid after opt-in. An automatic
`xapp-` migration should remain unverified until Slack confirms the exact exchange
contract; accepting newly created rotating app-level pairs does not depend on it.
[Rotation guide][rotation], [exchange arguments][exchange], [Tokens][tokens]

**Host design inference:** serialize by credential family, persist a durable
consumption claim before sending a single-use value, and transactionally replace
the entire pair under the same revision fence. A lost response has an uncertain
provider effect. Neither a documented grace duration nor idempotent concurrent
refresh/replay is available as a recovery contract. Reconnect rather than guessing
which branch of an uncertain refresh won. Independent bot, person and app-level
families may refresh independently. This is a proposed application policy, not a
Slack guarantee of atomic storage or crash recovery.

The pinned SDK refreshes bot/user credentials when their remaining lifetime is
at most **7,200 seconds**, launches those independent refreshes concurrently, then
stores the updated installation. Its `authorize` path is not evidence of a
cross-process consumption lock or crash-safe compare-and-swap. These are SDK
implementation choices, not Slack protocol timings or host requirements.
[SDK expiry threshold][sdk-threshold], [SDK refresh/store path][sdk-refresh]

Configuration-token renewal is a different protocol:
`tooling.tokens.rotate(refresh_token=...)`, returning `token`, `refresh_token`,
`iat`, `exp`, `team_id` and `user_id`. The configuration access token lasts 12
hours; the method is marked beta. Do not send these pairs to an operational
`oauth.v2.access` renewal path.
[`tooling.tokens.rotate`][tooling-rotate]

Do not silently enable PKCE while setting up this server-hosted private app.
Slack marks that public-client transition as one-way without support; PKCE-enabled
apps' refresh tokens expire after 30 days. Desktop redirects cannot request bot
scopes, custom-scheme redirects always receive rotating tokens, and public-client
refresh does not require a client secret. These are distinct from the confidential
server flow above. [Using PKCE][pkce]

The documented **24-hour** grace applies to a regenerated OAuth **client secret**,
unless manually revoked. It supplies no refresh-token or signing-secret grace
duration. [Regenerating client secrets][client-secret]

## Disconnect, provider revocation and installation removal

| Operation or signal | Documented meaning and resulting boundary |
| --- | --- |
| Cairn local disconnect | Removes host authority and fences in-flight work; it currently makes no provider revocation call. Its label must remain explicit. [Current host contract][host-doc] |
| `auth.revoke` | The method returns a `revoked` boolean and has a test mode that does not revoke. Org-wide removal requires an org admin. Do not turn a successful HTTP response alone into a successful revocation claim. [Method reference][revoke] |
| Revocation with rotation | Revokes one token, including a refresh or expiring access token, without removing the underlying installation; events and UI token generation can continue. Revoking one current value is not proof that every overlapping access token or authorization is gone. [Rotation guide][rotation] |
| `apps.uninstall` | Removes one app installation and revokes all its associated tokens; requires a token plus app client credentials. The method lists `client_id_token_mismatch` and a granular-bot `no_permission` case. Do not promise bot-token-only uninstall. [Method reference][uninstall] |
| `tokens_revoked` | Subscription event with `oauth` and `bot` arrays of **user IDs**, not secret token strings. Use its app/workspace envelope to select the installation, then the affected subjects. It does not document app-level credential values or a refresh-generation identifier. [Event reference][tokens-revoked] |
| `app_uninstalled` | Complete uninstall event. It and token-revocation events can arrive in either order; the event identifies app and workspace. Handle both idempotently. [Event reference][app-uninstalled] |
| `team_access_revoked` | An organization-ready app lost access to specified workspaces. Disable those workspace routes/resources without assuming the whole org authorization was revoked. [Event reference][team-access-revoked] |

**Implementation inference:** provider disconnect needs a local-first authority
fence plus a bounded host-only revocation operation with honest states such as
confirmed, failed and uncertain. It must account for all locally known refresh
and overlapping access credentials before claiming complete token removal.
Keep authorization removal separate from uninstall; an app-level credential shared
by several workspaces cannot be erased merely because one workspace is removed.
An authenticated lifecycle event must also invalidate pending exchanges and held
operations, not just remove a status badge. No assumption about event delivery
alone should revive a locally disconnected authorization.

**Documentation caution:** the generic `auth.revoke` and installation guides
describe older bot-removal side effects differently. Use the rotation-specific
single-token contract above for rotating credentials; verify the chosen
nonrotating removal journey before claiming exact channel-membership effects.
[Method reference][revoke], [OAuth guide][oauth-guide]

## Requester and installation identity

For the standard installation flow, request bot rights using `scope` and personal
rights using `user_scope`. Its user proof is `authed_user.id`, not `bot_user_id`.
Scopes accumulate on reauthorization and cannot be downgraded merely by requesting
a smaller set. Cairn grants must therefore remain narrower than provider token
rights. [OAuth guide][oauth-guide]

The separate `v2_user/authorize` / `oauth.v2.user.access` flow is user-only and has
a different documented response shape: root `access_token` with `authed_user.id`.
It must not reuse a nested `authed_user.access_token` mapping. Its method page is
not sufficient evidence for assuming identical refresh behavior; the existing
standard flow already supports personal authorization.
[`oauth.v2.user.access`][user-access], [OAuth guide][oauth-guide]

Org installation can return `team: null`, `enterprise.id` and
`is_enterprise_install: true`. Org tokens still need workspace context for methods
that require `team_id`; workspace access is independently assigned. Enterprise
users can have `U` or `W` IDs. Slack Connect/source-workspace fields must not
silently replace the authorization's namespace.
[Enterprise app guide][enterprise]

**Required Cairn interpretation:** bind personal authority to the proved subject,
the configured app and workspace/org resource, and the actual admitted requester.
A matching display name, email, bot identity or channel participant is insufficient.
Owner intervention and later turns do not transfer another person's authorization.
External people need the exact provider proof for their admitted route, without
acquiring Cairn membership. These are already accepted requirements, not new
product decisions. [ADR-0050](adr/0050-personal-plugin-authorizations.md),
[current host contract][host-doc]

## Existing host support and mandatory gaps

The current implementation already has confidential Basic authentication,
configurable `user_scope`/comma separation, nested initial token mappings and
separate root refresh mappings. It encrypts tokens, binds consent to requester,
approval and resource revisions, serializes lookup/refresh per authorization,
claims refresh consumption durably and refuses interrupted renewal. Near-expiry
renewal currently starts within **60 seconds**, which is Cairn policy. Personal
proof and resource matching are rechecked around I/O.
[Manifest types][host-types], [exchange/capture/finish/credential][host-oauth]

| Required generic host work | Evidence and scope |
| --- | --- |
| Owner-only seed/capture of a managed token pair | Existing OAuth starts with an authorization-code grant; static secret fields do not initialize a refresh family. Rotating app-level setup needs access, refresh and expiry captured without Luau seeing them, with draft/activation provenance and the same fences. [Host types][host-types], [consent and credential implementation][host-oauth] |
| Opaque migration and provider revocation | No declared exchange/revocation lifecycle operation exists in the OAuth manifest. Ordinary HTTP can inject the selected access token in a header, but form values come from Lua and response bodies return to Lua. This is insufficient for secret-valued migration/refresh-token forms and secure response capture. A bearer-authenticated `auth.revoke` call is technically possible; that alone does not implement the lifecycle. [Host types][host-types], [HTTP handling][host-http] |
| Service-event invalidation by verified scope/subject | `oauth.disconnect` addresses the current admitted personal requester; native account deletion addresses that account. Slack revocation events need a bounded generic mechanism to invalidate the selected resource/provider/subject or installation, including pending and in-flight authority, without impersonating each person. [External OAuth dispatcher][host-external], [native revoke][host-oauth] |
| Sufficient provider-proved identity and token class | Metadata accepts only string paths ending in `id`/`name`; it can capture optional `/team/id` and `/enterprise/id`, but not root `/app_id`, `/bot_user_id` or boolean `/is_enterprise_install`. Token capture does not enforce `token_type`. Add an approved, bounded way to validate these facts, or prove an equally strong binding using existing fields. Preserve distinct null-team org resources. [Manifest validator][host-types], [capture/finish][host-oauth] |
| Reconciliation status and real client journeys | Existing UI correctly distinguishes local removal from provider revocation. Add actionable app-level replacement, migration, revocation failure/uncertainty and workspace-removal diagnostics on web and Android when the host operations exist. Their absence remains delivery work. [Implementation log](SLACK-PLUGIN-IMPLEMENTATION.md) |

These gaps are lifecycle responsibilities, not a proposal to move Slack REST
business logic into native code. Exact API names, persistence compatibility and
which known token generations to retain need implementation design and review.

## Proposed protocol fixture and live-validation checklist

The following are acceptance proposals derived from the accepted contract and the
findings above, not claims that tests or live behavior have passed:

- Exercise bot-only, user-only, combined and rotating installation responses;
  independent credential families; optional scope omission; wrong token class;
  provider `ok:false`, non-2xx, malformed, oversized and interrupted responses.
- Use real host capture/injection with fake Slack HTTP: pair replacement,
  concurrent calls to one family, independent families, crash after claim,
  provider success before storage failure, missing next refresh and changed
  identity. Never replay an uncertain single-use value or print fixture secrets.
- Cover creation-time app-level pair import, restart, expiry and replacement;
  reject configuration/workflow/service credentials in operational slots.
  Exercise Socket Mode bootstrap through managed injection after renewal.
- Hold migration, refresh and revoke responses while disconnecting, deleting,
  changing configuration, reapproving a package or activating/discarding a draft.
  Late responses must not restore authority. Track provider failure separately
  from successful local removal.
- Deliver lifecycle events repeatedly and in both uninstall/revocation orders;
  use multiple apps, workspaces, people and an org resource with no fixed team.
  Revoke Alice without changing Bob; remove one org workspace without removing
  its siblings. Preserve a new authorization when processing stale signals, or
  explicitly require revalidation where the signal cannot distinguish generations.
- Reproduce Alice/Bob consent swaps, a different Slack subject/resource, stale
  membership, external route changes and owner intervention. Exercise both nested
  and root user-token mappings without pooling personal authority.
- On an explicitly authorized development Slack app, separately verify actual
  rotating app-level creation/refresh, bot/user migration, disconnect effects,
  workspace removal and the two transports. Confirm ambiguous app-level migration
  and old/new revocation-event behavior with Slack rather than encoding guesses.
  Browser/Android fixtures do not replace this live evidence.

No builds, tests, live workspace checks or implementation changes were performed
for this research note.

[tokens]: https://docs.slack.dev/authentication/tokens/
[rotation]: https://docs.slack.dev/authentication/using-token-rotation/
[app-rotation-announcement]: https://slack.dev/app-level-token-rotation-in-slack/
[manifest-reference]: https://docs.slack.dev/reference/app-manifest/
[access]: https://docs.slack.dev/reference/methods/oauth.v2.access/
[exchange]: https://docs.slack.dev/reference/methods/oauth.v2.exchange/
[tooling-rotate]: https://docs.slack.dev/reference/methods/tooling.tokens.rotate/
[pkce]: https://docs.slack.dev/authentication/using-pkce/
[client-secret]: https://docs.slack.dev/authentication/verifying-requests-from-slack/#regenerating-client-secrets
[revoke]: https://docs.slack.dev/reference/methods/auth.revoke/
[uninstall]: https://docs.slack.dev/reference/methods/apps.uninstall/
[tokens-revoked]: https://docs.slack.dev/reference/events/tokens_revoked/
[app-uninstalled]: https://docs.slack.dev/reference/events/app_uninstalled/
[team-access-revoked]: https://docs.slack.dev/reference/events/team_access_revoked/
[oauth-guide]: https://docs.slack.dev/authentication/installing-with-oauth/
[user-access]: https://docs.slack.dev/reference/methods/oauth.v2.user.access/
[enterprise]: https://docs.slack.dev/enterprise/developing-for-enterprise-orgs/
[sdk-package]: https://github.com/slackapi/node-slack-sdk/blob/70e15b8442745d215f9ae259a2c970b306aff51a/packages/oauth/package.json
[sdk-refresh]: https://github.com/slackapi/node-slack-sdk/blob/70e15b8442745d215f9ae259a2c970b306aff51a/packages/oauth/src/install-provider.ts#L228-L320
[sdk-threshold]: https://github.com/slackapi/node-slack-sdk/blob/70e15b8442745d215f9ae259a2c970b306aff51a/packages/oauth/src/install-provider.ts#L862-L892
[host-doc]: PLUGINS.md#host-managed-resource-oauth
[host-types]: ../crates/plugin-api/src/oauth.rs
[host-oauth]: ../crates/installation/src/plugins/oauth.rs
[host-http]: ../crates/installation/src/plugins/capabilities.rs
[host-external]: ../crates/installation/src/plugins/oauth/external.rs

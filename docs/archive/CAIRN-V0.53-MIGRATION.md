# Historical migration to Cairn and Beacon (v0.53)

The initial Cairn/Beacon release was
[v0.53.0, published on 2026-10-10](https://github.com/leo91000/cairn/releases/tag/v0.53.0).
This archive records the first-launch plan and its authorization boundaries.
It is not a checklist to repeat for later application releases. Use the
[current release runbook](../PRODUCTION-CAIRN.md) and
[configuration reference](../PRODUCTION-CAIRN-SETUP.md) for ongoing operations.

## Initial qualification and authorization

The original preparation runbook required Léo's approval after the
[direct/relais qualification (#105)](https://github.com/leo91000/cairn/issues/105)
and review of the production preparation work in #110 and
[#113](https://github.com/leo91000/cairn/issues/113). Implementing those tickets
did not itself authorize tags, deployment, DNS or Coolify changes.

The v0.53.0 deployment and its manual approval are recorded in #105. Remaining
qualification work in that issue keeps its own scope; it does not turn the whole
initial cutover into a prerequisite for every later patch. Changes to a network
path or provider still need the relevant configuration checks.

## Fresh installation and retirement of the old public manager

The initial plan provisioned a fresh Beacon service and a separately claimed
installation. It provided no automatic migration of old conversations, local
accounts or volumes. This was the access-model change described in
[ADR-0028](../adr/0028-autonomous-installations-behind-the-official-service.md).

The cutover plan called for recording the old Coolify service UUID, immutable
image and exact volume/bind-mount names, then replacing GitHub's production
target with the new Beacon service UUID. The old public manager route and its
standalone CLI-update timers were to be retired. Installations instead follow
Beacon's approved image through `cairn-installation-update.timer`.

Deletion of the old service's data, agent-home, workspaces, runner-state and S3
volumes was an explicitly destructive, separately authorized operation, with a
private encrypted backup if recovery might be needed. These historical
instructions authorize no deletion or reinstallation of a current installation.
Beacon's Postgres volume remains separate from installation data and from
Coolify/Traefik volumes.

## First-launch configuration

The cutover established:

- `cairn.build` DNS/TLS and the Beacon Coolify service, with one Beacon process,
  a separate Postgres database and an exact trusted proxy address.
- Resend email sign-in, GitHub/Google OAuth registrations, the privacy policy,
  Web Push and Android/Firebase configuration.
- GitHub's `production` environment targeting Beacon and requiring Léo's approval.
- Initial claim, installation connectivity and direct/relais qualification.

The maintained configuration and verification commands now live in the
[configuration reference](../PRODUCTION-CAIRN-SETUP.md). An ordinary release uses
the existing DNS, provider registrations, service UUID and installation identity.

## Naming cutover (historical checklist)

This checklist was recorded before the first approved release. Its dates,
provider publication status and remaining actions describe that historical
snapshot, not current release prerequisites. The naming decision is recorded in
[ADR-0034](../adr/0034-cairn-and-beacon-before-release.md); current configuration
belongs in the [setup reference](../PRODUCTION-CAIRN-SETUP.md).

The original first-launch checklist was:

- GitHub `production`: configure `CAIRN_BEACON_ORIGIN=https://cairn.build`,
  `COOLIFY_URL`, the new Beacon `COOLIFY_SERVICE_UUID`, and private `COOLIFY_TOKEN`;
  keep environment approval required. Replace the former product-prefixed variable
  names rather than providing aliases.
- Coolify: use the production Compose above and configure its `CAIRN_BEACON_*`,
  `CAIRN_INSTALLATION_IMAGE` and `CAIRN_PROXY_*` values. The paired images are
  `ghcr.io/leo91000/cairn` and `ghcr.io/leo91000/cairn-beacon`; approve only validated
  digests from the same Git tree. Replace the old service and timers as described
  in the [retirement plan](#fresh-installation-and-retirement-of-the-old-public-manager); no production configuration is changed by this PR.
- GHCR: verify that both new packages `ghcr.io/leo91000/cairn` and
  `ghcr.io/leo91000/cairn-beacon` have **public** visibility, then check an
  anonymous pull of each validated digest. Installations pull without registry
  credentials (`deploy/installations/host.py`); a private package prevents setup.
- Resend: verify `cairn.build`, authorize the new sender `Cairn <cairn@cairn.build>`
  and put its sending key in `CAIRN_BEACON_EMAIL_KEY`. The email wordmark is served
  by Beacon at `/brand/cairn-wordmark.png`.
- OAuth: done on 2026-10-09, except the secrets. GitHub OAuth App **Cairn**
  (client ID `Ov23li402ZCgzExlWmny`) and Google project `cairn-e2bea` with the web
  client `514056782905-samtv7l6kskpgphh0vt84cne52obnprg.apps.googleusercontent.com`
  (origin and callback above) plus an Android client for `build.cairn.app` use the
  values in the [provider configuration](../PRODUCTION-CAIRN-SETUP.md#email-oauth-and-push). At cutover, generate each client secret and set it only in
  Coolify. The Google consent screen stays in **Testing** (test users only) until
  Cairn publishes a privacy policy, which Google requires before publishing.
- Android/Firebase: done on 2026-10-09. Firebase project `cairn-e2bea` has the
  Android app `build.cairn.app`; the four `CAIRN_ANDROID_FIREBASE_*` repository
  variables and repository `CAIRN_BEACON_ORIGIN=https://cairn.build` are set.
  Releases keep signing with the existing release key (a single-key keystore, so
  no alias is configured). Its certificate fingerprints are
  SHA-256 `71:41:88:30:35:FC:1E:00:FA:DD:81:7F:17:D9:0A:37:3B:0D:20:B5:30:D9:D3:B7:EE:36:FA:3A:C4:CA:6C:E4`
  (for `CAIRN_BEACON_ANDROID_CERTIFICATES`) and
  SHA-1 `5D:C0:DF:50:55:72:24:D0:DE:36:1A:38:81:2A:62:9F:37:72:E1:BF` (registered on
  the Google Android OAuth client). Reinstall the app: the package changed. At
  cutover, create the FCM service account key for `cairn-e2bea` and set it as
  `CAIRN_BEACON_FCM_SERVICE_ACCOUNT_JSON` in Coolify only.
- GitHub repository: done on 2026-10-09 with Léo's approval. The repository is
  now `leo91000/cairn` and references were updated; GitHub redirects old links.
  The image paths already use Cairn and remain independent of the repository
  name.

No DNS, provider setting, repository rename, release or deployment is performed
by implementing #122/#132.


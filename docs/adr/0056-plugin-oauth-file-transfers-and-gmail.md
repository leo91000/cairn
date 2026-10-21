# Generic plugin OAuth, private transfers and Gmail

Date: 2026-10-21.

## Context

The device grant in the [GitHub plugin decision](0053-github-as-a-bundled-plugin.md)
does not cover Gmail mailbox scopes. Browser automation is also unsuitable for
unattended mailbox access. Passing raw messages or attachments through Luau's
1 MiB protocol and 16 MiB memory limit cannot support ordinary mail attachments.

## Decision

Gmail is a bundled Luau package, with multiple independently granted instances.
The installation gains provider-independent mechanisms only:

- Manifest-declared authorization-code OAuth with host-generated state and PKCE,
  owner-bound callbacks, optional offline access, serialized refresh and provider
  revocation. The encrypted credential record extends the existing `{token}`
  envelope with OAuth metadata. Tokens never enter a package or client.
- An approved `files` capability for private, expiring, instance-bound temporary
  handles. Host HTTP can upload/download them outside the worker. The run-local
  CLI transfers VM files through the same current run/account grants, without
  providing filesystem paths to packages.
- Provider-independent MIME composition over those handles, including Unicode,
  alternative bodies, binary/inline attachments and reply headers.
- Optional durable per-instance HTTP mutation receipts. Claims precede network
  effects; interrupted requests are ambiguous and never automatically repeated.
  This prevents accidental retries, without claiming provider-side exactly-once
  delivery.
- Public MCP and opted-in installation management operations for plugin setup,
  account lifecycle and sign-in. Account use still requires its separate live
  plugin grant; installation management cannot acquire another account's data
  through its panel or form. Grant editing remains exclusive to the owner's
  agent editor, as required by [agent-centric permissions](0051-agent-centric-permissions.md).

This extends the general-OAuth deferral in the
[initial plugin decision](0038-luau-installation-plugins-and-external-bot-delegation.md).
Web and Android render the same generic OAuth contract. Mailbox identity and Gmail
API operations remain in `plugins/gmail/`; its generated catalog covers the entire
official REST discovery surface. Google eligibility, quotas and Workspace-only
requirements remain provider constraints. Owner OAuth does not impersonate a
domain-wide service account, configure Pub/Sub infrastructure or bypass consent.

## Compatibility and validation

Existing credential envelopes and device-flow behavior remain readable. New
fields/types are additive; no existing data is rewritten and the compatibility
epoch does not advance. Older binaries lack the Gmail package and reject installed
manifests declaring unsupported OAuth/files capabilities instead of operating on
them. Temporary files disappear on restart; receipts remain and are deleted with
their instance. Credential refresh and management writes recheck authorization
at publication, and in-flight revocation cannot disclose an API result.

Synthetic Google tests use the actual worker and host APIs: all discovered REST
methods, explicit account selection, concurrent refresh, one-shot owner-bound
callbacks, MIME reply/draft, large attachments, interrupted-send receipts and
revocation. Browser and Android journeys cover the generic sign-in editor;
external Google consent is an owner setup step. The opt-in transfer measurement
records local bytes, cold/warm reads, writes and process CPU separately from
production network latency.

See [plugin APIs](../PLUGINS.md) and [Gmail setup and limits](../../plugins/gmail/README.md).

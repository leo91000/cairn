# Slack plugin design

Status: Q1–Q20 accepted. Shared understanding confirmed; implementation and a
dedicated pull request are authorized. The implementation log records actual
delivery and remaining work.

## Established requirements

- Deliver a Slack plugin with broad useful functionality and rigorous protocol
  behavior, checked against Slack's current documentation and official SDKs.
- Cover three independently enabled roles: conversations with agents from Slack,
  Slack tools for agents, and event/workflow-triggered work.
- Target a private Slack app configured for each Cairn installation, supporting
  multiple workspaces. Public app distribution is outside this delivery.
- Allow several private Slack apps in one Cairn installation, including separate
  apps where unrelated standalone workspaces need native Workflow Builder steps.
- Use the bot for ordinary actions and optional per-person OAuth for user-only
  functions. Personal authorization is not pooled between service users; request
  only the rights required by enabled features.
- Support both Socket Mode and public HTTP reception. The public HTTP delivery
  path is part of this delivery, rather than a later extension.
- Ordinary DMs and private-channel exchanges imported into Cairn use the existing
  installation-wide conversation visibility, with explicit setup information and
  notice before the first private exchange. This does not waive RTS restrictions.
- Deliver the complete everyday Slack experience: public/private/Connect channels,
  DMs/group messages, threads, mentions, slash commands, shortcuts, buttons/forms,
  App Home, native agent streaming/progress/stop, incoming/outgoing files and
  deliverable links. Detect scope and plan availability and explain fallbacks.
- Default invocation: mentions, commands and shortcuts in channels; follow an
  engaged thread without repeated mentions; answer DMs. Listening to all channel
  messages is explicit. The owner chooses an agent and optional project by
  workspace/channel.
- RTS search uses a separate requester-only transient mode. Enable agent
  assistance only after complete no-retention behavior has been verified for its
  engine; otherwise offer transient human consultation. Results are not
  automatically added to shared conversations.
- Initial tools cover collaboration/productivity, including channel/user
  discovery, reading/search, messages and scheduling, reactions, files,
  links/pins, ordinary channel management, canvases/lists, personal status and
  workflows. Enterprise-wide administration, SCIM and audit are a separate module.
- One Slack thread maps to one Cairn conversation shared by its authorized
  participants. Import bounded authorized thread context. A top-level DM request
  creates a new conversation; continuing a thread resumes it. Provide new/reset
  controls.
- Public ingress uses a Beacon-provided URL by default, with optional direct
  owner-exposed HTTPS. Beacon relays without retaining payload content; the online
  installation owns secrets, request verification and accepted work.
- Authorized participants may contribute to a shared thread. Only the current
  run's author may answer its questions or stop it from Slack; the owner retains
  control in Cairn. Each run's personal authority belongs to its own requester.
- Explicitly enabled automation rules execute within their source, conditions,
  agent/project and rate/cost/concurrency limits. Human-origin events retain
  author admission checks; nonhuman triggers need dedicated owner authorization.
- Tool grants have Read, Contribute and Manage profiles, with fine adjustments by
  agent and Slack scope. Personal operations keep their requester-bound authority.
  No agent receives access by default.
- A newly delivered request up to 15 minutes old may execute automatically;
  older requests await their author. The window is configurable, and automation
  rules have their own age limits. Already accepted work uses Cairn recovery and
  deduplication rather than starting again.
- Provide guided web/Android setup with generated manifests, links/URLs, OAuth
  and capability diagnostics, plus advanced manual setup. Slack app creation is
  performed in Slack. Configuration remains a draft until activation.
- Ordinary imports follow Cairn retention. Source edits/deletions are marked and
  reflected in subsequent context loads; they do not retroactively erase existing
  Cairn work/history. Transient search responses and citations are not archived.
- Automation has strict start/rate/duration/concurrency limits. Show monetary cost
  only when measurable; subscription capacity is not a per-rule dollar ledger or
  a guaranteed invoice cap. This is the accepted Q19 clarification of Q14.
- Keep service-specific behavior in the multi-file Luau package. Add generic host
  capabilities where necessary, following the plugin architecture established by
  Discord and 1Password.
- Use the same declarative configuration and instance views on web and Android.
- Preserve the distinction between an installation, a plugin instance and an
  external delegation defined in `CONTEXT.md`.

## Decision tree

Q1–Q19 are accepted. The product decision frontier is empty. The complete
[implementation brief](SLACK-PLUGIN-SPEC.md) was confirmed in Q20 and implementation
is underway.

| Decision | Status | Proposed answer | Decisions it unlocks |
| --- | --- | --- | --- |
| Q1: Product roles | Accepted | Conversation from Slack, Slack tools for agents, event/workflow-triggered work; independently enabled | Conversation routing and visibility; tool catalogue and write permissions; automation triggers and execution identity |
| Q2: Distribution target | Accepted | Private app configured for each Cairn installation owner, supporting multiple Slack workspaces | Socket Mode versus public HTTP delivery; credentials and installation lifecycle; workspace/organization boundaries |
| Q3: Event transport | Accepted | Both Socket Mode and public HTTP reception | Public routing/authentication boundary; setup diagnostics; per-app selection and switching |
| Q4: Imported private conversation visibility | Accepted | Existing shared Cairn installation visibility, disclosed explicitly | DMs and private-channel context; external user admission; separate RTS handling |
| Q5: Everyday conversation surfaces | Accepted | Complete native Slack conversation, control and file experience in the initial delivery | Invocation/routing rules, native agent behavior and fallback, detailed feature matrix |
| Q6: Multiple private apps | Accepted | Support several private Slack apps in one Cairn installation when workspace/workflow distribution requires it | App/workspace instance model and setup journey |
| Q7: Action identity | Accepted | Bot by default, optional individual OAuth for user-only features; no pooled personal token | Search flow, user-only tool catalogue, actor binding and consent |
| Q8: Invocation defaults | Accepted | Mentions/commands/shortcuts in channels; continue engaged threads; answer DMs; all-message listening explicit; owner-chosen agent/project by workspace/channel | Conversation mapping, control authority and automation routing |
| Q9: Public endpoint placement | Accepted | Beacon URL by default, optional owner-exposed HTTPS URL; installation verifies requests and retains work/secrets; relay stores no payload | Exact ingress/callback contribution, availability and setup lifecycle |
| Q10: Search delivery | Accepted | Requester-only transient search, with agent assistance only on engines whose no-retention behavior is verified; human-only transient fallback | Transient execution contract, requester-aware UI, result/answer visibility and acceptance evidence |
| Q11: Tool catalogue | Accepted | Complete collaboration/productivity set; global Enterprise administration, SCIM and audit as a separate module | Exact method matrix and write permissions |
| Q12: Thread mapping | Accepted | One Slack thread per Cairn conversation; authorized thread context; DM top-level request creates a new conversation; new/reset control | Participant controls, interruption, history/edits and recovery |
| Q13: Shared thread controls | Accepted | Authorized participants can contribute; only current run's author controls its questions/stop from Slack; owner retains Cairn control; personal identity follows each requester | Control actor checks, late/stale actions and stop event behavior |
| Q14: Automation authority | Accepted | Explicit enabled rules execute automatically within source/condition/agent/project/rate/cost/concurrency limits; human authors still authorized; nonhuman triggers separately owner-authorized | Trigger catalogue, execution identity, replay/recovery and budgets |
| Q15: Tool permissions | Accepted | Read, Contribute and Manage profiles with fine overrides by agent and Slack scope; no agent access by default | Exact operation-to-profile matrix and grant enforcement |
| Q16: Late-request recovery | Accepted | Execute newly delivered requests up to 15 minutes old; older requests await their author; configurable per automation; accepted work uses normal recovery/deduplication | Retry/age policy, queue presentation and recovery tests |
| Q17: Setup | Accepted | Guided web/Android setup with generated Slack manifest/URLs, OAuth and diagnostics; advanced manual mode; draft before activation | Declarative collections/forms, draft callback lifecycle and setup acceptance |
| Q18: Retention and source changes | Accepted | Ordinary imports follow Cairn retention; reflect source edits/deletions on future context loads and mark them; no automatic retroactive erasure of existing work; transient answers/citations stay unarchived | Precise retention notices, invalidation, deletion and transient tests |
| Q19: Automation budget meaning | Accepted | Strict start/rate/duration/concurrency limits; monetary cost shown only when measurable, never a fabricated subscription invoice guarantee | Generic rule accounting, execution admission and honest usage UI |

The final confirmation concerns the complete design, concrete feature/permission
matrix and validation contract. Technical details must implement the accepted
boundaries, not reopen settled scope through undocumented assumptions.

The final review must include credential creation/renewal/revocation and workspace
removal; namespace and actor binding; concurrent thread messages and stale
controls; acknowledgement/durability/retry boundaries; scopes/plans and exact
methods; native web/Android setup; bounded overload; protocol fixtures and real
Slack validation. Distinguish proven behavior from proposed design and untested
live-service compatibility.

## Current factual investigation

Checked on 2026-10-17 against current Slack documentation and the official Node
SDK. These facts constrain the design; they do not settle the open product choices.

| Area | Verified constraint | Primary source |
| --- | --- | --- |
| Socket Mode | Opens a temporary WSS URL through `apps.connections.open` using an app-level token. Handle `hello`, connection refresh, unexpected loss and `link_disabled`; up to ten simultaneous connections receive payloads without fixed distribution. Acknowledge every envelope. Public Marketplace listing is unavailable for this transport. | [Socket Mode](https://docs.slack.dev/apis/events-api/using-socket-mode/) |
| Acknowledgements | Commands, actions and options need an acknowledgement within three seconds. Slow work cannot delay it. Socket retries must be duplicate-safe; the HTTP Events API retry timetable is not a documented Socket Mode guarantee. | [Acknowledgements](https://docs.slack.dev/tools/bolt-js/concepts/acknowledge/), [official SocketModeClient](https://github.com/slackapi/node-slack-sdk/blob/main/packages/socket-mode/src/SocketModeClient.ts) |
| App distribution | A private workspace app can be installed manually. Installation of the same app into other workspaces needs OAuth; distinguish that from customer-created separate apps. | [Distribution](https://docs.slack.dev/app-management/distribution/) |
| Tokens | App-level, bot and user credentials have different identities and rights. Rotating app-level tokens exist too; accepting only an `xapp-` prefix would reject valid credentials. Rotation must safely retain the newly issued refresh token. | [Tokens](https://docs.slack.dev/authentication/tokens/), [rotation](https://docs.slack.dev/authentication/using-token-rotation/) |
| Native agent experience | Current `agent_view` replaces legacy `assistant_view`, whose deprecation is scheduled for February 2027. Base the new plugin on current agent events and methods. | [Agent messaging update](https://docs.slack.dev/changelog/2026/08/20/agent-updates/) |
| Sessions and stop | Sessions belong to a channel/DM thread. Manage processing, active, suspended and closed status; subscribe to `agent_session_stopped`, cancel actual work and update status. Plain message posting does not finish a processing session automatically. | [Agent sessions](https://docs.slack.dev/ai/agent-sessions/) |
| Streaming | Native streams use start/append/stop operations. Channel streams require recipient identity as well as a thread; streaming is not equivalent to arbitrary top-level message updates. | [chat.startStream](https://docs.slack.dev/reference/methods/chat.startStream/) |
| Files | `files.upload` is retired. Allocate an upload URL, send bytes, then complete the upload; no completion means no retained file. Private downloads require authenticated access. | [File upload](https://docs.slack.dev/reference/methods/files.getUploadURLExternal/), [completion](https://docs.slack.dev/reference/methods/files.completeUploadExternal/) |
| Limits | Honor per-method/workspace/app limits and `Retry-After`. Internal customer-built apps and certain existing installations are exempt from the stricter new commercial non-Marketplace history/replies limits; do not assume a universal one-request-per-minute rule. | [Rate limits](https://docs.slack.dev/apis/web-api/rate-limits/), [history](https://docs.slack.dev/reference/methods/conversations.history/), [replies](https://docs.slack.dev/reference/methods/conversations.replies/) |
| Native workflow steps | Steps require an org-ready app and a paid plan. A standalone workspace can use its own app's steps, but those steps cannot be distributed to another standalone workspace. Enterprise requires organization installation and workspace access. Each function must report success or error. | [Workflow steps](https://docs.slack.dev/workflows/workflow-steps/) |
| Search | Current RTS uses `assistant.search.context`. Searching private content or invoking it outside Slack requires a user token; bot calls need an interaction `action_token`. Slack forbids retaining/copying retrieved data, exposing it to people without source access, and sharing authorization between service users. The guide also rejects use of legacy search endpoints as a substitute. | [Real-time Search](https://docs.slack.dev/apis/web-api/real-time-search-api/) |

No Slack app, credentials or external workspace resources have been created or
modified. Official SDK source above is a moving reference: pin an exact version
and commit before implementation conformance review.

Existing Cairn conversations are shared within their installation. A private Slack
surface must not silently be described as a private Cairn conversation; any change
to that boundary requires an explicit decision.

Two branches need further product decisions:

- Supporting native workflow steps in unrelated standalone workspaces may require
  a separate private Slack app per workspace. Distinguish this from multiple
  workspaces in the same Enterprise organization before finalizing app topology.
- Shared persisted tool results cannot be used as the default RTS integration.
  A compliant search design needs per-person authority and a verified transient
  context path, or an explicit narrower delivery scope. Acceptance of Q4 concerns
  ordinary imported conversations; it cannot override Slack's RTS constraints.

## Public HTTP protocol requirements

Supporting both transports means a selectable delivery mode for each Slack app;
Socket Mode redirects events/interactions away from HTTP rather than duplicating
delivery. OAuth's HTTPS callback can still coexist with Socket Mode.

HTTP Events use JSON; commands use URL-encoded fields; interactive requests wrap
JSON in the form field `payload`. One app's Events URL receives events for all its
workspaces. See [HTTP Events](https://docs.slack.dev/apis/events-api/using-http-request-urls/)
and [interaction handling](https://docs.slack.dev/interactivity/handling-user-interaction/).

Verify the exact raw body before parsing, using HMAC-SHA256 with the app signing
secret, the request timestamp and a constant-time signature comparison. Reject
timestamps outside the documented five-minute window. URL challenges must also
pass verification. Header names are case insensitive. Signing-secret rotation has
no documented previous-key grace period; do not confuse it with the OAuth client
secret's separate grace period.
[Signature verification](https://docs.slack.dev/authentication/verifying-requests-from-slack/)

Responses must honor the three-second deadline and each payload's semantics:
events can acknowledge accepted work; external selects need synchronous options;
modal submissions need synchronous validation or view actions. An empty modal
acknowledgement closes the view. Opening a modal also needs a single-use
`trigger_id` before it expires. A blanket empty HTTP 200 is insufficient.
See [acknowledgements](https://docs.slack.dev/tools/bolt-python/concepts/acknowledge/)
and [modals](https://docs.slack.dev/surfaces/modals/).

An interaction `response_url`, where present, permits up to five responses within
30 minutes; it is a capability URL and cannot replace the initial acknowledgement.
It defaults to an ephemeral reply; an in-channel response has different visibility.
[Interaction response contract](https://docs.slack.dev/interactivity/handling-user-interaction/)

The HTTP Events retry timetable is not a universal guarantee for commands,
interactions or Socket Mode. Delayed Events is a separate optional retry setting;
recovery policy must be decided explicitly rather than claiming lossless delivery.
[Events API](https://docs.slack.dev/apis/events-api/)

## Existing host capabilities and gaps

Inspected at `30a22be91f326c99357218a68b81cfd614705ff4`; these are candidates for
generic host work, not a final API design.

| Area | Available now | Gap or behavior to resolve |
| --- | --- | --- |
| Credentials | Multiple encrypted secret fields and selected HTTP-header injection; draft credentials work in setup forms | OAuth exchange/rotation must capture newly issued credentials without leaking them to Luau, logs or ordinary settings; no corresponding generic credential lifecycle contribution exists yet |
| Socket bootstrap | Authenticated HTTP POST and dynamic allowlisted WSS URL | Temporary socket tickets are ordinary strings visible to the worker; decide whether sensitive response fields need opaque handles |
| Acknowledgements | Generic sends and separate transport/event workers | Socket sends flush only when a handler returns. A send followed by slow admission/HTTP still delays the wire ACK |
| Admission | Atomic durable enqueue plus deduplication; owner-controlled external delegation | `inbound.accept` returns the same `false` for duplicates, denied input and queue saturation. Reliable ACK/backpressure handling needs to distinguish these outcomes |
| Private inbound files | Bounded attachment download from declared hosts | Downloader has no authentication injection. Normal HTTP responses are text/JSON, not binary artifact handles |
| Outbound files | JSON HTTP steps and artifact-backed multipart with configurable field names; Slack explicitly accepts multipart at its allocated upload URL | A new raw-body API is not required just for Slack uploads; verify the actual filename field and binary transfer with a fixture. Current artifact access is limited to the active reply |
| Multiple workspaces | Package can namespace inbound identities and routing keys | One app-level transport must dispatch to the matching workspace credential. Opening one competing socket per independent channel/workspace instance would not guarantee correct routing |
| Connection replacement | One active socket per instance; a replacement drops it | No concurrent old/new socket IDs for graceful overlap during rotation |
| Tools and controls | Declarative commands, MCP tools, instructions, instance/agent grants and arbitrary inbound kinds | Starting delegated conversations requires a normalized `message` event; owner-panel actions are distinct from external Slack interactions |
| Public HTTP ingress | Existing plugin API has no incoming public HTTP contribution | Q3 deliberately reopens ADR-0045's webhook deferral. Add a generic ingress capability; decide routing/verification/fast-response boundaries without native Slack-specific handlers |
| Transient agent search | Ordinary tool outputs, engine sessions and client conversation histories persist; specialized ephemeral title generation is not a generic execution mode | RTS cannot be wired into normal tool results. A separate requester-bound transient context/execution path needs proof across host, engines, logs, files/artifacts and clients before activation |

Relevant implementation: `crates/installation/src/plugins/{capabilities,socket,work,connections}.rs`,
`crates/plugin-api/src/lib.rs`, `packages/plugin-types/cairn.luau`.

Public ingress options have existing seams, but require new narrow capabilities:

- Beacon's public artifact relay demonstrates anonymous, installation-scoped
  forwarding through the installation's outbound tunnel. Ordinary relay requires
  a Cairn account, and neither current public identity nor relay scope authorizes
  plugin webhooks. Existing relay preserves body bytes but filters out provider
  signature/timestamp headers. General relay timeout is longer than Slack's ACK
  window. Do not reuse artifact ingress or member authority unchanged.
- Direct installation ingress can use the existing HTTP listener behind a
  user-managed HTTPS proxy. The default listener is loopback and ordinary API
  routes require trusted authority; adding a public URL does not authorize ingress.
- Either deployment needs bounded raw intake, package/instance approval and
  enablement checks, secret-backed verification, appropriate synchronous responses,
  and precise durable-admission outcomes. It requires an online installation.
- Existing Cairn account sign-in callbacks and MCP callback flows do not provide
  generic personal plugin OAuth. Form results cannot write secrets into the vault.
  Add generic lifecycle handling rather than persisting tokens in plugin storage.

Relevant implementation: `crates/beacon/src/relay.rs`,
`crates/protocol/src/lib.rs`, `crates/installation/src/{http,auth,mcp_oauth}.rs`,
`crates/installation/src/plugins/forms.rs`.

Transient search has more sinks than the visible chat UI. Ordinary tool results
are stored in conversation events, engine sessions persist for resume, and web and
Android cache conversation histories. The CLI's secret output mode does not make
an ordinary command tool transient. Current plugin panels avoid conversation
storage but are owner-only, have no requester identity in their scope, and do not
enforce a transient-data policy. Specialized ephemeral model helpers are not a
generic agent execution mode. No RTS compliance claim follows merely from hiding
activity output; the complete path needs evidence before activation.

## Catalogue research boundaries

These are available platform contracts, not accepted feature exclusions:

- [Canvases](https://docs.slack.dev/reference/methods/canvases.create/) and
  [Lists](https://docs.slack.dev/reference/methods/slackLists.create/) have bot/user
  APIs with corresponding scopes; plan restrictions apply.
- [Profile/status writes](https://docs.slack.dev/reference/methods/users.profile.set/)
  need a user token. Administrative operations have separate role and
  organization requirements; an ordinary bot grant cannot imply them.
- [Reminders and saved items](https://docs.slack.dev/changelog/2023-07-its-later-already-for-stars-and-reminders/)
  are not equivalent to scheduled messages. Do not claim a complete Slack Later
  integration where no suitable public API exists.
- Workflows mean three distinct directions: Slack events starting Cairn work;
  a Workflow Builder step calling Cairn and completing asynchronously; and Cairn
  invoking an existing Slack workflow webhook. A workflow webhook is an outgoing
  request from Cairn, not a public endpoint hosted by Cairn.
- Enterprise installation can lack a team ID. Slack Connect can change channel
  IDs and has distinct invoking/receiving installation identities. Credential and
  routing lookup must not assume every event has one fixed workspace namespace.
  See [OAuth installation objects](https://docs.slack.dev/tools/bolt-js/concepts/authenticating-oauth/)
  and [Slack Connect](https://docs.slack.dev/apis/slack-connect/).

## Decision log

- 2026-10-17: user requested a Slack plugin and invoked `grill-with-docs`.
  Accepted Q1 (all three roles) and Q2 (private app per installation with multiple
  workspaces). Opened Q3–Q5. Implementation has not started.
  [ADR-0048](adr/0048-private-slack-apps-per-installation.md) records the app
  ownership/distribution boundary.
- 2026-10-17: accepted Q3 with both transports, Q4 with explicitly disclosed shared
  Cairn visibility, and Q5 with the full everyday experience and availability-aware
  fallbacks. [ADR-0049](adr/0049-plugin-public-http-ingress.md) records the new
  incoming HTTP requirement. Implementation has not started.
- 2026-10-17: opened Q6 (multiple private apps), Q7 (bot and individual identity)
  and Q8 (invocation defaults). Public ingress placement and transient search
  execution are being checked against existing Cairn boundaries before asking
  their dependent product questions.
- 2026-10-17: accepted Q6–Q8 as proposed. Q9 (public URL placement) remains pending.
  [ADR-0071](adr/0071-personal-plugin-authorizations.md) records optional personal
  authorization. Opened Q10 (transient search), Q11 (catalogue) and Q12 (thread
  mapping). No implementation has started.
- 2026-10-17: accepted Q10–Q12 as proposed. Recorded the transient boundary in
  [consultations éphémères des plugins](adr/0072-transient-plugin-contexts.md). Re-presented unanswered Q9
  alongside Q13 (controls), Q14 (automations) and Q15 (tool profiles).
- 2026-10-17: accepted Q9 and Q13–Q15 as proposed. Beacon relay/direct addressing,
  per-run control, bounded explicit automations and scoped tool profiles are
  settled. Implementation has not started.
- 2026-10-17: opened Q16 (late requests), Q17 (guided setup) and Q18 (ordinary
  retention versus transient results). Prepared the final design review branches;
  no implementation or external Slack resource creation has started.
- 2026-10-17: accepted Q16–Q18. Accepted Q19: hard work/resource limits and honest
  measurable-cost reporting, rather than a subscription monetary-cap claim.
  Product questions are settled; final shared-understanding confirmation remains.
- 2026-10-17: accepted Q20. Implement the complete contract on a dedicated Slack
  branch, validate it and open a dedicated PR. No further permission is required
  for these authorized steps.

Create ADRs when a consequential trade-off is settled. Update `CONTEXT.md` only
when a new domain term or a changed domain meaning is actually agreed.

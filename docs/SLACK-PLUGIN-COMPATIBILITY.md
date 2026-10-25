# Slack foundations: current data contracts

The integration follows Main `85f9e805`, retaining the data contract introduced
by `40a59561`: the installation reads only database version **2**. Previous installation versions are not
migrated or opened as current data. Manager, runner, nodes and guests must use the
same release. The next production release requires an owner-authorized complete
reinstallation under [the production runbook](PRODUCTION-CAIRN.md#réinstallation-complète).
This implementation task authorizes no release, reinstallation, deletion of host
data, or production change. Preserve existing workspaces, credentials and evidence.

The earlier epoch-14 / SQLite-13 proposal and migration measurements belong to
[the historical checkpoint](https://github.com/leo91000/cairn/blob/4478fdb955fb36c8dc3c68a645a6c8d73964fe96/docs/SLACK-PLUGIN-COMPATIBILITY.md).
They are not a current compatibility promise. The old migration modules, updater
compatibility manifest, question-retirement migration and approval normalizer are
retired with Main. No fallback reader is added to resume older installation data.

Status: draft PR #226, incomplete Q1–Q20, **not merge or deployment approval**.
New integration validation must identify its exact source and current binaries;
previous checkpoint results retain their original source scope.

## Persistent inventory

These records describe fresh current data and ordinary recovery within the same
release. They do not describe an upgrade from the earlier development formats.

| Owner | Current data and authority |
| --- | --- |
| Registry | API-v2 package manifests, modular files, approvals and digests retain their declared webhooks, resources, operations, OAuth and command/tool bindings. Files are revalidated; changed or invalid code needs explicit review. No silent reapproval. |
| Instances | Validated settings, complete agent/secret metadata, revision, owner access and approved agent policy are current records. Root instance grants remain independent from resource grants. Missing stored permissions do not acquire implicit defaults or restore authority. Optional runtime routing is host-derived and never accepted from package settings. |
| Resources and grants | Instance/collection/parent identity and revisions bind each resource. Secret fields remain encrypted in the vault. Explicit operation/profile/scope grants belong to the owner-approved agent; creating metadata never grants an agent a private account. |
| Routes | A route records its selectors, enabled state, chosen agent/project, complete approved access, owner/package proof and limits. Its edit token covers all fields. Admitted work retains the native route revision plus individual person generations; removing Bob does not invalidate Alice. Reapproval cannot revive Bob's retired work. |
| Resource and personal OAuth | Encrypted pending/completed authorizations, external subjects and generation links bind exact owner/package/resource and actor. Requested and granted scopes remain distinct. A thread never borrows another participant's personal authorization or defaults to the owner's identity. |
| Root credential renewal | The encrypted root credential and per-instance lock retain optional consumed-refresh state. Refresh intent removes the refresh token before I/O; confirmed authorized completion replaces that record. Absent consumed state is the ordinary current shape. Uncertain exchange/refresh requires reconnection, not replay. |
| Prepared setup | Expiring root drafts and inert resource namespaces bind owner, package approval and revision. Stable IDs survive activation. Pending consent is not promoted; finished credentials and resources become live only in activation's transaction. Discard, expiry and deletion clean references and secrets. No inert record is operational authority. |
| Inputs and delegation | Per-message requester proofs and personal-suspension markers retain the original human/external identity. The delegated input copies the source and its scope ceiling. Internal child returns retain their own marker without manufacturing a Slack destination. A later human input has its own authority. Conversation purge removes the associated KV prefixes. |
| Native waits | Current grouped conditions and member-specific observations retain owner/run/slot/deadline/requester identity. The accepted continuation reserves its own reply destination. Replacement and late progress cannot restore a retired reservation or another participant's authority. |
| Queued events and delivery | Events retain source/resource/route/person/package proofs and bounded retry state. Optional `startsWork` is a capacity hint, never authority. Already accepted delivery has no automatic 24-hour discard. `continuationOf` separates resumed output. Captured terminal ranges stay before the next requester and use current status values. Old field omissions are not a supported cross-version replay contract. |
| Question controls | Current persisted questions carry their issuing turn. Current code does not scan or migrate unattributed older questions into new authority. Delivery-owned nonsecret control values retain canonical fields/partial choices, request/person/provider-message binding and posting intent. Native question IDs include provider-turn identity. Secret or unsupported questions remain in Cairn. Deleting the delivery removes its bound values. |
| Progress controls | Delivery-owned progress stores its deadline, exact person/source/message binding and preparing/posting/sent/update intent. Definite quota refusal can retry; uncertain creation is not replayed. A confirmed Stop binding survives an uncertain status edit, and final/native cleanup retires it. |
| Clients | Bounded Android setup snapshots exclude declared secrets and backups and are cleared when their session/scope retires. Web drafts stay browser-local. They are disposable editor state, not an offline mutation queue. Installation-resolved setup and current generated resume contracts are consumed by both clients. |

## Current recovery boundaries

Every import, credential use, control, wait observation and reply rechecks the
captured authority after relevant I/O and at its transaction. Temporary authority
or metadata failure remains retryable where no uncertain external mutation is
repeated. Conditional retry/checkpoint writes do not recreate removed or replaced
work. A final attempt's output never absorbs a later requester's messages/files.

Native waits, retries, manager restart and same-version storage recovery must
preserve files, history, identity and reply destination. They cannot repair an
incompatible older installation version by rewriting it. Invalid or incomplete
approval data fails closed; the owner must explicitly review current configuration.

OAuth codes and rotated tokens are externally consumed state. Restoring local JSON
cannot undo that provider effect. Unknown POST outcomes retain their write intent;
only definite refusals such as quota rejection permit automatic retry. Local
removal and provider revocation remain distinct, and callback completion rechecks
its original owner/actor/resource rather than substituting a current one.
A confirmed, validated refresh may retain its previous refresh token when the
provider does not rotate it; an uncertain exchange cannot restore that token.

Resource/route metadata management shares the owner UI's validation and effects
with public MCP and the installation tool. Full management permission does not
supply private credentials or bypass owner-only agent grant exceptions. Dedicated
delegation retains its source ceiling; management-created work uses the separately
approved semantics of its own configured agent. Neither path can silently exchange
its original requester for a broader principal.

## Qualification and observations

The implementation remains incomplete. Record exact current-source native, CLI/MCP,
web, Android and browser results and their binary identities. Fixture providers do
not establish real Slack behavior. All Q1–Q20, OAuth/grant mutation parity, guided
setup, complete Socket lifecycle, role/tool catalogue, native surfaces/files,
automation and requester-only transient search remain delivery requirements.

Local storage observations retained in history describe representative JSON/key
bytes, KV reads/writes, CPU and workload, not production latency or whole-service
recovery. Examples are the historical
[root credential renewal](https://github.com/leo91000/cairn/blob/4ccc044c0337bf246f9bae3a785c4c686124cc49/docs/performance/ROOT-FORM-CREDENTIAL-RENEWAL.md),
[progress state](https://github.com/leo91000/cairn/blob/4ccc044c0337bf246f9bae3a785c4c686124cc49/docs/performance/SLACK-PROGRESS-STATE.md),
[delegated requester](https://github.com/leo91000/cairn/blob/4ccc044c0337bf246f9bae3a785c4c686124cc49/docs/performance/DELEGATED-PLUGIN-SOURCE.md)
and [authority retry](https://github.com/leo91000/cairn/blob/4ccc044c0337bf246f9bae3a785c4c686124cc49/docs/performance/PLUGIN-AUTHORITY-OUTAGE.md)
measurements. They do not qualify the current fresh-data release by themselves.
No measurement artifacts are committed in the current tree.

Production qualification must follow the current runbook, including exact-tree CI,
matched images and served-engine validation, owner-approved provisioning/recovery,
and actual service authorization. This draft neither triggers nor approves those
effects. Keep the PR draft and unmerged until the full accepted contract and its
required evidence are complete.

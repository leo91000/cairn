# Automatic working environment expiration

The installation owner sets **Days of inactivity** under web **Settings →
Installation → Automatic environment cleanup**, or **Jours d’inactivité** in
Android **Paramètres → Nettoyage automatique des environnements**. The setting
applies to every conversation, including existing conversations, and is saved
server-side. The default remains **7 days**; whole numbers from 0 to 36500 are
accepted. **0 disables future automatic expiration**. Both apps confirm the
irreversible loss of working files before saving.

Expiration removes the conversation's working environment: local run files,
managed worktrees (including unpushed changes), tools stored in the environment, native provider
session, node disks and published disk references. It retains the conversation,
messages, activity, uploaded attachments, all published artifact versions and
public artifact links. These remain readable without starting an environment.
Explicit conversation deletion still removes all of them and revokes shared links.
Previously deleted conversations cannot be recovered by this policy change.

Inactivity starts at `lastActivityAt` or the last run's `finishedAt`, whichever is
later. Reading does not extend the delay. New empty chats and converted missions
initialize their activity at creation. Queued/running agents, pending input,
open questions (including nonblocking questions), scheduled wakeups, recovery,
node movement, backend migration, coding-agent switches, run preparation and
worker finalization protect the environment. Pausing alone does not protect it.

The worker checks every 15 seconds and processes at most one eligible conversation
per pass. It rechecks policy, activity and pending work atomically before claiming
expiration, under the existing process, storage, preparation and worker locks.
Once claimed, expiration is irreversible. The conversation immediately reports
`environmentState: "expired"` and detaches its old environment in the writer
transaction. Older `cleaning` environments are expired idempotently on startup.
After the old execution stops (or its unreachable node's lease lapses), the
manager moves its run files and local publication files into
`retired-environments/<retirementId>/`. A writer transaction detaches the old
account lease and exact recovery references. This opens admission before physical
erasure: failed worktree deletion, provider-home erasure or remote publication
retirement never blocks a fresh turn. Fencing and namespace detachment have their
own worker loop, independent of slow physical cleanup.

Physical retirement continues from a durable queue and retries failures after one
minute, even if future automatic expiration is disabled. A node must delete the
retired disk before admitting this run's next environment there; other nodes need
not wait for that deletion. A revoked node's disk is forgotten. Each disk
retirement sends its durable queue identity as `retirementId`. Native and VM
controllers serialize retries and persist a completion receipt outside the disk
before acknowledging it. Receipts survive subsequent environments and controller
restarts, so delayed duplicates are harmless. They remain permanently (one small
receipt per retired environment on that node).

The manager checks `diskRetirementProtocol: 1` before sending deletion and requires
the matching acknowledgement. Fresh environments require a compatible controller;
legacy controllers cannot silently ignore the request identity. The bundled VM
controller shares the manager volume, so its verified upgrade is also required
before manager namespace reuse. That proof survives temporary controller outages.
Controller deletion without an identity requires `permanentlyDeleted: true` and is
reserved for irreversible conversation purge. Manual whole stale-disk cleanup uses
the same durable queue and rechecks current ownership in its writer transaction.
Revocation also expires retained environments in legacy, restorable Trash. It
keeps those conversations trashed and paused without extending their restoration
deadline. New conversation deletion retains ADR-0041 behavior with no recovery
window.

After expiration, send a new message (and resume the queue if paused) to recreate
the environment. The same conversation and run retain their history and artifacts;
the old checkpoint, session and disk pointers are not reused. The fresh native
session receives the bounded visible transcript, and uploaded attachments are
copied back into its input directory. Every subsequent agent turn carries the
expiration date and cause (inactivity, node change, node revocation or storage
migration), and warns that older files and unpushed changes were removed.
The agent must inspect current files and verify external effects before repeating
actions; environment expiration does not undo commits, messages or deployments.
Web and Android show the expiration in the conversation.

An explicit node change uses the same expiration, including VM ↔ native changes.
The destination and current grants are checked before files are discarded. Every
surface requires `discardWorkingFiles: true` for an existing environment, separately
from migration's `confirm: true`. The current turn stops and open questions close.
The first admission waits on the destination while it remains authorized and not
revoked; otherwise placement becomes automatic. It does not permanently pin the
conversation. A new explicit choice replaces this destination without another
warning while the environment is expired. Queued messages and scheduled or
conditional waits retain their continuations. Active missions without a wait
resume automatically; a chat otherwise waits for its next message. An agent moving
its own conversation automatically continues there with the expiration warning.

For an already expired environment, placement or `migrate_chat_backend` selects the
next node/backend without a loss warning or `discardWorkingFiles`. The migration
preview reports `environmentExpired`. Current management, destination and native
provenance grants still apply. No old disk is read, transferred or converted.

`GET /api/settings/conversations` returns `{ "inactivityDays": 7 }` by default;
`PUT` accepts the same shape and records an audit entry. Existing owner-only HTTP
authorization is unchanged. Public MCP and `cairn_installation` expose
`get_conversation_retention` (`read`) and `set_conversation_retention` (`manage`),
with revocable installation grants rechecked at the database boundary.
`list_chats` / `get_chat` expose `environmentState`, `environmentExpiredAt`,
`environmentExpirationCause` and `environmentError`; normal chat and artifact tools keep their authorization.

Remote retirement queues exact old manifest keys rather than a run-wide prefix:
a delayed cleanup must never erase a newly created environment's publications.
Shared immutable blocks keep their existing ownership-aware collection.

Expiration metadata uses the existing `records` store: `environment-expirations`
holds the detached environment while its interrupted worker finishes stopping;
`environment-retirements` holds captured attempt/process/node identities, worktrees,
projects and retry progress. No SQLite schema/version change is needed. The run
boundary records an `environmentAdmissionRevision` to reject stale preparations
when the first destination/backend is replaced, and `environmentExpirationCause` alongside `environmentExpiredAt`.
An interrupted worker's checkpoint is retained only for fencing until it stops;
then it is replaced by a fresh checkpoint. The queue removes manager/provider files
and retires exact old S3 references before admission, so a delayed cleanup cannot
erase a new environment. Node disk deletion can finish afterwards on other nodes.

## Validation

Backend lifecycle tests cover policy validation and persistence, protected work,
atomic rechecks, bounded passes, retry after interruption, preserved history,
uploads/artifacts, explicit deletion and fresh-session continuation. MCP tests
exercise equivalent retention settings and artifact access through both interfaces.
Browser and shared Android Compose tests cover settings and expiration notices.

Local storage measurements can be reproduced with the isolated backend launcher:

```sh
CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 \
  pnpm test:backend -p cairn-installation --test conversation_lifecycle \
  measure_retention_storage -- --ignored --nocapture --test-threads=1
```

The synthetic workload measures 100 inactive chats, 50 warm/fresh-connection reads
and writes, and 20 cleanup passes. A fresh connection still has a warm OS page
cache. Record reference/candidate SHAs, encoded bytes, median latency, CPU and host
conditions in the PR; commit no measurement artifacts. These are local SQLite and
filesystem measurements, not production node/S3 network latency.

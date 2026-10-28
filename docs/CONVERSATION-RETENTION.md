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
Once claimed, expiration is irreversible. `environmentState` is `cleaning` until
physical retirement succeeds, then `expired`. Unavailable nodes/storage leave
cleanup pending with a visible `environmentError`; retries start after one minute,
even if future expiration is disabled. New messages are refused during cleanup.
Manual deletion remains available during cleanup.

After expiration, send a new message (and resume the queue if paused) to recreate
the environment. The same conversation and run retain their history and artifacts;
the old checkpoint, session and disk pointers are not reused. The fresh native
session receives the bounded visible transcript, and uploaded attachments are
copied back into its input directory. Every subsequent agent turn carries the
expiration date and warns that older files and unpushed changes were removed.
The agent must inspect current files and verify external effects before repeating
actions; environment expiration does not undo commits, messages or deployments.
Web and Android show the expiration and pending cleanup in the conversation.

If the agent's execution backend changed after expiration, use the existing
explicit backend migration confirmation in its execution-node details. It checks
current management, destination and provenance grants, then selects the new backend
without reading or converting the deleted disk. The next message creates an empty
environment on that backend. History, uploads and published artifacts remain; other
reduced resource permissions still prevent continuation under the existing policy.
The same confirmed action is available through public MCP and `cairn_installation`;
its preview reports `environmentExpired`. Cleanup must finish before it is allowed.

`GET /api/settings/conversations` returns `{ "inactivityDays": 7 }` by default;
`PUT` accepts the same shape and records an audit entry. Existing owner-only HTTP
authorization is unchanged. Public MCP and `cairn_installation` expose
`get_conversation_retention` (`read`) and `set_conversation_retention` (`manage`),
with revocable installation grants rechecked at the database boundary.
`list_chats` / `get_chat` expose `environmentState`, `environmentExpiredAt` and
`environmentError`; normal chat and artifact tools keep their authorization.

Remote retirement queues exact old manifest keys rather than a run-wide prefix:
a delayed cleanup must never erase a newly created environment's publications.
Shared immutable blocks keep their existing ownership-aware collection.

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

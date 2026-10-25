# Automatic conversation deletion

The installation owner can set **Days of inactivity** under web **Settings →
Installation → Automatic conversation deletion**, or **Jours d’inactivité** in
Android **Paramètres → Suppression automatique des conversations**. The setting
applies to every conversation in the installation and is saved server-side.
The default is **7 days**. Choose a whole number from 0 to 36500; **0 disables
future automatic deletion**. Both apps ask for confirmation before saving and
explain that existing conversations are included.

Inactivity starts at the last conversation activity (`lastActivityAt`, falling
back to `updatedAt`, then `createdAt` for older records), or the last run's
`finishedAt`, whichever is later. Merely reading a conversation does not extend
its life. A queued/running agent, pending message, open question (including a
nonblocking question), scheduled wakeup, run preparation or worker finalization
protects the conversation. Pausing a conversation does not exempt it once its
pending work is resolved.

The worker checks every 15 seconds and removes at most one eligible conversation
per pass. It rechecks the current policy, activity and pending work atomically
before claiming deletion; a concurrent message or disabling the policy wins if
it commits first. Existing process, storage, preparation and worker locks prevent
cleanup from racing run setup or finalization.

Deletion uses the existing permanent deletion path: immediately hide the
conversation, revoke public artifact links and grants, then remove its history,
attachments, artifacts, workspaces and published disks. Unavailable nodes/storage
leave durable cleanup state and retry after one minute. Logical deletion is
irreversible; physical erasure can finish later. Disabling or increasing the
delay cannot recover a conversation already claimed for deletion. Existing trash
keeps its promised recovery deadline.

`GET /api/settings/conversations` returns `{ "inactivityDays": 7 }` by default.
`PUT` at the same owner-only endpoint accepts the same shape and records an audit
entry. The `conversation-expiration` KV record stores the current policy.
Deleted conversations cannot be restored by changing the policy.

## Validation

Backend lifecycle tests cover default/configured/disabled retention, invalid
values, owner permissions, policy persistence, the legacy activity fallback,
recent run completion, pending work, finalization, concurrency and physical
cleanup including public links. The settings browser journey covers confirmation,
cancellation, persistence and disabling; shared Android Compose cases cover the
same actions, input validation and failed-save retry on JVM and device.

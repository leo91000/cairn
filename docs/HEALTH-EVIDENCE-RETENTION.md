# Health monitor evidence retention

`scripts/health-evidence-retention.mjs` is the archive engine used by the external
installation health monitor. It does not run inside Cairn or change an installation.
The monitor copies this exact versioned file next to its private adapter:

```js
export { retainAlert } from './health-evidence-retention.mjs'
```

The candidate manifest records the file's SHA-256. Review and activation must
check that it matches the tested public file. Private node addresses, account
identities, state, observations and notification transport stay outside this
repository.

## Contract

The monitor first persists the complete observation and outbox entry. It then
calls:

```js
const retention = await retainAlert(
  directory,
  eventId,
  originalEntry,
  coordinator,
  pendingNotification,
  relatedNotifications,
)
```

One archive writer owns the directory. Event identifiers are unique and contain
only digits, `T`, `Z` and hyphens, up to 80 characters. Existing archive and staging
files are never overwritten. Staging files use exclusive creation and mode `0600`;
only a temporary file created by the current call is cleaned up. A staging file
left by a terminated process is preserved for operator inspection.

Protection comes from `coordinator.retainedAlerts`, each incident's
`evidencePaths`, and `incidentPath` on pending or related notifications. Optional
collections may be absent. Present collections must be arrays, incidents and
notifications must be plain objects, and paths must be nonempty strings without
NUL characters. A notification may have no archive path or `incidentPath: null`.
Relative paths resolve against the archive directory. Historical references need
not still exist. Structured annotations belong in a separate metadata field,
not in a list of protected paths.

All protection and budgets are validated before any archive mutation. An unknown
container or element blocks the entire operation; it is never filtered or coerced
into an empty protection list. Defaults are 32 archive files and 8 MiB. Configured
budgets must be positive safe integers. Pruning considers only regular
`alert-*.json` archive files, oldest first, and never removes a protected path.
It plans a feasible archive first and stages the new content before pruning.

Success returns an `incidentPath`, file/byte counts and `pressure: false`.
Unavailability returns `incidentPath: null`, `pressure: true` and a fixed reason:

| Reason | Meaning |
| --- | --- |
| `invalid_archive` | Invalid directory or event identifier. |
| `invalid_protection` | Protection schema is unknown; nothing is pruned or archived. |
| `invalid_budget` | Invalid monitor configuration or archive budget. |
| `archive_serialization` | The event cannot be serialized as JSON. |
| `archive_budget` | The event cannot fit without removing protected evidence. |
| `archive_conflict` | The archive identifier already exists. |
| `archive_io` | A filesystem operation failed. |

Diagnostic fields contain only fixed schema field/stage names and allowlisted
error codes. They contain no raw paths, exception messages or evidence values.

## Delivery and failure isolation

An archive is optional; durable evidence is not. When the archive returns no path,
the monitor delivers the original durable outbox/sample reference and records the
bounded archive diagnostic. It keeps observing, appending samples and retrying
failed notification transport. The call boundary also isolates unexpected archive
exceptions. It does not swallow failures to persist the primary observation or
outbox, and it does not mark a notification sent before transport acceptance.

This contract neither acknowledges incidents or coverage gaps nor clears cursors,
outbox entries or recovery state. A clean new observation does not erase evidence
of a transient incident awaiting delivery.

## Validation

Run `node --test scripts/tests/health-evidence-retention.node.mjs`. The normal
`pnpm test` suite runs the same Node cases through a Vitest wrapper. Cases cover
malformed containers/elements, historical and notification pins, count/byte
budgets, serialization, existing evidence, staging ownership and filesystem faults.

The private candidate must additionally run the real copied watcher tick with
only its probes and transport simulated: failed transport followed by restart and
retry, original-sample delivery, malformed protection, archive exceptions, primary
state persistence failures and unchanged historical log cursors/gaps. Publishing
this helper does not activate the private candidate or prove a live runtime repair.

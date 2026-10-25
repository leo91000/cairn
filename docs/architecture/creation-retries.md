# Retrying resource creation

Editors keep a saved resource ID as soon as the create response arrives, before
saving a GitHub token or refreshing the workspace. A failed secondary operation
leaves the editor open with a message that distinguishes the committed save from
the failed token update or reload. The next save updates that ID.

A lost POST response is a separate case. Web and Android editor sessions generate
a random UUID and send it as optional `creationId` on POST `/agents`, `/projects`
and `/tasks`. The server validates it as UUIDv4 and uses it as the normal record
ID. In the write transaction, an existing record returns unchanged; a collision
with another resource kind is rejected. Concurrent requests and process restarts
therefore keep one resource and one mission schedule. Clients without this field
retain the existing server-generated identity contract. PUT uses the URL ID and
ignores `creationId`.

The editor retains the original request after an uncertain response. A retry first
recovers that creation, then applies subsequent form edits with PUT. It stores the
acquired ID before that PUT, so even a failed update keeps future retries on the
same resource. A definitive validation rejection allows correction of the initial
request. GitHub imports retain their existing repository-based idempotency; once
an import response is acquired, subsequent saves update the returned project.

This protects retries within an editor session. Deliberately starting a new editor
session creates a new identity. Deleting the resource ends the lifetime of its
identity; there is no permanent request tombstone or deduplication by mission text.

## Compatibility

No installation persistent schema, record shape, storage codec, or database version changes.
`creationId` is removed by input normalization and is not stored as an extra field.

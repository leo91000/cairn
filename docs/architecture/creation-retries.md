# Retrying resource creation

Editors keep a saved resource ID as soon as the create response arrives, before
saving a GitHub token or refreshing the workspace. A failed secondary operation
leaves the editor open with a message that distinguishes the committed save from
the failed token update or reload. The next save updates that ID.

A lost POST response is a separate case. Web and Android editor sessions generate
a random UUID and send it as optional `creationId` when creating an agent, a
local project or a hook rule. The same ID recovers the first committed response
across concurrent requests and process restarts. The rule path uses an
account-scoped receipt. Subsequent edits address the returned resource ID.

The editor retains the original request after an uncertain response. A retry first
recovers that creation, then applies subsequent form edits with PUT. It stores the
acquired ID before that PUT, so even a failed update keeps future retries on the
same resource. A definitive validation rejection allows correction of the initial
request. GitHub imports retain their existing repository-based idempotency; once
an import response is acquired, subsequent saves update the returned project.

This protects retries within an editor session. Deliberately starting a new editor
session creates a new identity.

## Compatibility

Callers can omit `creationId` to use a server-generated identity.

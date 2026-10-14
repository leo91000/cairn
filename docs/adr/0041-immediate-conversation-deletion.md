# Immediate permanent conversation deletion

New conversation deletions have no recovery window, as requested by the owner. This supersedes the 30-day trash policy for new deletions in ADR-0008 and the historical conversation lifecycle proposal. Web and Android require confirmation before sending the deletion request; Android no longer offers Undo.

Deletion immediately hides the conversation, revokes public access and cancels pending input. Physical cleanup remains asynchronous and retryable through the existing worker, which waits for running agents to stop and safely deletes disks, published objects and records. Immediate irreversibility does not promise synchronous erasure on unavailable nodes.

Previously trashed conversations retain their original deadline and recovery behavior. Keeping those promises avoids an unannounced destructive migration; expiring all existing trash immediately was rejected.

> Statut : Mise à jour par [ADR-0084](0084-local-conversation-disks-without-s3.md) : la suppression efface les disques locaux et les enregistrements de la conversation ; Cairn n’efface plus les anciens objets publiés, que l’exploitant supprime avec leur bucket. La suppression immédiate et irréversible reste applicable.

# Immediate permanent conversation deletion

New conversation deletions have no recovery window, as requested by the owner. This supersedes the 30-day trash policy for new deletions in ADR-0008 and the historical conversation lifecycle proposal. Web and Android require confirmation before sending the deletion request; Android no longer offers Undo.

Deletion immediately hides the conversation, revokes public access and cancels pending input. Physical cleanup remains asynchronous and retryable through the existing worker, which waits for running agents to stop and safely deletes disks, published objects and records. Immediate irreversibility does not promise synchronous erasure on unavailable nodes.

A permanently revoked node cannot acknowledge disk removal. Once a conversation is
being permanently purged, the manager releases that node's volume and disk-grant
references so its published data and conversation records can be erased. Local
erasure on the revoked machine remains unconfirmed. Nodes that are temporarily
offline or unknown still keep cleanup pending until disk removal succeeds.

Previously trashed conversations retain their original deadline and recovery behavior. Keeping those promises avoids an unannounced destructive migration; expiring all existing trash immediately was rejected.

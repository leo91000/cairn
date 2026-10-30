# Conversation storage operations

## Current disk publication

Conversation disks synchronize to S3 in the background. Acknowledged writes are durable in the local journal; the default remote publication target is 60 seconds. After five minutes of unsynchronized work the execution reports overdue synchronization and continues while its local disk is safe. Integrity, free-space and unavailable cold-read protections remain active. This is not synchronous remote durability: losing the node can lose its unpublished work. See [direct storage transfers](DIRECT-STORAGE-TRANSFERS.md).

Configure `STORAGE_S3_BUCKET`, optionally `STORAGE_S3_ENDPOINT` (HTTPS) and `STORAGE_S3_REGION`, or a server-owned `DATA_DIR/storage-s3.json` with `bucket`, `endpoint`, and `region`. Credentials remain server-side via AWS configuration or a workload role. `AWS_*`, `STORAGE_S3_*` and legacy `ARCHIVE_*` variables are removed from agent subprocesses.

Use a private bucket without lifecycle rules or Object Lock. Current disk blocks must remain immediately readable and deletable. S3-compatible providers without public access blocks are checked through their ACL and bucket policy. Bucket inspection and object read/write/delete permissions are required, including version and multipart deletion. The optional server-only `awsBinary` configuration chooses the CLI used for bucket administration and purge; block reads and verified writes use the persistent SDK client.

Only the latest published state is retained. Mounted disks and unfinished publication acknowledgements pin their required generations until they can safely release them. These references protect live reads; they are not restore history. Cleanup runs after publication and periodically for idle disks. Missing or damaged referenced manifests prevent destructive collection.

## Provider encryption and transfer concurrency

`STORAGE_S3_SERVER_SIDE_ENCRYPTION=none` omits the S3 server-side encryption request
header. `AES256` explicitly requests SSE-S3; other nonempty values are refused.
When unset, R2 endpoints omit this unsupported header and other S3 providers keep
the existing AES256 request. This setting does not disable application AES-256-GCM
encryption of disk blocks or HTTPS transport. R2 encrypts objects at rest itself.

Recovery transfers share sixteen upload slots and sixteen read slots across the
manager. The guest virtqueue also batches independent reads through sixteen shared
workers, bounded to 8 MiB per batch. Single requests stay on the existing direct
path; reads drain before a following write or flush. Each PUT includes a `Content-MD5` transfer checksum and holds its upload slot until
the provider acknowledges the checksum-validated upload. A rejected or failed PUT
keeps the local publication pending. Uploads do not download the object again;
recovery reads retain their existing content-hash and authenticated-decryption checks.
The provider must enforce `Content-MD5`; compatibility checks and periodic restore
tests should include intentionally incorrect checksums and complete reads.
This bounds memory while allowing independent block transfers to overlap; an
individual missing block still waits for its own transfer. Sealed journals also
reconstruct up to sixteen independent blocks together, preserving generation
fences and the existing foreground cache admission order. Direct local publications
use one journal reader for the full stream; remote requests remain bounded to
32 MiB to fit reverse-proxy deadlines. A local cold journal has up to 30 minutes
to initialize, with the existing 120-second idle deadline for every streamed block. R2 requires a private
dedicated bucket without public domains, locks or enabled lifecycle rules, with
`STORAGE_S3_PRIVATE_BUCKET_CONFIRMED=true` after those provider controls are checked.

## Environment expiration and deletion

After the configured inactivity delay (7 days by default), only the working environment expires. History, uploaded attachments, published artifacts and shared artifact links remain available. A new message recreates an environment and warns the agent about the lost working files. See [conversation retention](CONVERSATION-RETENTION.md).

Conversation archival, archive restoration and cold transitions have been removed. Before deploying this change, finish the owner-authorized deletion of existing archived conversations and their objects using the prior release or an explicitly scoped maintenance procedure. Never empty a bucket referenced by active on-demand disks.

Deleting a conversation immediately revokes public links, cancels pending work and removes the ability to restore it. There is no 30-day retention window for new deletions. Deletion wakes the cleanup worker immediately; it also checks every 15 seconds and removes one eligible conversation per pass after its agent stops: conversation files, disk instances, S3 publications and database records. Unavailable nodes and other failures remain visible and retryable. Existing trash keeps its original recovery deadline; this change does not accelerate previously promised retention. See [ADR-0041](adr/0041-immediate-conversation-deletion.md).

## Recovery prerequisites

Protect the application database and `DATA_DIR/mcp-encryption-key` separately. Remote disk objects alone cannot recreate the application or decrypt its data. The key is never uploaded alongside disk blocks. Keep configuration and persisted object keys stable while disks reference them.

See [ADR-0008](adr/0008-current-disk-publication-without-archives.md).

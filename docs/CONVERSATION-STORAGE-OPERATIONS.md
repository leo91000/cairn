# Conversation storage operations

## Current disk publication

Conversation disks synchronize to S3 in the background. Acknowledged writes are durable in the local journal; the default remote publication target is 60 seconds, with execution paused after 5 minutes of unsynchronized work. This is not synchronous remote durability.

Configure `STORAGE_S3_BUCKET`, optionally `STORAGE_S3_ENDPOINT` (HTTPS) and `STORAGE_S3_REGION`, or a server-owned `DATA_DIR/storage-s3.json` with `bucket`, `endpoint`, and `region`. For compatibility, `ARCHIVE_S3_BUCKET`, `ARCHIVE_S3_ENDPOINT`, `ARCHIVE_S3_REGION` and `archive-s3.json` are accepted when the new configuration is absent. They no longer enable archival. Credentials remain server-side via AWS configuration or a workload role. `AWS_*`, `STORAGE_S3_*` and legacy `ARCHIVE_*` variables are removed from agent subprocesses.

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
path; reads drain before a following write or flush. An upload slot remains held until full read-back verification succeeds.
This bounds memory while allowing independent block transfers to overlap; an
individual missing block still waits for its own transfer. Sealed journals also
reconstruct up to sixteen independent blocks together, preserving generation
fences and the existing foreground cache admission order. R2 requires a private
dedicated bucket without public domains, locks or enabled lifecycle rules, with
`STORAGE_S3_PRIVATE_BUCKET_CONFIRMED=true` after those provider controls are checked.

## Deletion

Conversation archival, archive restoration and cold transitions have been removed. Before deploying this change, finish the owner-authorized deletion of existing archived conversations and their objects using the prior release or an explicitly scoped maintenance procedure. Never empty a bucket referenced by active on-demand disks.

Trash revokes public links and cancels pending work. Conversations are recoverable for 30 days without automatically restarting execution or restoring public grants. Expired trash removes conversation files, disk instances, S3 publications and database records. Failures remain visible and retryable.

## Recovery prerequisites

Protect the application database and `DATA_DIR/mcp-encryption-key` separately. Remote disk objects alone cannot recreate the application or decrypt its data. The key is never uploaded alongside disk blocks. Keep configuration and persisted object keys stable while disks reference them.

See [the current specification](STORAGE-SYNC-SIMPLIFICATION.md) and [ADR-0008](adr/0008-current-disk-publication-without-archives.md).

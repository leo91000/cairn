# Compression of immutable disk objects

Date: 2026-10-11.

New immutable S3 disk objects compress their plaintext with zstd level 3 when it
saves bytes, then encrypt the representation with AES-256-GCM. Content identities
remain hashes of the original bytes. The versioned envelope authenticates its
codec, plaintext length and object locator, and readers bound decompression to the
authenticated manifest's extent size and the existing 4 MiB maximum.

The installation accepts downtime for a blocking startup migration. Before any
listener, relay or publication worker starts, a durable migration copies existing
referenced binary, JSON/base64 and prior v2 objects into a new key namespace,
verifies their authenticated plaintext identity, and purges the old keys and their
versions. Local encoded caches are converted with a durable atomic rename.
Interrupted work resumes from SQLite checkpoints; a failed conversion keeps the
service offline. Normal recovery accepts only v2 envelopes immediately after
startup. Legacy decoders exist only as migration input readers.

The manager database moves to version 7 so older executables refuse to open it.
Publication IDs, hashes, object incarnation IDs and authenticated manifests stay
unchanged. The new namespace avoids rewriting an immutable object in place and
keeps existing grants usable after downtime. Rollback to an older executable
requires restoring a consistent pre-upgrade database and referenced objects;
startup migration deletes the old remote copies, rather than retaining them for
compatibility.

This preserves ADR-0008's current-state publication and ADR-0010's immutable
incarnations, durable ownership and independent garbage collection. Compression
adds no delta dependencies or history. Delta compression, content-defined chunking
and discard propagation remain separate work because they require changes to
dependency ownership, manifests/read paths or the guest/journal durability model.

Read/write timings and encoded/plaintext byte counters accompany the change.
The reproducible reference measures both the codec and isolated S3/LazyDisk paths
against the previous raw format. See [storage compression](../STORAGE-COMPRESSION.md)
for the envelope, compatibility, rollout and measurement details.

# Compression of immutable disk objects

Date: 2026-10-11.

New immutable S3 disk objects compress their plaintext with zstd level 3 when it
saves bytes, then encrypt the representation with AES-256-GCM. Content identities
remain hashes of the original bytes. The versioned envelope authenticates its
codec, plaintext length and object locator, and readers bound decompression to the
authenticated manifest's extent size and the existing 4 MiB maximum.

Existing binary and JSON/base64 objects remain readable and reusable. The manager
database moves to version 6 so older executables refuse publications whose new
envelopes they cannot decode. Existing objects are not rewritten at upgrade.
Rollback requires a decoder that supports the new envelope or restoration of a
consistent pre-upgrade database and referenced objects.

This preserves ADR-0008's current-state publication and ADR-0010's immutable
incarnations, durable ownership and independent garbage collection. Compression
adds no delta dependencies or history. Delta compression, content-defined chunking
and discard propagation remain separate work because they require changes to
dependency ownership, manifests/read paths or the guest/journal durability model.

Read/write timings and encoded/plaintext byte counters accompany the change.
The reproducible reference measures both the codec and isolated S3/LazyDisk paths
against the previous raw format. See [storage compression](../STORAGE-COMPRESSION.md)
for the envelope, compatibility, rollout and measurement details.

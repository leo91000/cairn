# Cairn's pinned ext4 journal recovery fixes

Source: the crates.io `ext4-view` **1.0.0** archive, SHA-256
`0ecf6379b7cfeace5e109ccc51955afc05f5f7f2466ae0cf2697bda46afd01e6`,
the checksum previously recorded in Cargo.lock. Upstream source revision:
[`8facbf4c2986b7e2dbdef92514d696f1559a2b90`](https://github.com/nicholasbishop/ext4-view-rs/tree/8facbf4c2986b7e2dbdef92514d696f1559a2b90).
The upstream MIT and Apache licenses are retained.

Compared with that archive, this directory changes only
`src/journal/block_map.rs`, `src/journal/superblock.rs` and
`src/journal/revocation_block.rs`, updates the superblock literals in the
tests in `commit_block.rs` and `descriptor_block.rs`, adds this note, and omits
the packaging metadata
`.cargo_vcs_info.json` and the dependency's `Cargo.lock`. Every other published
file is byte-identical. `.cargo-ok` is a local Cargo cache marker, not archive data.
Cairn's Cargo.lock resolves all dependencies. This crate remains outside the
workspace so formatting preserves the upstream source.

When a transaction commits, its revoked blocks must also be removed from mappings
installed by earlier committed transactions. Previously only this transaction's
pending mappings were removed. Reused blocks could therefore be read from stale
journal metadata or data instead of their current filesystem location. This broke
migration of a valid fragmented directory in a retained VM snapshot.

The loader also follows the circular journal across its physical end. It validates
its first/start/end bounds against the journal inode and visits each usable block
at most once, excluding the journal superblock. A zero start denotes an empty
journal. Previously transactions spanning the end were silently left incomplete.

The revoke parser interprets `r_count` as total used bytes, including the
16-byte header, as specified in the
[Linux journal format](https://www.kernel.org/doc/html/latest/filesystems/ext4/journal.html#revocation-block).
It validates the table after subtracting that header and excludes the checksum.
Previously a valid full record could stop recovery before its commit, while
smaller records incorrectly included padding as extra block indices.

Revocations remain pending until their commit validates. Subsequent committed
writes can map the same block again. No source image is modified, no filesystem
is mounted, and no journal checksum or other integrity validation is weakened.

The converter regression generates synthetic journal transactions with debugfs:
an older committed write followed by a committed revoke, a full revoke record,
an uncommitted revoke, a later rewrite after revocation, and a valid descriptor/data/commit transaction
that crosses the physical end of the journal. It checks converted file content, other
workspace/output content and the unchanged source hash. Run it using:

```sh
pnpm test:backend -p cairn-installation --lib conversion_recovers_wrapped_and_revoked_journals
```

To audit provenance, unpack the archive with the checksum above and compare its
files with this directory; the differences must be limited to the list above.
Remove the patch when a pinned upstream release includes this fix, preserving
the converter regression.

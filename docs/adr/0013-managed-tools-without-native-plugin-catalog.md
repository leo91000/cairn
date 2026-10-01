# Managed tools without the native Codex plugin catalog

Date: 2026-09-30.

## Problem

Fresh production conversations spent 40–56 seconds between turn completion and
run completion. VM teardown itself took 436 ms; S3 publication started after the
run completed. Removing the final guest `sync` would hide pending writes and
allow loss of guest page-cache contents.

A real Firecracker/FUSE reproduction needs no model or account: initialize native
Codex 0.159.2, create a thread, wait twenty seconds, close Codex and exit normally.
Codex downloads a Git plugin catalog into `$CODEX_HOME/.tmp/plugins`. The guest's
writable overlay contains a 24 MB Git pack and unpacked plugin files. The journal
receives approximately 124 MB through 15,000 writes. Finalization takes 41–46 s.
Writing 128 MiB to a fixture alone does not reproduce that delay. Disabling only
`recommended_plugins` does not prevent the catalog download.

## Decision

All managed Codex app-server sessions pass `-c features.plugins=false`. This
includes account and model inspection as well as conversation execution. Leo
already supplies the selected skills and MCP configuration independently and
does not expose native Codex plugin installation or plugin RPCs.

This intentionally disables native Codex plugins, including any enabled only by
a repository's Codex configuration. It does not disable Leo's selected skills,
configured MCP servers or native app support. A future native plugin integration
must make catalog acquisition explicit and reuse its content instead of cloning
the catalog separately into every conversation disk.

Guest synchronization, durable journal acknowledgements, publication receipts,
controller protections and shutdown timeouts remain unchanged. Previously saved
catalog files are not removed from existing conversations.

## Validation

The same native VM reproduction with the override finalizes in 0.52–1.36 s and
writes 5.3–5.4 MB. The explicit guest `sync` remains active and takes 0.14–0.99 s.
These are prototype measurements, not production deployment evidence. The raw
comparisons are in [codex-plugin-shutdown-2026-09-30.json](https://github.com/leo91000/leo-agent-manager/releases/download/v0.50.6/codex-plugin-shutdown-2026-09-30.json).

The session regression verifies the centralized launch policy while retaining
caller-supplied MCP configuration. Existing conversation, resume, managed-auth,
login, account and model tests exercise the same app-server entry point.

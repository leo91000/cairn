# Persistent deliverables

Agents in Firecracker publish completed files with the built-in `leo_workspace.publish_artifact` tool. The user sees galleries beside the associated answer, plus a **Files** button for the whole conversation. Tasks expose the same files in their result and activity views.

Images support zoom, fullscreen and comparison with the previous version. Video and audio use native browser playback with HTTP byte ranges. Markdown and code have inline readers. PDFs use a lazily loaded PDF.js reader with page navigation and zoom; its worker, fonts and codecs are served locally. Other formats, including HTML/SVG and archives, remain downloadable. Executable HTML previews are a separate future feature.

## Publishing

```json
{
  "path": "/tmp/mobile.png",
  "title": "Mobile navigation",
  "key": "mobile-navigation",
  "group": "Navigation review"
}
```

Use the same `key` for subsequent revisions. Each version has an immutable identifier and download URL. Exact repeated publications within the same turn reuse the existing version. Different keys publish independent deliverables; `group` groups related files in galleries. The agent must wait for success before claiming a file is available. Run instructions advertise this tool automatically; local development execution without a Firecracker runner does not advertise VM export.

The source must be a regular file in the current run directory or `/tmp`. Symlinks, ancestor symlinks, parent traversal and special files are rejected. Export happens inside the guest. A private snapshot detects concurrent source writes and gives the transfer stable bytes. The runner selects the actual active VM and authorized run root; guest-supplied paths never select a host file.

The manager streams bytes into a private temporary file, verifies length and computes SHA-256. Remote node exports also carry `X-Leo-Artifact-Size`, copied by the manager from the node channel’s existing `length` field (the runner’s private snapshot size). This preserves the exact expected size if a streaming intermediary removes `Content-Length`; when both headers are present, they must agree. An unknown or malformed size is rejected explicitly, declared and streamed sizes remain bounded to 512 MiB, and short or oversized transfers never publish. Local exports continue using `Content-Length`; the guest snapshot and its concurrent-write checks are unchanged. It rechecks the run grant, account-independent agent permissions, active attempt and chat turn before committing metadata and its activity event together. Retries do not duplicate a committed publication. Interrupted transfers remove temporary files and never publish incomplete contents. Restart recovery continues unfinished preview jobs; an interrupted transfer is retried by publishing again, rather than resumed at a byte offset.

Originals live in `DATA_DIR/artifacts/<version-id>`, with metadata in SQLite under `artifact:<run-id>:<version-id>`. They survive VM shutdown, restart and workspace cleanup. Include this directory together with the database in backups. Publication fsyncs file contents and the containing directory before acknowledgement. Startup reconciliation removes unreferenced files older than one hour while preserving recent transfers and every committed version. Published files currently have no automatic retention or deletion policy.

Limits: **512 MiB per file**, **2 GiB and 500 versions per run/conversation**, two simultaneous manager transfers. Text previews are limited to 512 KiB. Preview jobs are serialized, have a 25-second deadline, and run with CPU, address-space and output-size limits. FFmpeg/FFprobe and Poppler are already installed in the runtime. Missing tools or unsupported codecs leave the original downloadable and mark its preview unavailable. Video playback depends on the browser's codec support; originals are not automatically transcoded.

All list, preview and download routes use the existing authenticated application session. Identifier scoping prevents retrieving another run's file via a mismatched route. Downloads set explicit media types, `nosniff`, and restrictive CSP; unrecognized files are forced downloads. Byte ranges support seeking, suffix ranges and `416` for unavailable ranges. The preview UI constructs URLs from scoped identifiers rather than trusting URLs in tool output.

## Verification

Local validation: 67 Rust tests, 182 application tests, six Chromium/WebKit journeys (including existing chats and attachments), and nine real Firecracker scenarios passed. Lint, rustfmt, Clippy, the frozen lockfile and the production frontend build passed. Screenshots are in [the UI evidence gallery](screenshots/deliverables/README.md). The PDF reader is loaded only when opened; its lazy bundle exceeds the existing 400 kB build-warning threshold.

- Rust integration tests publish through an authenticated run grant to a fixture runner, check concurrent retries and revisions, stop that runner, reopen the manager and retrieve original bytes/ranges. They also cover revocation during transfer, partial streams, guest path rules, and asynchronous thumbnail generation/fallback.
- The Firecracker smoke suite exports real binary files from a running guest and rejects cross-run requests, symlinks and host paths. Existing restart, cancellation, isolation and pool scenarios remain enabled.
- Chromium and WebKit journeys cover gallery placement, old/new comparison, persistence across live state updates and reload, mobile/dark/light layouts, document/PDF viewers, media and authenticated downloads. Browser fixtures are synthetic deliverables stored through the real database and served by the Rust backend; they do not call an inference provider.

PDF.js follows its [official display-layer examples](https://mozilla.github.io/pdf.js/examples/). Streaming delivery follows [HTTP range semantics](https://developer.mozilla.org/en-US/docs/Web/HTTP/Guides/Range_requests).

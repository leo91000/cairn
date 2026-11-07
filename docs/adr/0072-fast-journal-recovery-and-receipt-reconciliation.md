# Fast journal recovery and reconciliation of durable publication receipts

This supersedes the ordinary full-payload journal-open check described in
[ADR-0058](0058-prepared-native-project-bases.md). Its explicit verification of
shared prepared bases remains required.

## Context

An inactive native conversation repeatedly reopened a large journal and scanned
all payloads while other conversations waited for the same global registry
mutex. Storage status and publication acknowledgements could exceed their
control deadlines. Health heartbeats continued, so node availability did not
reveal this storage stall.

A completed conversation also retained a synchronization error in the
manager although its node already stored the matching durable publication
receipt and had no remaining journal segments. The ordinary monitor selected
active runs and at most two retained guests; this completed native disk was no
longer selected.

## Decision

Ordinary journal recovery authenticates frame headers, validates sequence,
generation, extent and file-length boundaries, and preserves interrupted-tail
recovery and durability barriers. It indexes payload locations without reading
their contents. Every payload is still authenticated by `Record::read` before
its bytes are returned to an agent or used to reconstruct a publication. A
recovered record has no trusted zero-range hint; a subsequent discard remains
conservative. Disk and publication formats do not change.

The explicit `verify_sealed` operation continues to verify every payload in a
shared prepared base. A changed base or a process restart must still invalidate
a damaged base before a conversation can claim it. Ordinary lazy recovery and
prepared-base admission have separate verification entry points.

The registry mutex protects directory lookup only. Opening/replacing one disk
uses its own entry lock. An opener already in progress returns a retry condition
without occupying another bounded I/O permit while waiting. Unrelated disk
opens and CPU-control probes do not wait for that disk's filesystem work.

The manager also monitors inactive conversations with unpublished work or
unfinished publication requests, with four concurrent status requests. A node
reports its durable completed generation alongside the published receipt. Only
a matching, fully published completed generation with no dirty bytes can
acknowledge a previously observed request. Reconciliation never acknowledges a
newer request that appeared while the status request was in flight, and older
nodes lacking completion evidence retain their pending requests.

## Validation

Regression coverage checks corrupt payload rejection on actual reads and
publication, recovery without reading overwritten payloads, header corruption
and interrupted-tail recovery, slow-open isolation with sixteen same-disk
waiters, damaged prepared-base rejection, inactive receipt reconciliation,
newer-request races and missing/newer completion boundaries.

The [journal recovery guide](../JOURNAL-RECOVERY.md) describes the reproducible
measurement harness. Quantitative results, exact reference/candidate identities
and host conditions belong in the PR description; raw evidence stays in
temporary files or CI artifacts.

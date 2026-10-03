# One journal reader for a stopped-disk publication transfer

Date: 2026-10-03.

## Problem

The manager requested missing blocks in batches of eight. Each response owned
its journal reader; a stopped disk had no mounted volume retaining that reader
between responses. The next batch therefore reopened and checked the complete
journal. The Android qualification recorded 143 opens and 163.237 s of journal
checking across its publication lifecycle. This diagnostic is not a matched
before/after production benchmark.

## Decision

Request all missing hashes through one authenticated
`POST /snapshots/{id}/publication` response. Its reader owns the stopped disk
lease and journal until EOF or cancellation. It streams one 4 MiB block at a
time with backpressure; payload memory does not grow with disk size. The hash
inventory is bounded by the supported 1 TiB disk limit, and manifest membership
is indexed once before streaming.

The journal is opened and checked once for the block transfer. Initial capture
and final receipt remain separate operations; this change does not claim one
open across those three phases. Each record and reconstructed block still has
its integrity checked when read. Later writes cannot replace the captured
generation. A disconnected stopped reader releases its lease and cancels remote
reads; disconnecting a response from a mounted disk does not cancel the VM.

No reader survives in a global cache after its response. S3 dependencies,
manifest commit, publication receipt, pending-generation ownership and disk
movement fences retain their existing ordering. Unsupported endpoints fall back
once to eight-block responses, then to single-block reads on older nodes.

A healthy full-disk transfer can exceed the old two-minute response deadline.
Individual block reads stay bounded; relay uploads expire after two minutes
without body progress, with a separately bounded final response. A connection
that stops making progress cannot retain a reader indefinitely.

## Measurements and validation

Three alternating trials on the same 512 MiB stopped journal (128 unique blocks,
warm host page cache, development build, no S3):

| Median | Eight-block batches | Publication response |
| --- | ---: | ---: |
| Journal opens per transfer | 16 | 1 |
| Opening/checking time | 3,357.5 ms | 216.1 ms |
| Complete block transfer and integrity checks | 3,915.4 ms | 758.5 ms |

The measured transfer reduction is 80.6%. This isolates repeated journal
checking; it does not estimate production upload speed. Raw evidence remains
outside Git and belongs in release assets when this change is qualified.

Reproduce with the isolated backend launcher, alone:

```sh
pnpm test:backend --lib stopped_publication_reader_benchmark -- \
  --ignored --nocapture --test-threads=1
```

Regression tests cover a response larger than the legacy batch, reader identity
and cancellation, sealed-generation reads with newer writes present, malformed
inventories, truncated/excess responses, old-controller fallback, progress beyond
the previous upload deadline and rejection of a stalled upload. They also detect
payload corruption introduced after the reader's initial journal check. The full
backend and Rust/web checks pass, including Clippy with the optional ublk
prototype; loopback S3 tests verify encrypted recovery, repeated disk movement,
and coalesced publication while a newer attempt writes.

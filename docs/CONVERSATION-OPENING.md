# Conversation opening

Opening a conversation from the Fil used to keep the previous conversation on screen
behind a small « Opening conversation… » pill, show a saved copy that the server then
replaced, flash « Loading history… » while a short window refilled, and make the
installation rebuild the whole conversation list for every page of every open stream.
This change shows only the selected conversation, opens it in one piece, and reduces what
the installation reads to open, page and follow it. Measurements against main `293117d0`
follow; raw data is in [measurements/conversation-opening](measurements/conversation-opening).

## Changes and preserved behavior

### Installation

- A conversation's stream carries the conversation list only when the client asks for it
  (`list=1`). A browser tab asks, so it keeps a single stream; Android follows the list on
  its own stream and no longer receives it with each conversation. The list stream
  (`/chats/stream`) keeps its payload and rebuilds the list at most every 250 ms after
  commits. Streams that carry the list share one rebuild per commit and account (cached by
  store revision: a read at a revision sees at least that commit). Deleted conversations are
  skipped before their view is built. The 15-second idle recovery check invalidates the
  account's list cache so external maintenance writes that bypass the store still appear.
- The opening page is sent without the 25 ms coalescing delay meant for live updates.
- A resume past more than one page of events first reads the newest window. When it
  reaches back to the saved cursor, which a backlog of replaced text snapshots usually does,
  the reader receives only what it lacks and keeps its history and position. Otherwise the
  behavior is unchanged: the recent window for a reader following the latest output, the
  replay for a reader of older messages.
- History pages read the run and its events only, not the stream metadata.
- Reads share one queue across four SQLite connections instead of alternating between two,
  so a slow page no longer delays the next read queued behind it.
- New image attachments record their displayed size (the camera orientation applied),
  read from the header at upload. These are optional fields of the attachment record.

### Web

- Switching shows the selected conversation only. A saved copy that the Fil shows as
  current (same finished turn, no work in progress, no later change) opens in the same task
  as the click; otherwise a centred loader, revealed after 200 ms, stands in until the
  stream's first snapshot. The saved copy then only resumes the stream from its cursor, and
  still shows when there is no connection. The header names the conversation from the Fil.
- « Updating… » appears only when revalidating a saved copy takes more than a second.
- The reader relies on browser scroll anchoring and pins the end while following. Only a
  reader scrolling up pauses following; positions the feed sets and anchoring adjustments do
  not. The previous manual offset remains for a page arriving at the very top, where
  browsers do not anchor, and for engines without anchoring. `column-reverse` was evaluated
  and not used: with anchoring, both directions keep the reader's text in place in Chromium,
  WebKit and Firefox, and the normal direction keeps scroll offsets, saved reading positions
  and their tests unchanged.
- Opening fills the view and one screen above it; the three-screen buffer of older pages
  loads once the browser is idle. « Loading history… » appears only for a reader waiting at
  the start.
- The Fil follows the list on the conversation's stream on the conversation page and on the
  list stream elsewhere, instead of polling every 30 seconds; the Fil page has no stream of
  its own. A conversation that fails terminally or uses the finite relay fallback releases
  list delivery to the shell, so the Fil remains available. A retry takes back its stream
  slot before connecting, preserving the per-account capacity. A link by identifier to a
  conversation the Fil knows opens under its slug, so both
  forms share one saved copy.
- The finite HTTP read for installations on the first relay protocol runs only when the
  stream fails before its first batch, instead of beside every uncached opening.
- The history cache keeps serialized entries in memory (no re-serialization of every entry
  on each save) and evicts through an IndexedDB index of ages and sizes instead of reading
  every saved conversation. Saving while output streams happens at most every 2 seconds, and
  when the page is hidden.
- A single sent image reserves its box from the recorded size; older uploads still size
  themselves when they load.

### Android

The selected conversation waits for its stream when its saved copy is outdated, with the
same rule as the web; pager previews keep showing their saved copies and recheck their
freshness whenever selected, including pages retained after a previous visit. « Actualisation… »
appears only after a second. See [web parity](../apps/android/docs/WEB-PARITY.md).

## Compatibility

- Streams accept the new `list=1` parameter; requests without it get conversation streams
  without the list. A browser tab still running an earlier bundle refreshes its Fil every
  30 seconds until reloaded.
- Attachment records gain optional `width` and `height`. Earlier releases ignore them and
  this release reads records without them. The installation data epoch is unchanged.
- The browser history cache moves to the `cairn-history-v2` IndexedDB database and removes
  `cairn-history-v1`. Saved copies are downloaded again once; an earlier build that opens
  the old database starts from an empty one.
- Relay protocol and stream capacity are unchanged: a tab keeps one stream on the
  conversation page, as before, and one elsewhere instead of polling.

## Installation measurements

`profile_conversation_opening` seeds 80 active and 200 deleted conversations, one of them
with 4,021 events: 40 turns of 25 tool calls and an answer streamed in 41 snapshots, the
last one in 301. Each sample opens a new HTTP stream; delivery is a write followed by its
batch on a following reader while six conversation streams carrying the list and six list
streams stay open. Local release builds, same host, two alternating runs per side; milliseconds.

| Scenario (p50, run 1 / run 2) | Before | After |
| --- | ---: | ---: |
| Conversation opening with the list (web) | 38.4 / 38.4 | 9.3 / 8.8 |
| Conversation opening without it (Android) | 39.0 / 39.3 | 9.6 / 8.1 |
| List opening | 33.8 / 33.7 | 3.8 / 3.6 |
| Older history page | 8.1 / 7.5 | 4.3 / 4.6 |
| Resume after the last answer streamed | 38.3 / 38.1 | 7.7 / 8.8 |
| Delivery with 12 other open streams | 51.9 / 51.6 | 29.3 / 29.2 |

The resume is a reset to the newest 100 events (203 KB) before, and the 54 events the
reader lacks (108 KB) after, keeping its history. Delivery includes the intentional 25 ms
coalescing; process CPU for its 60 writes falls from 3.1 s to 0.65 s. The first batch
without the list falls from 203 KB to 165 KB; with it, it is unchanged.

## Web measurements

`opening-measurement.spec.ts` opens conversations from the Fil in Chromium at 1440 × 900,
with 150 ms added to every conversation stream and history request to stand for a relay.
Samples are taken after each frame is painted, during 2.5 s after the click. Medians of three
rounds; « previous » counts frames showing the conversation just left, « outdated » frames
showing the selected conversation without the output the reader came for.

| Scenario | First content (ms) | Previous frames | Outdated frames | History banner frames | History requests |
| --- | ---: | ---: | ---: | ---: | ---: |
| First open of a long conversation | 271 → 258 | 13 → 0 | 0 → 0 | 0 → 0 | 2 → 1 |
| Back to a saved conversation | 263 → 58 | 13 → 0 | 0 → 0 | 0 → 0 | 1 → 0 |
| Saved long conversation | 67 → 70 | 0 → 0 | 0 → 0 | 0 → 0 | 0 → 0 |
| First open, folded tool calls | 277 → 237 | 13 → 0 | 0 → 0 | 11 → 0 | 4 → 3 |
| Outdated saved conversation | 272 → 264 | 0 → 0 | 13 → 0 | 0 → 0 | 0 → 0 |

The loader takes the place of the previous conversation for 10–12 frames, transparent for
the first 200 ms. Times to first content that differ by less than about 25 ms are within the
variation between runs. Main reopened the first conversation from the network: it was saved under
the link it was opened by (its identifier), and the Fil links to its slug. No scenario moved
the text under the reader after it first showed, and the layout-shift score was 0 on both
sides.

## History cache

`scripts/measure-history-cache.mjs 293117d0` adds a write with eleven other conversations
saved. Same workload as [the cache measurements](CONVERSATION-CACHE-MEASUREMENTS.md):
1,250 events (719,346 bytes), medians of 30 samples, Chromium 153, shared Linux host.

| Version | Write | Warm read | Cold read | Write, 12 saved | Renderer CPU |
| --- | ---: | ---: | ---: | ---: | ---: |
| Before | 5.5 ms | 1.3 ms | 4.6 ms | 18.3 ms | 1,120 ms |
| After | 2.1 ms | 0.4 ms | 2.8 ms | 2 ms | 260 ms |

## Reproduction

```sh
CAIRN_PROFILE_OPENING=1 cargo test --release -p cairn-installation --test live \
  profile_conversation_opening -- --nocapture
CAIRN_MEASURE_OPENING=1 pnpm exec playwright test --project=measure-opening
node scripts/measure-history-cache.mjs 293117d0
```

For a reference, copy the profile and the browser measurement into a checkout of it; the
browser measurement takes `CAIRN_TEST_BINARY` and `CAIRN_BEACON_WEB_DIR` for the reference
installation binary and web build. Measured on 2026-10-19: Linux x86-64, Intel Xeon
(12 logical CPUs reported to Node), 31 GiB RAM, Node 24.21.0, Chromium 153.0.8010.12,
Rust 1.97.1, on a shared development VM.

## Limits

These are local measurements, not production network latency. The added 150 ms stands for
a relay; real relays vary. Neither version moved the text under the reader after it first
showed in these scenarios: the jumps in the original report came from outdated copies
replaced in place and from history refills, measured here as outdated frames and banner
frames. Older pages still skip replaced text snapshots with the same query: once the stream
metadata left it, a page takes about 4.5 ms on this conversation, which does not justify a
stored « replaced » flag and its schema change. The web still renders the whole loaded
history; it adds no virtualization.

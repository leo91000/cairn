# Conversation opening

Opening from the Fil shows only the selected conversation. A current saved copy
opens immediately; otherwise the reader waits for the stream’s first snapshot.

## Reader and stream behavior

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
  its own. A conversation that fails terminally releases list delivery to the shell,
  so the Fil remains available. A retry takes back its stream
  slot before connecting, preserving the per-account capacity. A link by identifier to a
  conversation the Fil knows opens under its slug, so both
  forms share one saved copy.
- The conversation opens from its stream alone; there is no finite compatibility read.
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

## Current client contract

Streams accept `list=1` to include the conversation list. The web and Android apps
use the current stream protocol and installation API; old bundle polling and finite
relay reads are no longer supported. The browser stores saved conversations in
`cairn-history-v2`, with no migration or cleanup of earlier cache databases. Images
may carry dimensions; an image without dimensions sizes itself when loaded.

## History cache

Web (IndexedDB) and Android (encrypted atomic files) retain loaded history pages
within the existing byte budget instead of discarding everything except the last
200 events. Reopening displays saved output immediately when it is current or
the connection is unavailable; otherwise the reader waits for the first stream
snapshot under the freshness rule above. Resumption uses the validated cursor. The web client skips its initial full HTTP history read
when a saved snapshot exists. Revalidation preserves the earliest loaded page;
a history reset replaces that boundary.

The cache remains session-scoped, expires after seven days, and holds at most
12 conversations / 20 MiB. The history suffix has a 3 MiB budget including state
and headroom; web accounts conservatively in UTF-16 bytes, Android in UTF-8 bytes.
An evicted prefix remains available from server pagination. Evicting a prefix also
invalidates its reading position. Logout and existing revision/deletion handling
still invalidate saved conversations.

Activity groups contain at most 30 visible folded calls. Global event-ID gaps and
hidden events do not count toward that limit. Existing full groups keep their
membership when older pages arrive or new calls append. Updates to one call still
replace its previous state. Consecutive successful reads remain grouped within that limit;
failures, running calls, messages and subagent teams retain their presentation.

### Android persistence

`HistoryCacheWriter` saves sequentially with one latest pending snapshot per
reader, so a disk write does not block publication of received output. Exit and
cancellation drain the final pending revision. A reopening reader can restore a
queued snapshot before it reaches disk. Shared offer identities, generations and
cursor ordering reject invalidated or superseded work across readers; automatic
cleanup preserves newer pending offers. Position validation uses the same bounded
history representation and cursor order as persistence, and clears anchors whose
retained boundary changed. Warm LRU reads reuse estimated sizes. Cold reads and
explicit flushes can still wait for the filesystem mutex.

## Rendering

Web timestamps cache the exact native Date labels within a reader, invalidating
on locale, time zone, calendar, numbering or hour-cycle context changes. Removed
messages do not accumulate in the cache. Markdown still parses and sanitizes the
complete document on every content change, so references, raw HTML and incomplete
syntax retain their existing semantics. A bounded component-owned cache reuses
unchanged code highlighting. Sanitized nodes are moved directly into the reader;
equal top-level prefixes and suffixes retain selection, link focus and table scroll.
Documents with raw HTML tokens keep the historical serialization/reparse after
transformations, preserving SVG namespaces and raw-text container semantics.

Android skips activity presentation for events already classified as messages
while scanning for recovered connection notices. User turn boundaries still clear
pending notices; agent messages still count as recovery. Timeline folding, wire
IDs, display keys, lazy rendering and native Markdown layout remain unchanged.

The web renders the whole loaded history. Full-document Markdown parsing remains
linear in answer size; a giant native paragraph requires whole-paragraph layout.

# Conversation opening

Opening from the Fil shows only the selected conversation. A saved copy opens
immediately, saying it is updating when the Fil shows it is outdated; a conversation
never saved opens on an outline until the stream's first snapshot.

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
- Streams accept `compact=1`, which both apps send. A compact stream sends the list whole
  in its connection's first page only. Afterwards `chatChanges` carries the conversations
  whose summary changed (`upserts`), those that left the list (`removed`) and, when the
  order changed beyond removals, every listed id (`order`); a batch's `state` omits the
  unchanged list. The conversation's `chat` omits `run`, which is the state's own `run`.
  Without `compact=1` streams keep their previous payloads.
- Trashing a conversation changes `cacheRevision` only on streams without `compact=1`,
  whose older clients drop every saved conversation on that change. Compact streams carry
  a separate history revision that trashing leaves unchanged; their clients drop the saved
  copy of each conversation that leaves the server's list instead.
- The opening page is sent without the 25 ms coalescing delay meant for live updates.
- A resume past more than one page of events first reads the newest window. When it
  reaches back to the saved cursor, which a backlog of replaced text snapshots usually does,
  the reader receives only what it lacks and keeps its history and position. Otherwise the
  behavior is unchanged: the recent window for a reader following the latest output, the
  replay for a reader of older messages.
- History pages read the run and its events only, not the stream metadata. Their
  event arrays use a 256 KiB gzip budget and at most 1,000 events, with a separate
  4 MiB decoded ceiling. One oversized event remains readable whole so cursors
  always advance. Backwards candidates are measured in chronological wire order;
  unused candidates remain reachable through the next cursor. Candidates are
  serialized once and reused for bulk gzip probes; probes at least double their
  decoded input, and at most two proportional cuts precede a single-event
  fallback for highly uneven rows. Pages can therefore leave budget unused.
  HTTP still compresses the final response separately. Live SSE initial/replay windows and forward
  batches keep their existing 100-event and raw-byte limits. Public MCP and agent-tool responses retain
  their bounded previews and chunked full-content reads.
- Relay requests preserve compression negotiation and encoding headers. Web and
  Android direct channels request gzip and decode it explicitly, with the same
  8 MB decoded-response bound as their wire bodies. Older browsers without
  DecompressionStream request identity. Direct streaming paths, including shared
  artifact files, request identity as well, so file contents and JSON error
  responses remain readable through streaming frames. A direct client that can decode
  gzip asks for compressed event streams with `x-cairn-stream-encoding: gzip`; the
  installation then compresses the whole SSE response in one gzip stream and flushes
  every chunk, so each event decodes on arrival while repeated summaries share the
  compression history. Files, errors and streams without that header stay identity.
  Relay SSE streams remain uncompressed.
- Reads share one queue across four SQLite connections instead of alternating between two,
  so a slow page no longer delays the next read queued behind it.
- New image attachments record their displayed size (the camera orientation applied),
  read from the header at upload. These are optional fields of the attachment record.

### Web

- Switching shows the selected conversation only. A saved copy opens in the same task as
  the click when it was read in this page; hovering, focusing or pressing a Fil row reads
  it from storage first. A copy the Fil shows as outdated (another turn, work in progress
  or a later change) shows « Updating… » at once until the stream completes it; a current
  one shows it only when revalidating takes more than a second. A conversation never saved
  opens on an outline of a conversation, revealed after 200 ms, under the header the Fil
  names. The saved copy resumes the stream from its cursor and still shows offline.
- The reader uses browser scroll anchoring and pins the end while following. Only a
  reader scrolling up pauses following; positions the feed sets and anchoring adjustments do
  not. Before prepending history it records a visible row's pixel offset, then corrects
  any movement the browser did not absorb. This covers large pages that suppress
  native anchoring, the very top and engines without anchoring. `column-reverse` was evaluated
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

The selected conversation shows its saved copy at once, with the same rule as the web: an
outdated copy says « Actualisation… » at once, a current one only after a second. Pager
previews recheck their freshness whenever selected, including pages retained after a
previous visit. A conversation never saved opens on the same outline as the web.
See [web parity](../apps/android/docs/WEB-PARITY.md).

## Current client contract

Streams accept `list=1` to include the conversation list and `compact=1` for list
changes and a chat without its duplicated run. The web and Android apps
use the current stream protocol and installation API; old bundle polling and finite
relay reads are no longer supported. The browser stores saved conversations in
`cairn-history-v3`, with no migration or cleanup of earlier cache databases; a tab of an
older bundle keeps `cairn-history-v2`, so neither evicts nor clears the other's copies. Images
may carry dimensions; an image without dimensions sizes itself when loaded.

## History cache

Web (IndexedDB) and Android (encrypted atomic files) retain loaded history pages
within the existing byte budget instead of discarding everything except the last
200 events. Reopening displays saved output immediately; an outdated copy says it
is updating under the freshness rule above. Resumption uses the validated cursor. The web client skips its initial full HTTP history read
when a saved snapshot exists. Revalidation preserves the earliest loaded page;
a history reset replaces that boundary.

The cache remains session-scoped, expires after seven days, and holds at most
60 conversations / 20 MiB. A saved conversation holds its own history only: neither
the conversation list nor the chat's duplicated run. The web saves the list once,
apart (at most every 2 seconds and when the page is hidden), and shows it until a
stream delivers the server's list; Android's list stream keeps its own saved copy. The history suffix has a 3 MiB budget including state
and headroom; web accounts conservatively in UTF-16 bytes, Android in UTF-8 bytes.
An evicted prefix remains available from server pagination. Evicting a prefix also
invalidates its reading position. Logout and a changed cache revision still invalidate
every saved conversation. A conversation that leaves the server's list (deleted, or no
longer shared) loses its own saved copy on web and Android, and every saved run log goes
with it, since a run does not name its conversation; the web also compares the server's
first list with the list it saved last. Signing out of the web also deletes the earlier
`cairn-history-v2` store.

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

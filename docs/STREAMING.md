# Resumable live conversations

Chats, the chat list, and task activity use authenticated SSE connections. Opening
another tab or browser creates an independent subscriber; it never starts another
agent. Closing a viewer never cancels a run.

## Delivery and recovery

- `GET /api/chats/stream` follows the conversation list.
- `GET /api/chats/:id/stream` follows one conversation, including its list, queue,
  questions, run status, events, and deliverables.
- `GET /api/runs/:id/stream` follows task activity and deliverables.

The `batch` event carries `{events, state?, reset, more}`. Its SSE `id` is the last
persisted event delivered. `Last-Event-ID` takes precedence over `?after=`. A fresh
page replays from zero; a disconnected viewer resumes after its last accepted
batch. State is sent initially and whenever it changes, even when no new text
event exists. Completed runs remain subscribed so follow-ups and finished
previews appear without reopening the page.

The database actor emits a notification after each write/transaction finishes,
including when its caller disconnects. Subscribing **before** reading the initial
SQLite snapshot avoids a gap between history and live delivery. Notifications are
wake-ups, not the event store: all replay reads committed database rows in order.
Rolled-back events never become visible. Metadata and events in each page use one
read transaction.

Subscribers pull bounded pages (100 events, approximately 256 KiB of stored text
and payload; one larger existing event may exceed that budget). A slow client
does not accumulate a private event queue or block the writer. Rapid updates are
grouped with a 25 ms delivery interval. A cursor beyond the available history or
a changed conversation run triggers a reset and replay.

Text uses a complete baseline followed by append-only `payload.item.delta`
updates. Every reconnect establishes its own baseline; replacements send a new
complete value. This encoding happens **after redaction**, only on the SSE wire.
Persisted snapshots and the existing paginated REST event API remain compatible.
The client folds intermediate text updates in place, while keeping its replay
cursor separate from the displayed message count.

## Connection lifecycle

The shared Vue composable owns a native EventSource and disposes it on route
changes/unmount. It acknowledges a batch only after applying it, ignores
callbacks from replaced connections, and deduplicates already applied event IDs.
Reconnect delay grows from 500 ms to 15 seconds. Offline clients wait for network
recovery; returning to a visible page reconnects from the retained cursor.

Named keep-alives arrive every 10 seconds. A 45-second client watchdog recovers a
silently stalled connection. Sessions are checked on each stream read and at
least every 15 seconds while idle; revoked sessions stop receiving new data.
The idle check also notices external maintenance writes that bypass the actor.
Authentication/permission/not-found failures stop retries after an HTTP access
check. The viewer shows a small connection notice without discarding its history.

Axum supplies SSE framing and keep-alives. API responses are not cached, SSE is
excluded from compression, and `X-Accel-Buffering: no` requests immediate proxy
forwarding. A reverse proxy must preserve streaming and allow keep-alives.

## Verification

`crates/installation/tests/live.rs` exercises real HTTP streams: concurrent clients, paginated
replay, writes during replay, slow readers, rollback, cursor precedence/reset,
server restart, session revocation, invalid requests, metadata-only updates, and
Unicode text baselines/deltas. The local 25-sample write-to-HTTP measurement was
26 ms median and 27 ms p95; it excludes provider generation and Internet latency.

`tests/e2e/live.spec.ts` runs in Chromium and WebKit with separate authenticated
browser contexts: progressive text, offline/online recovery, mid-answer refresh,
server restart, a late third viewer, route changes, and mobile screenshot/scroll
checks. Existing chat, question, deliverable and task journeys exercise the same
stream. `apps/web/tests/live-connection.test.ts` covers heartbeat/watchdog behavior,
backoff, malformed batches, failed application, and cleanup. `apps/web/tests/live-events.test.ts`
checks deduplication, Unicode reconstruction and bounded intermediate snapshots.

No inference provider is used by these tests. The guest-to-runner log transport
is unchanged; its idle read interval remains 100 ms. This change removes browser
polling, not model generation time or VM startup time.

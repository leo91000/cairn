# C1: ownership of a web send attempt

Design experiment for [#265](https://github.com/leo91000/cairn/issues/265), part of
[#240](https://github.com/leo91000/cairn/issues/240). Reference: current main
`87630c75459b262ad913df6ad1e209a19b94ae02` (F1, #239 and #281 included).
The prototype lives only in
[`scripts/tests/chat-send-ownership.test.ts`](../scripts/tests/chat-send-ownership.test.ts).
It is an injected fake transport experiment, **not production coverage**.

## Decision

Keep F1's existing submission selection in `Chats.vue`. Do not introduce a
production owner in this change. The candidate can own created-chat retention,
upload receipts and retry identity, but that alone does not demonstrate a safe
integration with the screen's current timing. A behavior-preserving wrapper still
needs the screen's lifecycle resets, late selection decisions and origin guards.
The immutable candidate avoids those late reads by changing semantics. That is
insufficient gain for an extraction claimed to preserve behavior.

The small `submissionFor(previous, target, content, nextId)` alternative in the
experiment expresses F1's identity rule with an injected ID allocator. Moving
only that comparison into production would leave every ownership problem below
in place, so there is no gratuitous production helper extraction either.
This is a measured stop decision, like C6, rather than a claim that ownership
cannot improve the design. Reopen with a specified lifecycle policy and a real
screen integration witness for each boundary below.

## What the production code actually does

Read together: [`Chats.vue`](../apps/web/src/views/Chats.vue),
[`chat-delivery.ts`](../apps/web/src/chat-delivery.ts) and
[`chats.rs`](../crates/installation/src/chats.rs). The sequence below refers to
the reference commit, not an idealized fake caller.

1. `send` captures `originalDraft`, trimmed text, raw route `origin`, `mode` and
   `direct` **before** any await. `busy` prevents a second send through this view.
2. It chooses `detail ?? createdChat ?? await POST /chats`; that new-chat request
   uses the current agent/project. Only a received create response can be retained.
   `createdChat = chat` happens after that await, without an origin check.
3. It computes pending uploads from the then-current attachments/files and an
   unscoped `Set<attachmentId>`. Uploads run with `Promise.allSettled`. A successful
   callback mutates the attachment record and adds its ID to `uploaded`; failures
   retain the draft and prevent message submission. Reused records without a
   local file are not uploaded. Removing/clearing an attachment deletes its receipt.
4. **After** uploads settle, it reads attachment IDs, editing target, provider,
   model and reasoning from the current screen. Text and mode remain the original
   values. It keys retry identity by resolved chat ID, editing target and all wire
   content. F1 already handles identical edits to different queued messages.
5. `direct` determines whether to create `outgoing`; it is a presentation prediction,
   not the server's queue decision. The request uses the captured chat but the
   current editing target. Only **after** HTTP success does the raw route-origin
   guard protect composer clearing/navigation. It does not prevent a message POST
   after switching during upload, or guard the catch block's error/outgoing reset.
6. Success clears submission, editing and attachments, conditionally clears the
   unchanged draft, then navigates/focuses. A send error clears outgoing and sets
   an error but preserves retry identity. `createdChat` can survive an uncertain
   first message until a route change or explicit new conversation.

`newConversation` and `watch(chatId)` each reset `createdChat` and `submission`.
The explicit action matters when `/chats` stays `/chats`: no route watcher fires.
The watcher also clears attachments, resolves a created chat's slug to its UUID
and preserves the first optimistic bubble only for that destination.
An upload completing after a clear can add an old ID back into `uploaded`.
IDs are random in the UI; the absence of chat scope is still an ownership gap,
not evidence that production users routinely collide attachment IDs.

## Two acknowledgements, two lifetimes

`chats.rs::send` persists a queued message and returns an existing same-ID,
same-content message on retry. A globally reused message ID with a different
target/content can conflict; the client's contextual identity remains necessary.
An edited queued message uses `edit`, a separate operation; the fake transport
does not reproduce its status validation or merge semantics.

End the **retry identity** on successful message HTTP response. An ambiguous
failure retains it. This does not establish that the agent has read the message.
`chats.rs::acknowledge` marks it delivered and writes `chat.user` later.
`Chats.vue` watches that event to clear outgoing; `chatDelivery` reconciles the
optimistic record with metadata/events and respects server queue decisions.
Keeping identity until `chat.user` could treat an intentional second identical
message as a retry while a paused queue waits indefinitely. Clearing outgoing at
HTTP acceptance could remove feedback before agent delivery.

Keep outgoing with the screen/delivery presentation, outside this attempt owner.
Its lifecycle includes origin changes, initial slug navigation, transport errors
and stream acknowledgement. Owning it would require the live stream and queue
policy as well as the submission transport. This decision leaves the contracts,
question withdrawal behavior from #239 and agent permissions from #281 intact.

## Discriminating experiment

The candidate has `enter(chatId?)` and `send(content, localFiles, editing?)`;
the create/upload/submit transports and ID allocator are injected. A context
object owns created chat, upload receipts and submission identity. `enter`
replaces that object, including on the same route. Completion mutates only its
captured context; a detached upload does not submit a message. Inputs are copied
at send entry. Concurrent sends are excluded by the existing caller's busy gate;
the prototype is not a concurrency-safe public service.

| Executable probe | Observable result / design distinction |
| --- | --- |
| Save succeeds, response is lost, retry | One chat creation, one upload, two identical submissions, one fake stored ID. The pure selector covers identity only; the owner retains chat and receipt. |
| New conversation after failure on the same route | Explicit `enter()` creates a different chat and message ID and uploads into that chat. A route string alone is insufficient. |
| Switch while upload is held | Old receipt stays detached; no old POST; destination uploads the same file ID again. This cancellation policy differs from the actual post-upload origin check. |
| Partial upload failure | No submit until all uploads succeed; only the failed file retries. A reused record without a local file adds no upload. |
| Editing and steer identity | The pure selector reuses unchanged content/target and changes ID for each target or content dimension, including edited message and queue/steer. No fake backend edit guarantees are inferred. |
| HTTP success without `chat.user` | Next identical intentional send has a fresh ID; creating a chat is not repeated. Delivery presentation is a separate lifetime. |
| Mutate inputs during held upload | Prototype sends captured values. Actual view captures text/mode early but reads selection/attachments/edit late. This is an integration decision, not an equivalence proof. |
| Late failed POST after context replacement | Old promise still rejects but cannot overwrite the new context's identity. The caller still needs an origin guard before rendering that error. |

Measured ownership inventory (source inspection plus the probes, not a runtime
performance benchmark): three retained stores move into the candidate; the two
screen lifecycle sites still have to signal `enter`. Draft/attachment presentation
cleanup, unchanged-draft comparison, origin/error handling, optimistic eligibility,
slug-aware navigation and focus remain screen decisions. The immutable candidate
also requires agreed capture and detachment semantics. Passing current mutable
refs/getters to preserve the late reads returns those decisions to the caller.
The count of sites/fields is descriptive, not the acceptance criterion; the
unresolved timing and observable policy are what prevent a production extraction.

## Existing witnesses and limits

F1's real browser cases in [`tests/e2e/chats.spec.ts`](../tests/e2e/chats.spec.ts)
cover lost HTTP acknowledgement, explicit new conversation on the same route,
abandoned-error clearing and edits to different queued messages. They exercise the
actual view and backend. The unchanged 14 cases in
[`chat-delivery.test.ts`](../apps/web/tests/chat-delivery.test.ts) cover stream/
metadata ordering, optimistic deduplication, server queue decisions, steering,
paused/failed chats and private question answers. The attachment browser journey
in [`chat-attachments.spec.ts`](../tests/e2e/chat-attachments.spec.ts) covers reuse,
queue editing and steer with real attachment records. These sources support the
production analysis; they are not replaced by the eight fake-API probes.

Neither the existing cases nor this isolated prototype demonstrate production
navigation-during-upload safety. The fake stored-ID set models only the lost
response discriminator; it is not a server idempotency, persistence or network
latency test. A lost **create-chat** response remains ambiguous: no chat ID is
available to retain. Reload durability, request cancellation, account/installation
switches, agent/project creation selection and upload progress rendering are not
implemented here. File IDs are assumed immutable; receipt eviction when a draft
item is removed is also absent from the sketch (the view does delete that receipt).

This change touches no production/UI code or persisted format. Android is outside
C1's scope; `ChatJourneyTest.kt` and `ImagePasteCases.kt` were examined as existing
parity witnesses, not executed or changed. A Claude UI pass, redesign, visual
artifacts and a parity entry are not applicable to this isolated experiment.
Agent-error recovery remains governed by
[bounded agent-error recovery](adr/0032-bounded-agent-error-recovery.md), not this
HTTP submission retry sketch.

Reproduce the local experiment with pinned dependencies:

```sh
pnpm exec vitest run scripts/tests/chat-send-ownership.test.ts apps/web/tests/chat-delivery.test.ts --maxWorkers=2
```

Record actual local validation, independent review head and CI results in the PR
and ticket checkpoint. A passing prototype must never be reported as production
browser/device evidence or as qualification for a future owner integration.

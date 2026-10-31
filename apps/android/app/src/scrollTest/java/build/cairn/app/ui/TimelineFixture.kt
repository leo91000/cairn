package build.cairn.app.ui

import build.cairn.app.data.ChatWakeup
import build.cairn.app.data.RunEvent
import build.cairn.app.data.TimelineCard
import build.cairn.contracts.TimelineContent
import build.cairn.contracts.TimelineNotice
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.decodeFromJsonElement
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.longOrNull

/**
 * Test fixture for families the timeline does not convert yet (plans, subagents, waits, tools): it
 * stands in for the server with the cards those tests need as boundaries — messages and plain
 * lifecycle notices. The server's own folding is qualified by the Rust reducer tests and the shared
 * `timeline-corpus.json`, never re-derived here.
 */
internal fun withTimeline(input: List<RunEvent>): Pair<List<RunEvent>, List<TimelineCard>> {
    val events = mutableListOf<RunEvent>()
    val cards = linkedMapOf<String, TimelineCard>()
    var turn = 0L
    fun card(event: RunEvent, id: String, content: TimelineContent) =
        TimelineCard(
            attempt = 0,
            content = content,
            createdAt = event.createdAt,
            id = id,
            position = event.id,
            revision = event.id,
            runId = event.runId,
            source = event.id,
            turn = turn,
            updatedAt = event.createdAt,
        )
    fun notice(event: RunEvent, notice: TimelineNotice) {
        cards["notice:${event.id}"] = card(event, "notice:${event.id}", TimelineContent.C(notice))
    }
    val covered =
        setOf(
            "chat.user",
            "status",
            "diagnostic",
            "error",
            "thread.started",
            "runner.failed",
        )
    for (event in input) {
        val payload = if (event.type == "diagnostic") null else event.payload
        val item = payload?.get("item") as? JsonObject
        if (event.type == "turn.started") turn++
        if (event.type.startsWith("item.") && item.string("type") == "agent_message") {
            val itemId = item.string("id")
            val id = if (itemId.isBlank()) "message:${event.id}" else "message:$turn:$itemId"
            val text = item.string("text").ifEmpty { event.text }
            val state = if (event.type == "item.completed") "done" else "running"
            val content = TimelineContent.B(itemId = itemId, state = state, text = text)
            val previous = cards[id]
            cards[id] =
                previous?.copy(revision = event.id, source = event.id, content = content)
                    ?: card(event, id, content)
            continue
        }
        val isCovered = event.type in covered || event.type.startsWith("turn.")
        events.add(if (isCovered) event.copy(timeline = true) else event)
        fun text(key: String) = (payload?.get(key) as? JsonPrimitive)?.contentOrNull
        when {
            event.type == "chat.user" -> {
                val sent =
                    (payload?.get("createdAt") as? JsonPrimitive)?.longOrNull?.takeIf {
                        it > 0 && it <= event.createdAt
                    } ?: event.createdAt
                cards["user:${event.id}"] =
                    card(
                        event,
                        "user:${event.id}",
                        TimelineContent.A(
                            attachments =
                                payload?.get("attachments")?.let {
                                    build.cairn.app.data.wireJson.decodeFromJsonElement<
                                        List<build.cairn.app.data.ChatAttachment>
                                    >(
                                        it
                                    )
                                } ?: emptyList(),
                            messageId = text("messageId"),
                            questionId = text("questionId"),
                            sentAt = sent,
                            text = text("text") ?: event.text,
                        ),
                    )
            }
            event.type == "turn.waiting" ->
                notice(
                    event,
                    TimelineNotice(
                        code = "backgroundTasks",
                        prominent = false,
                        state = "info",
                        tasks =
                            payload?.get("tasks")?.jsonArray?.map {
                                it.jsonObject.string("description").trim()
                            },
                        taskIds =
                            payload?.get("tasks")?.jsonArray?.map { it.jsonObject.string("id") },
                        waitingSince =
                            event.createdAt.takeIf {
                                payload?.get("tasks")?.jsonArray?.isNotEmpty() == true
                            },
                    ),
                )
            event.type in setOf("turn.failed", "runner.failed", "error") ||
                (event.type == "status" && event.text == "failed") -> {
                val error = payload?.get("error") as? JsonObject
                val detail =
                    error.string("message").ifBlank {
                        text("message") ?: if (event.type == "status") "" else event.text
                    }
                notice(
                    event,
                    TimelineNotice(
                        code = "failure",
                        detail = detail.ifBlank { null },
                        origin =
                            when (event.type) {
                                "status" -> "run"
                                "runner.failed" -> "runner"
                                "error" -> "engine"
                                else -> "turn"
                            },
                        prominent = true,
                        state = "error",
                    ),
                )
            }
            event.type == "status" -> {
                val stopped = event.text in setOf("cancelled", "interrupted")
                notice(
                    event,
                    TimelineNotice(
                        code = "runStatus",
                        prominent = stopped,
                        state = if (stopped) "stopped" else "done",
                        status = event.text,
                    ),
                )
            }
            isCovered ->
                notice(
                    event,
                    TimelineNotice(
                        code =
                            when (event.type) {
                                "turn.started" -> "turnStarted"
                                "turn.completed" -> "turnCompleted"
                                "thread.started" -> "sessionStarted"
                                "diagnostic" -> "diagnostic"
                                else -> "runStatus"
                            },
                        detail = event.text.takeIf { event.type == "diagnostic" },
                        prominent = false,
                        state = if (event.type == "diagnostic") "info" else "done",
                    ),
                )
        }
    }
    return events to timelineOrder(cards.values)
}

/** `timelineEntries` over raw test events, with the stand-in cards above. */
internal fun fixtureEntries(
    input: List<RunEvent>,
    chat: Boolean = false,
    pendingWakeup: ChatWakeup? = null,
): List<TimelineEntry> {
    val (events, cards) = withTimeline(input)
    return timelineEntries(events, chat, pendingWakeup, cards)
}

private fun timelineOrder(cards: Collection<TimelineCard>) =
    build.cairn.app.data.timelineOrder(cards)

/** A live batch as the server sends it: the given events with the cards they produce. */
internal fun timelineBatch(
    events: List<RunEvent>,
    state: build.cairn.app.data.LiveState? = null,
    reset: Boolean,
    more: Boolean,
    history: String? = null,
    oldest: Long? = null,
    hasOlder: Boolean = false,
): build.cairn.app.data.LiveBatch {
    val (covered, cards) = withTimeline(events)
    return build.cairn.app.data.LiveBatch(
        covered,
        state,
        reset,
        more,
        history,
        oldest,
        hasOlder,
        cards.map { build.cairn.contracts.TimelineChange.A(it) },
    )
}

/** A saved conversation with the cards its events produced. */
internal fun timelineCache(
    cursor: Long,
    history: String,
    state: build.cairn.app.data.LiveState,
    events: List<RunEvent>,
    position: build.cairn.app.data.ReadingPosition? = null,
    oldest: Long = 0,
    hasOlder: Boolean = false,
): build.cairn.app.data.CachedHistory {
    val (covered, cards) = withTimeline(events)
    return build.cairn.app.data.CachedHistory(
        cursor,
        history,
        state,
        covered,
        cards,
        position = position,
        oldest = oldest,
        hasOlder = hasOlder,
    )
}

/** An older history page with the cards that first appeared in it. */
internal fun timelinePage(
    events: List<RunEvent>,
    history: String,
    oldest: Long,
    hasOlder: Boolean,
): build.cairn.app.data.HistoryPage {
    val (covered, cards) = withTimeline(events)
    return build.cairn.app.data.HistoryPage(
        entries = cards,
        events = covered,
        history = history,
        oldest = oldest,
        hasOlder = hasOlder,
    )
}

package build.cairn.app.ui

import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import build.cairn.app.data.*
import build.cairn.app.diagnostics.PerfOperation
import build.cairn.app.diagnostics.Performance
import build.cairn.contracts.TimelineContent
import kotlinx.coroutines.flow.first
import kotlinx.serialization.json.*

internal fun RunEvent.item(): JsonObject? = activityData()?.get("item") as? JsonObject

internal fun JsonObject?.string(key: String) =
    this?.get(key)?.let { it as? JsonPrimitive }?.contentOrNull.orEmpty()

/** Approval identity is distinct from the row anchored for reading. */
internal fun proposedPlanApproval(
    events: List<RunEvent>,
    cards: List<TimelineCard> = emptyList(),
): String? {
    var latest: RunEvent? = null
    var lastRequest: RunEvent? = null
    for (event in events) {
        if (event.type == "chat.user" && event.id > (lastRequest?.id ?: Long.MIN_VALUE))
            lastRequest = event
        if (event.type !in listOf("item.started", "item.updated", "item.completed")) continue
        val item = event.item()
        if (
            item.string("type") == "proposed_plan" &&
                item.string("parent_id").isBlank() &&
                event.id > (latest?.id ?: Long.MIN_VALUE)
        )
            latest = event
    }
    val requestCard = cards.filter { it.content is TimelineContent.A }.maxByOrNull { it.source }
    val requestSource =
        maxOf(lastRequest?.id ?: Long.MIN_VALUE, requestCard?.source ?: Long.MIN_VALUE)
    val requestId =
        if (requestCard != null && requestCard.source == requestSource)
            (requestCard.content as? TimelineContent.A)?.messageId
        else lastRequest?.payload?.string("messageId")
    val proposal = latest ?: return null
    if (
        proposal.type != "item.completed" ||
            proposal.id <= requestSource ||
            proposal.item().string("text").isBlank()
    )
        return null
    val item = proposal.item()
    val authoredRequest = item.string("request_id")
    if (
        authoredRequest.isBlank() || requestSource != Long.MIN_VALUE && authoredRequest != requestId
    )
        return null
    return "${proposal.runId}:${proposal.id}"
}

internal data class TimelineEntry(
    val key: String,
    val events: List<RunEvent>,
    val message: Boolean,
    val files: List<Deliverable> = emptyList(),
    val notice: Boolean = false,
    /** The server's card for a message or a conversation notice. */
    val card: TimelineCard? = null,
    /** Run-log presentation of the lifecycle events in an activity group, by event. */
    val notices: Map<Long, build.cairn.app.data.TimelineNotice> = emptyMap(),
    /** Persisted events folded into each activity; item IDs may repeat in later turns. */
    val sourceIds: Map<Long, List<Long>> = emptyMap(),
    /** Subagents started one after another work as a team, shown apart from the agent's steps. */
    val agents: List<Subagent> = emptyList(),
    /** A scheduled wait, shown as one card from its scheduling to its resumption. */
    val wait: ActivityWait? = null,
    /** The approach a plan turn proposed. */
    val proposal: Boolean = false,
    /** The completed root event to approve, separate from the first-event display key. */
    val proposalSourceId: String? = null,
    /** The agent's plan, kept up to date by its later updates. */
    val plan: AgentPlan? = null,
    /** Where an update changed the plan. */
    val planUpdate: PlanUpdate? = null,
    /** The coding agent compacted its context; the history above stays as it was. */
    val compaction: ContextCompaction? = null,
)

/**
 * Fold tool updates in place within a turn. Messages, the run cycle, errors and retries arrive as
 * cards the server interpreted; they take their place among the activity, and the events they
 * present only mark the boundaries of the families still read here.
 */
internal fun timelineEntries(
    events: List<RunEvent>,
    chat: Boolean = false,
    pendingWakeup: ChatWakeup? = null,
    cards: List<TimelineCard> = emptyList(),
): List<TimelineEntry> =
    Performance.measure(PerfOperation.TIMELINE) {
        val folded = mutableListOf<RunEvent>()
        val sources = mutableListOf<MutableList<Long>>()
        val positions = mutableMapOf<List<String>, Int>()
        val proposals = mutableMapOf<List<String>, Int>()
        val proposalRequests = mutableMapOf<Int, String>()
        val subagents = subagentTimeline(events)
        for (event in events) {
            // Context usage is a measurement the conversation header shows, not an activity.
            if (event.type == "artifact" || event.type == "context.usage") continue
            if (event.id in subagents.captured) {
                if (event.id in subagents.anchors) {
                    folded.add(event)
                    sources.add(mutableListOf(event.id))
                }
                continue
            }
            when (event.type) {
                "turn.started",
                "turn.completed",
                "turn.failed" -> positions.clear()
            }
            val item = event.item()
            val id = item.string("id")
            val itemType = item.string("type")
            // Each plan update keeps its place: it shows what changed at that moment.
            val planUpdate = itemType == "todo_list"
            if (event.timeline != true && id.isNotBlank() && !planUpdate) {
                // A paginated child proposal can outlive its spawning event. Its
                // item ID must not replace the conversation's own proposed plan.
                val proposalParent =
                    if (itemType == "proposed_plan") item.string("parent_id") else ""
                val key = listOf(event.runId, itemType, id, proposalParent)
                val requestId = if (itemType == "proposed_plan") item.string("request_id") else ""
                // Authored proposals survive receipt replay across turn boundaries. Legacy
                // proposals without a request identity stay scoped to their current turn.
                val requestKey = if (requestId.isNotBlank()) key + requestId else null
                val sameTurn = positions[key]
                val sameTurnRequest = sameTurn?.let(proposalRequests::get)
                val sameTurnIsCompatible =
                    sameTurnRequest == null || requestId.isBlank() || sameTurnRequest == requestId
                val index =
                    requestKey?.let(proposals::get) ?: sameTurn?.takeIf { sameTurnIsCompatible }
                val position = index ?: folded.size
                positions[key] = position
                if (index == null) {
                    folded.add(event)
                    sources.add(mutableListOf(event.id))
                } else {
                    folded[index] = event.copy(id = folded[index].id)
                    sources[index].add(event.id)
                }
                if (requestKey != null) {
                    proposals[requestKey] = position
                    proposalRequests[position] = requestId
                }
            } else {
                folded.add(event)
                sources.add(mutableListOf(event.id))
            }
        }
        val sourceIds =
            folded.mapIndexed { index, event -> event.id to sources[index].toList() }.toMap()
        // The run log presents a lifecycle event through its card; a conversation shows the
        // cards the server marked for it.
        val runLog =
            if (chat) emptyMap()
            else
                cards
                    .mapNotNull { card ->
                        (card.content as? TimelineContent.C)?.let { card.source to it.notice }
                    }
                    .toMap()
        val result = mutableListOf<TimelineEntry>()
        var activity = mutableListOf<RunEvent>()
        var team: Int? = null
        fun flushActivity() {
            team = null
            if (activity.isNotEmpty()) {
                result.add(
                    TimelineEntry(
                        "activity:${activity.first().id}",
                        activity,
                        false,
                        sourceIds = activity.associate { it.id to sourceIds.getValue(it.id) },
                        notices =
                            activity.mapNotNull { e -> runLog[e.id]?.let { e.id to it } }.toMap(),
                    )
                )
                activity = mutableListOf()
            }
        }
        var nextCard = 0
        fun placeCards(before: Long) {
            while (nextCard < cards.size && cards[nextCard].position < before) {
                val card = cards[nextCard++]
                when (val content = card.content) {
                    is TimelineContent.A -> {
                        flushActivity()
                        result.add(TimelineEntry(card.id, emptyList(), true, card = card))
                    }
                    // A reply repeating a failure the conversation reports stays in the run log.
                    is TimelineContent.B ->
                        if (!chat || content.echo != true) {
                            flushActivity()
                            result.add(TimelineEntry(card.id, emptyList(), true, card = card))
                        }
                    is TimelineContent.C ->
                        if (chat && content.notice.shown()) {
                            flushActivity()
                            result.add(
                                TimelineEntry(
                                    card.id,
                                    emptyList(),
                                    false,
                                    notice = true,
                                    card = card,
                                )
                            )
                        } else if (!chat && card.source !in sourceIds) {
                            // A card can change in this window while its source is on an older
                            // page.
                            flushActivity()
                            result.add(TimelineEntry(card.id, emptyList(), false, card = card))
                        }
                }
            }
        }
        // The plan its updates change, until the agent finishes it or writes another one.
        var planIndex: Int? = null
        // By run and item: a compaction's start and end update one divider.
        val compactions = mutableMapOf<String, Int>()
        // The wait still pending, by its wakeup ID, so that its result and resumption complete it.
        val waits = mutableMapOf<String, Int>()
        var openWait: Int? = null
        fun updateWait(index: Int, change: (ActivityWait) -> ActivityWait) {
            result[index] = result[index].copy(wait = change(result[index].wait!!))
        }
        // A scheduled wait only starts once its turn ends: its card follows what the agent did
        // after the call, and moves to where the turn ends, another wait or a message comes, or
        // the history does.
        var scheduled: Int? = null
        val moves = mutableMapOf<Int, Int>()
        fun placeScheduled() {
            val index = scheduled ?: return
            flushActivity()
            moves[index] = result.size
            scheduled = null
        }
        for ((eventIndex, event) in folded.withIndex()) {
            placeCards(event.id)
            // A compaction still running when its turn or run ends did not complete.
            compactionEnd(event.type, event.text)?.let { end ->
                for ((key, index) in compactions) {
                    val compaction = result[index].compaction ?: continue
                    if (
                        compaction.state == "running" &&
                            key.startsWith("compaction:${event.runId}:")
                    )
                        result[index] =
                            result[index].copy(compaction = compaction.copy(state = end))
                }
            }
            if (
                event.type in WAIT_BOUNDARIES ||
                    (event.type == "status" && event.text in TERMINAL_STATUSES)
            )
                placeScheduled()
            when (event.type) {
                "chat.sleep",
                "chat.wait",
                "run.wait" -> {
                    val wakeup = event.wakeupPayload() ?: continue
                    if (wakeup.id in waits) continue
                    openWait?.let { index -> updateWait(index) { it.copy(ended = "replaced") } }
                    openWait = result.size
                    scheduled = result.size
                    waits[wakeup.id] = result.size
                    result.add(
                        TimelineEntry(
                            "wait:${wakeup.id}",
                            listOf(event),
                            false,
                            wait = ActivityWait(wakeup),
                        )
                    )
                    continue
                }
                // The condition completed: right away (the agent never waited) or while it waited.
                "chat.wait_ready",
                "run.wait_ready" -> {
                    val observation = event.activityData()?.let(::presentWaitObservation)
                    // Right away, the agent never waited, and this call replaced any wait.
                    val immediate = observation?.get("immediate") == JsonPrimitive(true)
                    if (immediate) {
                        openWait?.let { index -> updateWait(index) { it.copy(ended = "replaced") } }
                        openWait = null
                    }
                    // The resumption or live snapshot identifies a wait scheduled outside history.
                    val resumed =
                        folded
                            .drop(eventIndex + 1)
                            .takeWhile {
                                it.type !in
                                    listOf("chat.user", "chat.wait", "chat.sleep", "run.wait")
                            }
                            .firstOrNull {
                                it.type in listOf("chat.wakeup", "run.wakeup") &&
                                    (it.payload?.get("observation") as? JsonObject)?.let(
                                        ::presentWaitObservation
                                    ) == observation
                            }
                            ?.wakeupPayload()
                    val open =
                        openWait?.let { result[it].wait?.wakeup }?.takeIf { it.condition != null }
                    val live = pendingWakeup?.takeIf {
                        it.condition != null && it.createdAt <= event.createdAt
                    }
                    val target = if (immediate) null else open ?: resumed ?: live
                    var pending = target?.id?.let(waits::get)
                    if (pending == null && target != null) {
                        flushActivity()
                        pending = result.size
                        waits[target.id] = pending
                        result.add(
                            TimelineEntry(
                                "wait:${target.id}",
                                listOf(event),
                                false,
                                wait = ActivityWait(target),
                            )
                        )
                    }
                    if (pending != null)
                        updateWait(pending) { it.copy(observation = observation, observed = true) }
                    else {
                        flushActivity()
                        result.add(
                            TimelineEntry(
                                "wait-ready:${event.id}",
                                listOf(event),
                                false,
                                wait =
                                    ActivityWait(
                                        null,
                                        observation,
                                        observed = true,
                                        // The result carries the reason.
                                        reason =
                                            if (immediate) observation.string("reason") else "",
                                    ),
                            )
                        )
                    }
                    continue
                }
                "chat.wakeup",
                "run.wakeup" -> {
                    val wakeup = event.wakeupPayload()
                    // Its scheduling may be in an older page of the history.
                    val index =
                        wakeup?.id?.let(waits::get)
                            ?: run {
                                flushActivity()
                                result.add(
                                    TimelineEntry(
                                        "wait:${wakeup?.id ?: event.id}",
                                        listOf(event),
                                        false,
                                        wait = ActivityWait(wakeup),
                                    )
                                )
                                wakeup?.id?.let { waits[it] = result.lastIndex }
                                result.lastIndex
                            }
                    updateWait(index) {
                        it.copy(
                            observation = wakeup?.observationJson ?: it.observation,
                            observed = true,
                            resumedAt = event.createdAt,
                        )
                    }
                    if (index == openWait) openWait = null
                    continue
                }
                // A new message cancels the pending wakeup.
                "chat.user" -> {
                    openWait?.let { index -> updateWait(index) { it.copy(ended = "cancelled") } }
                    openWait = null
                    // A new request gets its own plan, even when the agent carries the previous
                    // list over.
                    planIndex = null
                }
            }
            // A run that ends without succeeding never resumes the wait it scheduled. Only its
            // terminal status says so: a failed turn may be retried, and the retry keeps the
            // wakeup.
            val failedTurn =
                event.type == "status" && event.text in setOf("failed", "cancelled", "interrupted")
            openWait
                ?.takeIf { failedTurn && result[it].wait?.observed != true }
                ?.let { index ->
                    updateWait(index) { it.copy(ended = "cancelled") }
                    openWait = null
                }
            // The timeline presents this event: its card takes its place here. The run log keeps
            // a lifecycle event among the activity, presented by its card.
            if (event.timeline == true) {
                placeCards(event.id + 1)
                if (event.id in runLog) {
                    if (activity.size == MAX_ACTIVITY_GROUP_SIZE) flushActivity()
                    team = null
                    activity.add(event)
                }
                continue
            }
            val waitTool =
                event.item().let { it.string("type") == "mcp_tool_call" && isWaitTool(it) }
            val delegateTool =
                event.item().let { it.string("type") == "mcp_tool_call" && isDelegateTool(it) }
            if (delegateTool && !presentActivity(event).failed) continue
            // The wait shows as its own card; only a call that failed stays among the tools.
            if (waitTool && !presentActivity(event).failed) {
                continue
            }
            val compactionItem =
                event.item()?.takeIf {
                    it.string("type") == "context_compaction" && event.type.startsWith("item.")
                }
            if (compactionItem != null) {
                val key =
                    "compaction:${event.runId}:${compactionItem.string("id").ifBlank { event.id.toString() }}"
                val index = compactions[key]
                val compaction =
                    contextCompaction(
                        compactionItem,
                        event.type,
                        index?.let { result[it].compaction },
                    )
                if (index == null) {
                    flushActivity()
                    result.add(TimelineEntry(key, listOf(event), false, compaction = compaction))
                    compactions[key] = result.lastIndex
                } else result[index] = result[index].copy(compaction = compaction)
                continue
            }
            // The agent's own plan shows apart from its tools. A call that failed did not change
            // it.
            val planItem =
                event.item()?.takeIf {
                    it.string("type") == "todo_list" && it.string("parent_id").isBlank()
                }
            if (planItem != null) {
                val steps = planSteps(planItem)
                if (steps.isEmpty() || planItem.string("status") == "failed") continue
                val current = planIndex?.let { result[it].plan }
                val explanation = planItem.string("explanation")
                if (current != null && samePlan(current.steps, steps)) {
                    val updatedExplanation = explanation.ifBlank { current.explanation }
                    if (steps != current.steps || updatedExplanation != current.explanation) {
                        val index = checkNotNull(planIndex)
                        result[index] =
                            result[index].copy(
                                plan =
                                    current.copy(
                                        steps = steps,
                                        explanation = updatedExplanation,
                                        updatedAt = event.createdAt,
                                    )
                            )
                    }
                    continue
                }
                if (current == null || !continuesPlan(current.steps, steps)) {
                    flushActivity()
                    planIndex = result.size
                    result.add(
                        TimelineEntry(
                            "plan:${event.id}",
                            listOf(event),
                            false,
                            plan = AgentPlan(steps, explanation, event.createdAt, event.createdAt),
                        )
                    )
                    continue
                }
                val index = checkNotNull(planIndex)
                val change = planChange(current.steps, steps)
                result[index] =
                    result[index].copy(
                        plan =
                            current.copy(
                                steps = steps,
                                explanation = explanation.ifBlank { current.explanation },
                                updatedAt = event.createdAt,
                            )
                    )
                if (change != null) {
                    flushActivity()
                    result.add(
                        TimelineEntry(
                            "plan-update:${event.id}",
                            listOf(event),
                            false,
                            planUpdate = PlanUpdate(result[index].key, change, steps.progress()),
                        )
                    )
                }
                continue
            }
            val agent = subagents.anchors[event.id]
            if (agent != null) {
                val index = team
                if (index == null) {
                    flushActivity()
                    result.add(
                        TimelineEntry(
                            "team:${agent.id}",
                            listOf(event),
                            false,
                            agents = listOf(agent),
                        )
                    )
                    team = result.lastIndex
                } else {
                    val entry = result[index]
                    result[index] =
                        entry.copy(events = entry.events + event, agents = entry.agents + agent)
                }
                continue
            }
            if (
                event.item().string("type") == "proposed_plan" &&
                    event.item().string("parent_id").isBlank()
            ) {
                flushActivity()
                val proposalSources = sourceIds.getValue(event.id)
                result.add(
                    TimelineEntry(
                        "proposal:${event.id}",
                        listOf(event),
                        false,
                        sourceIds = mapOf(event.id to proposalSources),
                        proposal = true,
                        proposalSourceId =
                            if (event.type == "item.completed")
                                "${event.runId}:${proposalSources.last()}"
                            else null,
                    )
                )
                continue
            }
            // An engine marker Cairn does not know yet stays in the run log.
            if (chat && presentActivity(event).let { it.kind == ActivityKind.NOTICE && !it.failed })
                continue
            if (activity.size == MAX_ACTIVITY_GROUP_SIZE) {
                flushActivity()
            }
            team = null
            activity.add(event)
        }
        placeCards(Long.MAX_VALUE)
        flushActivity()
        placeScheduled()
        val knownWait = pendingWakeup?.id?.let(waits::get)
        if (knownWait != null && pendingWakeup.observation != null) {
            updateWait(knownWait) {
                if (it.observed) it
                else it.copy(observation = pendingWakeup.observationJson, observed = true)
            }
        }
        if (pendingWakeup != null && knownWait == null) {
            result.add(
                TimelineEntry(
                    "wait:${pendingWakeup.id}",
                    emptyList(),
                    false,
                    wait =
                        ActivityWait(
                            pendingWakeup,
                            pendingWakeup.observationJson,
                            observed = pendingWakeup.observation != null,
                        ),
                )
            )
        }
        val placed =
            if (moves.isEmpty()) result
            else {
                // Each moved card goes before the entry at its target, in one pass.
                val arriving = moves.entries.groupBy({ it.value }, { it.key })
                buildList(result.size) {
                    for (index in 0..result.size) {
                        arriving[index]?.sorted()?.forEach { add(result[it]) }
                        if (index < result.size && index !in moves) add(result[index])
                    }
                }
            }
        placed
    }

// Where a scheduled wait's card is placed: its turn ended, or a message or another wait event came.
private val WAIT_BOUNDARIES =
    setOf(
        // The engine's turn.started can arrive after wait registration, before final replies.
        "turn.completed",
        "turn.failed",
        "chat.user",
        "chat.sleep",
        "chat.wait",
        "chat.wait_ready",
        "chat.wakeup",
        "run.wait",
        "run.wait_ready",
        "run.wakeup",
    )
internal val TERMINAL_STATUSES = setOf("succeeded", "failed", "cancelled", "interrupted")

/**
 * Match the web feed: attach each publication beside its response, keeping versions per message.
 */
internal fun deliveryTimeline(
    entries: List<TimelineEntry>,
    artifacts: List<Deliverable>,
): List<TimelineEntry> {
    val groups = linkedMapOf<Int, MutableList<Deliverable>>()
    val latest =
        artifacts
            .groupBy { it.messageId.orEmpty() to it.key }
            .values
            .map { it.maxBy { file -> file.version } }
    for (file in latest) {
        var position = entries.lastIndex
        for ((index, entry) in entries.withIndex()) {
            val card = entry.card?.takeIf { entry.message } ?: continue
            val sent = card.content as? TimelineContent.A
            val time = sent?.sentAt ?: card.createdAt
            if (time < file.createdAt) continue
            position = if (sent != null) index - 1 else index
            break
        }
        groups.getOrPut(position) { mutableListOf() }.add(file)
    }
    return buildList {
        fun append(index: Int) {
            groups[index]?.let {
                add(
                    TimelineEntry(
                        "files:${entries.getOrNull(index)?.key ?: "start"}",
                        emptyList(),
                        false,
                        it,
                    )
                )
            }
        }
        append(-1)
        entries.forEachIndexed { index, entry ->
            add(entry)
            append(index)
        }
    }
}

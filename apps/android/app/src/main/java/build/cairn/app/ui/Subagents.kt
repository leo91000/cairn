package build.cairn.app.ui

import build.cairn.app.data.Chat
import build.cairn.app.data.RunEvent
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.longOrNull

private val workspaceDelegateTool = Regex("^(?:mcp__)?cairn_workspace(?:__|_|\\.)delegate_task$")

/** Successful calls are represented by Cairn's chat.delegated team event. */
internal fun isDelegateTool(item: JsonObject?): Boolean {
    val tool = item.string("tool")
    return workspaceDelegateTool.matches(tool) ||
        (item.string("server") == "cairn_workspace" && tool == "delegate_task")
}

internal enum class SubagentState(val label: String) {
    RUNNING("En cours"),
    DONE("Terminé"),
    FAILED("Échec"),
    STOPPED("Arrêté"),
}

/** A subagent the agent delegated work to, with the steps it took on its own. */
internal data class Subagent(
    /** Stable presentation identity: one row per subagent, across the turns of its run. */
    val id: String,
    val name: String,
    val type: String,
    val prompt: String,
    /** Its final report, once it finished. */
    val result: String,
    /** Its latest message while it works. */
    val message: String,
    /** What Claude Code says it is doing. */
    val activity: String,
    val model: String,
    val background: Boolean,
    val state: SubagentState,
    val taskId: String,
    val startedAt: Long,
    val durationMs: Long?,
    val tokens: Long?,
    val toolUses: Long?,
    val steps: List<SubagentStep>,
    /** A task Cairn delegated to another conversation, followed through that conversation. */
    val delegation: SubagentDelegation? = null,
)

/** Where a delegated task works, and what its conversation shows now. */
internal data class SubagentDelegation(
    val chatId: String,
    val agentName: String,
    val provider: String,
    val summary: String = "",
)

internal sealed interface SubagentStep {
    val key: String

    data class Action(override val key: String, val activity: ActivityPresentation) : SubagentStep

    /** A subagent this subagent delegated to in turn. */
    data class Agent(val agent: Subagent) : SubagentStep {
        override val key
            get() = agent.id
    }
}

/**
 * What the subagents of a timeline became. Their steps and messages leave the agent's own timeline;
 * each top-level subagent stays at the event where it first appeared.
 */
internal class SubagentTimeline(val captured: Set<Long>, val anchors: Map<Long, Subagent>)

private class SubagentBuilder(val id: String, val startedAt: Long) {
    var name = ""
    var type = ""
    var prompt = ""
    var result = ""
    var message = ""
    var activity = ""
    var model = ""
    var background = false
    var state = SubagentState.RUNNING
    var taskId = ""
    var durationMs: Long? = null
    var tokens: Long? = null
    var toolUses: Long? = null
    var delegation: SubagentDelegation? = null
    var updated = false
    // Child builders for nested subagents, presentations for the subagent's own steps.
    val steps = linkedMapOf<String, Any>()

    fun update(item: JsonObject?, event: RunEvent) {
        val running = state == SubagentState.RUNNING
        state =
            mapOf(
                "completed" to SubagentState.DONE,
                "failed" to SubagentState.FAILED,
                "stopped" to SubagentState.STOPPED,
            )[item.string("status")] ?: SubagentState.RUNNING
        name = item.string("name").ifBlank { "Sous-agent" }
        type = item.string("agent_type").ifBlank { type }
        taskId = item.string("task_id").ifBlank { taskId }
        prompt = item.string("prompt").ifBlank { prompt }
        val report = item?.get("result") as? JsonPrimitive
        if (report?.isString == true) result = report.content
        // A follow-up starts over: the previous message belongs to the previous task.
        if (state == SubagentState.RUNNING && (!running || !updated)) message = ""
        activity = item.string("activity")
        model = item.string("model").ifBlank { model }
        background = (item?.get("background") as? JsonPrimitive)?.booleanOrNull == true
        tokens = (item?.get("tokens") as? JsonPrimitive)?.longOrNull
        toolUses = (item?.get("tool_uses") as? JsonPrimitive)?.longOrNull
        val reported = (item?.get("duration_ms") as? JsonPrimitive)?.longOrNull
        durationMs =
            when {
                state == SubagentState.RUNNING -> null
                reported != null -> reported
                updated -> (event.createdAt - startedAt).coerceAtLeast(0)
                else -> null
            }
        updated = true
    }

    /** The run ended: a subagent still marked as working will not report anymore. */
    fun stop(at: Long) {
        // A delegated task keeps working in its own conversation.
        if (state != SubagentState.RUNNING || delegation != null) return
        state = SubagentState.STOPPED
        durationMs = (at - startedAt).coerceAtLeast(0)
        for ((key, step) in steps) {
            if (step is ActivityPresentation && step.isRunning())
                steps[key] = step.copy(state = SubagentState.STOPPED.label)
        }
    }

    fun build(): Subagent =
        Subagent(
            id,
            name,
            type,
            prompt,
            result,
            message,
            activity,
            model,
            background,
            state,
            taskId,
            startedAt,
            durationMs,
            tokens,
            toolUses,
            steps.map { (key, step) ->
                if (step is SubagentBuilder) SubagentStep.Agent(step.build())
                else SubagentStep.Action(key, step as ActivityPresentation)
            },
            delegation,
        )
}

private val endedStatuses = listOf("succeeded", "failed", "cancelled", "interrupted")

/** Fold subagent lifecycles, their steps and their messages, like the web activity feed. */
internal fun subagentTimeline(events: List<RunEvent>): SubagentTimeline {
    // Claude Code may report a subagent before its spawn; the spawn then replaces that early ID.
    val aliases = mutableMapOf<String, String>()
    for (event in events) {
        val item = event.item()
        if (item.string("type") == "subagent" && item.string("replaces_id").isNotBlank())
            aliases["${event.runId}:${item.string("replaces_id")}"] = item.string("id")
    }
    fun agentKey(event: RunEvent, id: String) =
        "${event.runId}:${aliases["${event.runId}:$id"] ?: id}"

    // By the agent's own item ID: a subagent's steps can arrive after the turn that started it.
    val builders = linkedMapOf<String, SubagentBuilder>()
    val captured = mutableSetOf<Long>()
    val anchors = linkedMapOf<Long, SubagentBuilder>()
    for (event in events) {
        if (event.type == "artifact" || event.type == "chat.user") continue

        // Keep ended children stopped in the history when a later execution starts.
        val ended =
            event.type in listOf("turn.completed", "turn.failed") ||
                (event.type == "status" && event.text in endedStatuses)
        if (ended)
            for ((key, builder) in builders) if (key.startsWith("${event.runId}:"))
                builder.stop(event.createdAt)

        // A delegated task works like a subagent, in a conversation of its own.
        if (event.type == "chat.delegated") {
            val data = event.payload?.let(::JsonObject)
            val chatId = data.string("chatId")
            if (chatId.isNotBlank() && "delegated:$chatId" !in builders) {
                val builder = SubagentBuilder("delegated:$chatId", event.createdAt)
                builder.name =
                    data.string("title").ifBlank { event.text.ifBlank { "Tâche déléguée" } }
                builder.type = data.string("agentName")
                builder.prompt = data.string("task")
                builder.model = data.string("model")
                builder.background = true
                builder.delegation =
                    SubagentDelegation(chatId, data.string("agentName"), data.string("provider"))
                builders[builder.id] = builder
                anchors[event.id] = builder
            }
            captured += event.id
            continue
        }

        val item = event.item()
        val type = item.string("type")
        val itemId = item.string("id")
        val parentId = item.string("parent_id")
        val parent = if (parentId.isBlank()) null else builders[agentKey(event, parentId)]
        if (type == "subagent_message" && parent != null) {
            parent.message = item.string("text")
            captured += event.id
            continue
        }

        if (type == "subagent" && itemId.isNotBlank()) {
            val key = agentKey(event, itemId)
            val builder =
                builders.getOrPut(key) {
                    SubagentBuilder(key, event.createdAt).also {
                        if (parent != null) parent.steps[it.id] = it else anchors[event.id] = it
                    }
                }
            builder.update(item, event)
            captured += event.id
            continue
        }

        if (parent != null && !event.isMessage()) {
            val key = "${parent.id}:${itemId.ifBlank { "event:${event.id}" }}"
            parent.steps[key] = presentActivity(event)
            captured += event.id
        }
    }

    return SubagentTimeline(captured, anchors.mapValues { it.value.build() })
}

/** A subagent still marked as working once the run has ended will not report anymore. */
internal fun Subagent.state(active: Boolean) =
    if (state == SubagentState.RUNNING && !active && delegation == null) SubagentState.STOPPED
    else state

/** A delegated task as its conversation shows it now; a missing conversation was deleted. */
internal fun Subagent.followed(chat: Chat?): Subagent {
    val delegation = delegation ?: return this
    if (chat == null || chat.lifecycle != "active")
        return copy(
            state = SubagentState.STOPPED,
            delegation = delegation.copy(summary = "Conversation supprimée"),
        )
    val state =
        when {
            chat.status in listOf("failed", "interrupted") -> SubagentState.FAILED
            chat.paused || chat.status == "cancelled" -> SubagentState.STOPPED
            chat.pendingQuestions > 0 ||
                chat.status in listOf("running", "queued") ||
                chat.wakeup != null -> SubagentState.RUNNING
            chat.status == "idle" -> SubagentState.STOPPED
            else -> SubagentState.DONE
        }
    val summary =
        when {
            state == SubagentState.RUNNING && chat.pendingQuestions > 1 ->
                "${chat.pendingQuestions} questions attendent une réponse"
            state == SubagentState.RUNNING && chat.pendingQuestions == 1 -> "Pose une question"
            state == SubagentState.RUNNING ->
                if (chat.status in listOf("queued", "idle")) "En attente d’un runner"
                else if (chat.wakeup != null) "En attente" else "En cours"
            state == SubagentState.FAILED -> "Échec"
            state == SubagentState.STOPPED -> "Arrêtée"
            else -> chat.preview ?: "A rendu compte"
        }
    return copy(state = state, delegation = delegation.copy(summary = summary))
}

/** Delegated tasks of a timeline, followed through the conversations [chats] lists. */
internal fun List<TimelineEntry>.followingDelegations(chats: List<Chat>?): List<TimelineEntry> {
    if (none { entry -> entry.agents.any { it.delegation != null } }) return this
    val byId = chats.orEmpty().associateBy { it.id }
    return map { entry ->
        if (entry.agents.none { it.delegation != null }) entry
        else entry.copy(agents = entry.agents.map { it.followed(byId[it.delegation?.chatId]) })
    }
}

internal fun subagentInitials(name: String): String {
    val words = name.split(Regex("\\s+")).filter { word -> word.any { it.isLetterOrDigit() } }
    val letters =
        if (words.size > 1)
            "${words[0].first { it.isLetterOrDigit() }}${words[1].first { it.isLetterOrDigit() }}"
        else name.filter { it.isLetterOrDigit() }.take(2)
    return letters.uppercase().ifEmpty { "·" }
}

private fun firstLine(value: String) =
    value
        .lineSequence()
        .map { it.replace(Regex("^[#>*\\s-]+"), "").trim() }
        .firstOrNull { it.isNotEmpty() }
        .orEmpty()

/** What the subagent does now, or what it reported. */
internal fun Subagent.summary(active: Boolean): String {
    delegation?.let {
        return it.summary.ifBlank { "En cours" }
    }
    val report = firstLine(result).ifEmpty { firstLine(message) }
    return when (state(active)) {
        SubagentState.RUNNING -> {
            val latest =
                steps
                    .asReversed()
                    .firstNotNullOfOrNull {
                        (it as? SubagentStep.Action)?.activity?.takeIf { step ->
                            step.kind != ActivityKind.NOTICE
                        }
                    }
                    ?.let { step ->
                        listOf(step.title, step.command.ifBlank { step.subtitle })
                            .filter { it.isNotBlank() }
                            .joinToString(" · ")
                    }
            activity.ifBlank { latest.orEmpty() }.ifBlank { "Démarrage…" }
        }
        SubagentState.FAILED -> report.ifEmpty { "Échec sans rapport" }
        SubagentState.STOPPED -> report.ifEmpty { "Arrêté avant d’avoir rendu compte" }
        SubagentState.DONE -> report.ifEmpty { "Terminé" }
    }
}

internal fun Subagent.elapsed(active: Boolean, now: Long): String =
    // A delegated task's time is its conversation's.
    if (delegation != null) ""
    else if (state(active) == SubagentState.RUNNING) liveElapsed(startedAt, now)
    // liveElapsed formats any span; anchor this one at an arbitrary positive start.
    else durationMs?.let { liveElapsed(1, 1 + it) }.orEmpty()

internal fun Subagent.details(): String =
    listOfNotNull(
            model.ifBlank { null },
            toolUses?.let {
                if (it == 1L) "1 utilisation d’outil" else "${number(it)} utilisations d’outils"
            },
            tokens?.let { "${number(it)} jetons" },
        )
        .joinToString(" · ")

private fun number(value: Long) = java.text.NumberFormat.getIntegerInstance().format(value)

/** Nested subagents started one after another, like the top-level teams. */
internal fun Subagent.teams(): List<List<Subagent>> {
    val teams = mutableListOf<List<Subagent>>()
    var team = mutableListOf<Subagent>()
    for (step in steps) {
        if (step is SubagentStep.Agent) team.add(step.agent)
        else if (team.isNotEmpty()) {
            teams.add(team)
            team = mutableListOf()
        }
    }
    if (team.isNotEmpty()) teams.add(team)
    return teams
}

/** Teams include nested delegations so every visible subagent can open its own activity. */
internal fun List<Subagent>.withNestedTeams(): List<List<Subagent>> =
    listOf(this) + flatMap { agent -> agent.teams().flatMap { it.withNestedTeams() } }

internal fun subagentTeams(timeline: List<TimelineEntry>): List<List<Subagent>> =
    timeline.filter { it.agents.isNotEmpty() }.flatMap { it.agents.withNestedTeams() }

/** Preserve a team's identity through any already loaded member, including an older prefix. */
internal class SubagentTeamKeys {
    private var owners = mapOf<String, String>()

    fun stabilize(teams: List<List<Subagent>>): List<String> {
        val used = mutableSetOf<String>()

        val identities = teams.map { members ->
            val previous = members.firstNotNullOfOrNull { owners[it.id]?.takeIf { it !in used } }
            val base = previous ?: members.first().id
            var identity = base
            var suffix = 1
            while (!used.add(identity)) identity = "$base:${suffix++}"
            identity
        }

        owners = buildMap {
            teams.zip(identities).forEach { (members, identity) ->
                members.forEach { put(it.id, identity) }
            }
        }

        return identities
    }
}

/** Current-request agents and any earlier children still working in the same execution. */
internal fun currentSubagents(timeline: List<TimelineEntry>): List<Subagent> {
    val start = timeline.indexOfLast { it.message && it.events.single().type == "chat.user" }
    val current = subagentTeams(timeline.drop(start + 1)).flatten().map { it.id }.toSet()
    // A delegated task reports through its own conversation: earlier ones stay with their request.
    return subagentTeams(timeline).flatten().filter {
        it.id in current || (it.state == SubagentState.RUNNING && it.delegation == null)
    }
}

/** The bar replaces the explicit background wait only if it represents every awaited task. */
internal fun waitingForSubagents(events: List<RunEvent>, agents: List<Subagent>): Boolean {
    val announcement = events.lastOrNull { it.type == "turn.waiting" } ?: return false
    val tasks = announcement.activityData()?.get("tasks").asArray().orEmpty()
    return tasks.isNotEmpty() &&
        tasks.all { task ->
            val id = task.asObject().string("id")
            id.isNotEmpty() && agents.any { it.taskId == id }
        }
}

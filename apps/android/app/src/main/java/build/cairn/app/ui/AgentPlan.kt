package build.cairn.app.ui

import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.booleanOrNull

/**
 * The plan an agent keeps while it works. Codex, Claude Code and OpenCode each report the whole
 * list at every update, as one `todo_list` item. Mirrors the web's agent-plan.ts.
 */
internal enum class PlanStepState(val label: String) {
    PENDING("À faire"),
    ACTIVE("En cours"),
    DONE("Terminée"),
    CANCELLED("Abandonnée"),
}

internal data class PlanStep(
    val text: String,
    /** What the agent does during the step, when it says so (Claude Code's `activeForm`). */
    val activeText: String,
    val state: PlanStepState,
)

internal data class AgentPlan(
    val steps: List<PlanStep>,
    /** Why the agent chose this plan (Codex). */
    val explanation: String,
    /** When the agent wrote the plan, then last changed it. */
    val time: Long,
    val updatedAt: Long,
)

/** What one update changed in the plan, named by the steps' text. */
internal data class PlanChange(
    val done: List<String> = emptyList(),
    val started: List<String> = emptyList(),
    val added: List<String> = emptyList(),
    val cancelled: List<String> = emptyList(),
)

/** Where an update changed the plan, with its progress afterwards. */
internal data class PlanUpdate(
    val planKey: String,
    val change: PlanChange,
    val progress: PlanProgress,
)

/** Steps done out of the steps still part of the plan. */
internal data class PlanProgress(val done: Int, val total: Int)

internal data class CurrentStep(val step: PlanStep, val index: Int, val active: Boolean)

/** Steps of a `todo_list` item. Older items, and Codex exec, only say whether a step is done. */
internal fun planSteps(item: JsonObject?): List<PlanStep> =
    item?.get("items").asArray().orEmpty().mapNotNull { value ->
        val step = value.asObject() ?: return@mapNotNull null
        val text = step.string("text").trim()
        if (text.isEmpty()) return@mapNotNull null
        val completed = (step["completed"] as? JsonPrimitive)?.booleanOrNull == true
        val state =
            when (step.string("status")) {
                "completed" -> PlanStepState.DONE
                "in_progress",
                "inProgress" -> PlanStepState.ACTIVE
                "cancelled" -> PlanStepState.CANCELLED
                "pending" -> PlanStepState.PENDING
                else -> if (completed) PlanStepState.DONE else PlanStepState.PENDING
            }
        PlanStep(text, step.string("active_text").trim(), state)
    }

/** Every step is done or abandoned. */
internal fun List<PlanStep>.finished(): Boolean =
    isNotEmpty() && all { it.state == PlanStepState.DONE || it.state == PlanStepState.CANCELLED }

/** Steps done out of the steps still part of the plan. */
internal fun List<PlanStep>.progress(): PlanProgress {
    val kept = filter { it.state != PlanStepState.CANCELLED }
    return PlanProgress(kept.count { it.state == PlanStepState.DONE }, kept.size)
}

/** The step in progress, or the next one when the agent does not mark progress. */
internal fun List<PlanStep>.currentStep(): CurrentStep? {
    val active = indexOfFirst { it.state == PlanStepState.ACTIVE }
    if (active >= 0) return CurrentStep(this[active], active, true)
    val next = indexOfFirst { it.state == PlanStepState.PENDING }
    return if (next >= 0) CurrentStep(this[next], next, false) else null
}

/** The same steps in the same states: agents repeat the list when its call completes. */
internal fun samePlan(previous: List<PlanStep>, next: List<PlanStep>): Boolean =
    previous.size == next.size &&
        previous.zip(next).all { (before, after) ->
            before.text == after.text && before.state == after.state
        }

/** A plan still under way continues with any list sharing one of its steps. */
internal fun continuesPlan(previous: List<PlanStep>, next: List<PlanStep>): Boolean {
    if (previous.finished()) return false
    val known = previous.map { it.text }.toSet()
    return next.any { it.text in known }
}

/**
 * What changed between two versions of a plan, or null when nothing visible did. Claude Code drops
 * a step by leaving it out of the list.
 */
internal fun planChange(previous: List<PlanStep>, next: List<PlanStep>): PlanChange? {
    val before = previous.associate { it.text to it.state }
    val kept = next.map { it.text }.toSet()
    val done = mutableListOf<String>()
    val started = mutableListOf<String>()
    val added = mutableListOf<String>()
    val cancelled = mutableListOf<String>()
    for (step in next) {
        val state = before[step.text]
        if (state == step.state) continue
        when {
            step.state == PlanStepState.DONE -> done += step.text
            step.state == PlanStepState.CANCELLED -> cancelled += step.text
            step.state == PlanStepState.ACTIVE -> started += step.text
            state == null -> added += step.text
        }
    }
    for (step in previous) {
        val unfinished = step.state == PlanStepState.PENDING || step.state == PlanStepState.ACTIVE
        if (step.text !in kept && unfinished) cancelled += step.text
    }
    val change = PlanChange(done, started, added, cancelled)
    return change.takeIf {
        done.isNotEmpty() || started.isNotEmpty() || added.isNotEmpty() || cancelled.isNotEmpty()
    }
}

/** "Terminé X ; commencé Y", for accessibility services. */
internal fun PlanChange.label(): String =
    listOf(
            "terminé" to done,
            "commencé" to started,
            "ajouté" to added,
            "abandonné" to cancelled,
        )
        .filter { it.second.isNotEmpty() }
        .joinToString(" ; ") { (verb, steps) -> "$verb ${steps.joinToString(", ")}" }
        .let { "Plan mis à jour : $it" }

/**
 * The plan of the current request: the latest one the agent wrote or changed since your last
 * message, while some of it remains.
 */
internal fun currentPlan(timeline: List<TimelineEntry>): AgentPlan? {
    val start = timeline.indexOfLast { it.message && it.card?.userMessage == true }
    val latest = timeline.indexOfLast { it.plan != null || it.planUpdate != null }
    if (latest < 0 || latest < start) return null
    val entry = timeline[latest]
    val key = entry.planUpdate?.planKey ?: entry.key
    val plan = timeline.firstOrNull { it.key == key }?.plan ?: entry.plan
    return plan?.takeUnless { it.steps.finished() }
}

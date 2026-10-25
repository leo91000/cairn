package build.cairn.app.ui

import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.core.RepeatMode
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.tween
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.rotate
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import build.cairn.app.data.Chat
import build.cairn.app.data.ChatWakeup
import build.cairn.app.data.RunEvent
import build.cairn.app.data.wireJson
import kotlinx.coroutines.delay
import kotlinx.serialization.json.*

/** What a conversation waits for: a timer, a plugin's condition, another Cairn run or several. */
internal enum class WaitKind {
    TIMER,
    CONDITION,
    RUN,
    TASK,
    GROUP,
}

internal enum class WaitTone {
    OK,
    ERROR,
    WARNING,
    NEUTRAL,
}

/** [title] is a sentence for the conversation; [short] a few words for the Fil and composer. */
internal data class WaitTarget(
    val kind: WaitKind,
    val title: String,
    val short: String,
    val details: List<String>,
)

internal data class WaitOutcome(val tone: WaitTone, val title: String, val detail: String)

/** One condition of a group, as its card lists it. */
internal data class WaitMember(
    /** A native run/task, or a plugin condition. */
    val kind: WaitKind,
    val title: String,
    val details: List<String>,
    /** The run it waits for, which its row opens. */
    val runId: String? = null,
    /** Its result, once it has one; failures included. */
    val outcome: WaitOutcome? = null,
    /**
     * A plugin condition Cairn still checks: when it checks again, and why its last check failed.
     */
    val nextCheckAt: Long? = null,
    val lastError: String? = null,
    /** The delegated conversation, available before its first run starts. */
    val chatId: String? = null,
)

/** A durable wait the agent scheduled with `sleep` or `wait_for`, then how it ended. */
internal data class ActivityWait(
    /** Null when the condition had already completed and the agent never waited. */
    val wakeup: ChatWakeup?,
    /** What Cairn observed once the condition completed or the deadline passed. */
    val observation: JsonObject? = null,
    val observed: Boolean = false,
    /** When the conversation resumed. */
    val resumedAt: Long? = null,
    /**
     * A message or a failed turn cancelled it ("cancelled"), or another wait replaced it
     * ("replaced").
     */
    val ended: String? = null,
    /** The agent's reason, for a result that was already available: no wakeup carries it. */
    val reason: String = "",
) {
    val finished
        get() = resumedAt != null || observed
}

/** The conversation's pending wakeup, which keeps its wait card live, and how to pause it. */
internal data class LiveWait(
    val wakeup: ChatWakeup?,
    val paused: Boolean,
    val pause: () -> Unit = {},
    /** The turn that scheduled it ended successfully: only then does Cairn check its condition. */
    val settled: Boolean = true,
    /** Another action is under way, so pausing would be ignored. */
    val busy: Boolean = false,
    /** Missions cancel their run instead of pausing a conversation. */
    val canPause: Boolean = true,
    /** Opens a run a grouped wait waits for. */
    val openRun: ((String) -> Unit)? = null,
    val openChat: ((String) -> Unit)? = null,
)

internal val LocalLiveWait = compositionLocalOf<LiveWait?> { null }

/** A plugin's text, or empty when it gave none. */
private fun JsonElement?.text() =
    ((this as? JsonPrimitive)?.takeIf { it.isString }?.content).orEmpty()

private val tones =
    mapOf(
        "ok" to WaitTone.OK,
        "error" to WaitTone.ERROR,
        "warning" to WaitTone.WARNING,
        "neutral" to WaitTone.NEUTRAL,
    )

// Conclusions that read as a failure when a plugin result gives no wording of its own.
private val failures = setOf("failure", "failed", "error", "startup_failure", "timed_out")

// Codex names the workspace server; Claude and OpenCode prefix the tool with it.
private val workspaceWaitTool = Regex("^(?:mcp__)?cairn_workspace(?:__|_|\\.)(?:sleep|wait_for)$")

/** A tool call's arguments, which some providers record as JSON text. */
internal fun isWaitTool(item: JsonObject?): Boolean {
    val tool = item.string("tool")
    return workspaceWaitTool.matches(tool) ||
        (item.string("server") == "cairn_workspace" && tool in setOf("sleep", "wait_for"))
}

internal fun RunEvent.wakeupPayload(): ChatWakeup? = payload?.let {
    runCatching { wireJson.decodeFromJsonElement<ChatWakeup>(JsonObject(it)) }.getOrNull()
}

/** The time of day, with the date when it is not today. */
internal fun wakeTime(at: Long, now: Long = System.currentTimeMillis()): String {
    val zone = java.time.ZoneId.systemDefault()
    val date = java.time.Instant.ofEpochMilli(at).atZone(zone)
    val today = date.toLocalDate() == java.time.Instant.ofEpochMilli(now).atZone(zone).toLocalDate()
    return java.time.format.DateTimeFormatter.ofPattern(
            if (today) "HH:mm" else "d MMM HH:mm",
            // The copy is French: so are its months.
            java.util.Locale.FRENCH,
        )
        .format(date)
}

/** Time left before a deadline, rounded up so it never reads zero early. */
internal fun timeLeft(ms: Long): String {
    if (ms <= 0) return "d’un instant à l’autre"
    val seconds = (ms + 999) / 1000
    if (seconds < 60) return "$seconds s"
    val minutes = (seconds + 59) / 60
    if (minutes < 60) return "$minutes min"
    val hours = minutes / 60
    if (hours < 24)
        return if (minutes % 60 == 0L) "$hours h" else "$hours h ${"%02d".format(minutes % 60)}"
    return "${Math.round(hours / 24.0)} j"
}

/** How long a finished wait lasted. */
internal fun waited(ms: Long): String {
    val minutes = Math.round(ms / 60_000.0)
    return if (minutes < 1) "moins d’une minute" else timeLeft(minutes * 60_000)
}

/** A group's mode, `all` or `any`; null for a single condition. */
internal fun waitGroupMode(condition: JsonObject?): String? =
    condition
        ?.takeIf { it.string("type") == "group" && it["conditions"] is JsonArray }
        ?.string("mode")
        ?.takeIf { it == "all" || it == "any" }

/** The results of a group's observation, each beside its condition. */
private fun groupResults(observation: JsonObject?): JsonArray? =
    observation?.takeIf { it.string("type") == "group" }?.get("members").asArray()

/**
 * The conditions of a group, each with its result as soon as it has one: from the final result when
 * the wait ended, otherwise from the progress Cairn records on the pending wakeup.
 */
internal fun waitMembers(wakeup: ChatWakeup?, observation: JsonObject? = null): List<WaitMember> {
    val final = groupResults(observation)
    val conditions: List<JsonElement> =
        wakeup?.condition?.takeIf { waitGroupMode(it) != null }?.get("conditions").asArray()
            ?: final?.map { it.asObject()?.get("condition") ?: JsonNull }
            ?: return emptyList()
    return conditions.mapIndexed { index, element ->
        val condition = element.asObject()
        val progress = wakeup?.members?.getOrNull(index).asObject()
        val result =
            (if (final != null) final.getOrNull(index).asObject()?.get("result")
                else progress?.get("observation"))
                .asObject()
        val outcome = result?.takeIf { it.string("state").isNotEmpty() }?.let(::waitOutcome)
        if (condition.string("type") == "run_finished") {
            val runId = condition.string("runId")
            WaitMember(
                WaitKind.RUN,
                "Exécution Cairn",
                listOfNotNull(runId.take(8).ifBlank { null }),
                runId.ifBlank { null },
                outcome,
            )
        } else if (condition.string("type") == "task_finished") {
            val chatId = condition.string("chatId")
            WaitMember(
                WaitKind.TASK,
                "Tâche déléguée",
                listOfNotNull(chatId.take(8).ifBlank { null }),
                outcome = outcome,
                chatId = chatId.ifBlank { null },
            )
        } else {
            // A plugin condition reads as its plugin words it, or by its type.
            val display = progress?.get("display").asObject()
            val title =
                display
                    ?.get("short")
                    .text()
                    .ifBlank { display?.get("title").text() }
                    .ifBlank {
                        condition
                            .string("type")
                            .ifBlank { "condition" }
                            .replace('_', ' ')
                            .replaceFirstChar {
                                it.uppercase()
                            }
                    }
            WaitMember(
                WaitKind.CONDITION,
                title,
                display
                    ?.get("details")
                    .asArray()
                    .orEmpty()
                    .map { it.text() }
                    .filter {
                        it.isNotBlank()
                    },
                outcome = outcome,
                nextCheckAt =
                    if (outcome == null)
                        (progress?.get("nextCheckAt") as? JsonPrimitive)?.longOrNull
                    else null,
                lastError =
                    if (outcome == null) progress?.get("lastError").text().ifBlank { null }
                    else null,
            )
        }
    }
}

/** A group's words: how many conditions, and whether it waits for each of them or the first. */
private fun groupTarget(mode: String, wakeup: ChatWakeup): WaitTarget {
    val members = waitMembers(wakeup)
    val count = members.size
    val received = members.count { it.outcome != null }
    return if (mode == "any")
        WaitTarget(
            WaitKind.GROUP,
            "Attente du premier de $count résultats",
            "Premier de $count résultats",
            emptyList(),
        )
    else
        WaitTarget(
            WaitKind.GROUP,
            "Attente de $count résultats",
            if (received > 0) "$received sur $count résultats" else "$count résultats",
            emptyList(),
        )
}

internal fun waitTarget(wakeup: ChatWakeup, now: Long = System.currentTimeMillis()): WaitTarget {
    val condition = wakeup.condition
    val mode = waitGroupMode(condition)
    return when {
        mode != null -> groupTarget(mode, wakeup)
        condition.string("type") == "run_finished" ->
            WaitTarget(
                WaitKind.RUN,
                "Attente de la fin d’une autre exécution",
                "Autre exécution",
                emptyList(),
            )
        condition.string("type") == "task_finished" ->
            WaitTarget(
                WaitKind.TASK,
                "Attente de la fin d’une tâche déléguée",
                "Tâche déléguée",
                emptyList(),
            )
        // A plugin condition reads as its plugin words it.
        condition != null -> {
            val display = wakeup.display
            WaitTarget(
                WaitKind.CONDITION,
                display?.get("title").text().ifBlank { "Attente d’une condition" },
                display?.get("short").text().ifBlank { "Condition" },
                display
                    ?.get("details")
                    .asArray()
                    .orEmpty()
                    .map { it.text() }
                    .filter {
                        it.isNotBlank()
                    },
            )
        }
        else -> {
            val at = "Reprise à ${wakeTime(wakeup.wakeAt, now)}"
            WaitTarget(WaitKind.TIMER, at, at, emptyList())
        }
    }
}

/**
 * A wait seen only in a run's history: whether it still stands is unknown, so it reads as what was
 * scheduled rather than what will happen.
 */
internal fun recordedTitle(wakeup: ChatWakeup, now: Long = System.currentTimeMillis()): String {
    val target = waitTarget(wakeup, now)
    return if (target.kind == WaitKind.TIMER) "Reprise programmée à ${wakeTime(wakeup.wakeAt, now)}"
    else "Attente programmée · ${target.short}"
}

/**
 * The live status: a timer resumes at its deadline, a condition at the latest by it. An overdue
 * wait resumes when capacity allows.
 */
internal fun waitCountdown(wakeup: ChatWakeup, now: Long = System.currentTimeMillis()): String {
    val left = wakeup.wakeAt - now
    if (left <= 0) return "Reprise imminente"
    return if (wakeup.condition != null) "${timeLeft(left)} max" else "dans ${timeLeft(left)}"
}

/**
 * The wakeup a conversation still honours: a failed or interrupted turn never resumes it, even when
 * the record lingers. A paused conversation keeps it.
 */
internal fun liveWakeup(chat: Chat?): ChatWakeup? {
    val wakeup = chat?.wakeup ?: return null
    val status = chat.run?.status ?: chat.status
    if (chat.lifecycle != "active" || status in setOf("failed", "interrupted", "cancelled"))
        return null
    return wakeup
}

/** Its result already arrived, or its deadline passed: it resumes as soon as capacity allows. */
internal fun waitDue(wakeup: ChatWakeup, now: Long = System.currentTimeMillis()) =
    wakeup.observation != null || wakeup.wakeAt <= now

/** The conversation header while it waits. */
internal fun waitStatus(wakeup: ChatWakeup, now: Long = System.currentTimeMillis()): String =
    when {
        waitDue(wakeup, now) -> "Reprise imminente"
        wakeup.condition != null -> "Résultat attendu · ${timeLeft(wakeup.wakeAt - now)} max"
        else -> "Reprise dans ${timeLeft(wakeup.wakeAt - now)}"
    }

/** Historical marker for events that predate the explicit immediate boolean. */

/** A plugin's words for a result, when it gave a title. */
private fun resultDisplay(observation: JsonObject): WaitOutcome? {
    val display = observation["display"].asObject() ?: return null
    val title = display["title"].text().ifBlank { null } ?: return null
    return WaitOutcome(
        tones[display["tone"].text()] ?: WaitTone.NEUTRAL,
        title,
        display["detail"].text(),
    )
}

/** What Cairn observed when a wait ended; time reached differs from an external timeout. */
internal fun waitOutcome(observation: JsonObject?): WaitOutcome {
    val state = observation.string("state")
    val finished = WaitOutcome(WaitTone.NEUTRAL, "Attente terminée", "")
    if (observation == null || state.isEmpty()) return finished
    resultDisplay(observation)?.let {
        return it
    }
    if (state == "completed" && observation.string("type") == "time")
        return WaitOutcome(WaitTone.NEUTRAL, "Échéance atteinte", "")
    groupResults(observation)?.let {
        return groupOutcome(observation, it)
    }
    when (state) {
        "timed_out" -> return WaitOutcome(WaitTone.WARNING, "Échéance atteinte sans résultat", "")
        "unavailable" ->
            return WaitOutcome(
                WaitTone.WARNING,
                "Résultat indisponible",
                observation.string("message"),
            )
    }
    if (observation.string("type") == "task_finished") {
        val status = observation.string("status")
        if (status == "succeeded")
            return WaitOutcome(
                WaitTone.OK,
                "La tâche est terminée",
                when (observation.string("outcomeStatus")) {
                    "blocked" -> "Bloquée"
                    "needs_input" -> "Attend votre réponse"
                    else -> ""
                },
            )
        val title =
            when (status) {
                "failed" -> "La tâche a échoué"
                "cancelled" -> "La tâche a été annulée"
                "deleted" -> "La tâche a été supprimée"
                "interrupted" -> "La tâche a été interrompue"
                else -> "La tâche s’est arrêtée"
            }
        return WaitOutcome(
            if (status == "failed") WaitTone.ERROR else WaitTone.WARNING,
            title,
            observation.string("error"),
        )
    }
    if (observation.string("type") == "run_finished")
        return when (val status = observation.string("status")) {
            "succeeded" ->
                WaitOutcome(
                    WaitTone.OK,
                    "L’exécution est terminée",
                    when (observation.string("outcomeStatus")) {
                        "blocked" -> "Bloquée"
                        "needs_input" -> "Attend votre réponse"
                        else -> ""
                    },
                )
            "failed" -> WaitOutcome(WaitTone.ERROR, "L’exécution a échoué", "")
            else ->
                WaitOutcome(
                    WaitTone.WARNING,
                    if (status == "cancelled") "L’exécution a été annulée"
                    else "L’exécution a été interrompue",
                    "",
                )
        }
    // A plugin result without its own wording: its conclusion, when it has one.
    val conclusion = observation["conclusion"].text()
    return when {
        conclusion.isEmpty() -> finished
        conclusion == "success" -> WaitOutcome(WaitTone.OK, "Réussi", "")
        else ->
            WaitOutcome(
                if (conclusion in failures) WaitTone.ERROR else WaitTone.WARNING,
                conclusion.replace('_', ' ').replaceFirstChar { it.uppercase() },
                "",
            )
    }
}

/**
 * A group's result: the first result for `any`, every result for `all`, with the most serious tone
 * among them. Results that did not arrive before the deadline are counted, never assumed.
 */
private fun groupOutcome(observation: JsonObject, members: JsonArray): WaitOutcome {
    val received = members.mapNotNull { member ->
        member
            .asObject()
            ?.get("result")
            .asObject()
            ?.takeIf { it.string("state").isNotEmpty() }
            ?.let(::waitOutcome)
    }
    val count = members.size
    if (observation.string("state") == "timed_out")
        return WaitOutcome(
            WaitTone.WARNING,
            "Échéance atteinte",
            if (received.isEmpty()) "Aucun résultat" else "${received.size} sur $count résultats",
        )
    if (observation.string("mode") == "any")
        return received.firstOrNull()?.let {
            WaitOutcome(it.tone, "Premier résultat · ${it.title}", "")
        } ?: WaitOutcome(WaitTone.NEUTRAL, "Attente terminée", "")
    val failed = received.count { it.tone == WaitTone.ERROR }
    val warned = received.count { it.tone == WaitTone.WARNING }
    val succeeded = received.count { it.tone == WaitTone.OK }
    val tone =
        when {
            failed > 0 -> WaitTone.ERROR
            warned > 0 -> WaitTone.WARNING
            succeeded == count -> WaitTone.OK
            else -> WaitTone.NEUTRAL
        }
    return WaitOutcome(
        tone,
        "Les $count résultats sont arrivés",
        listOf(
                if (failed > 0) "$failed en échec" else "",
                if (warned > 0) "$warned à vérifier" else "",
                if (failed + warned > 0) "$succeeded réussis" else "",
            )
            .filter { it.isNotEmpty() }
            .joinToString(" · "),
    )
}

private data class WaitView(
    val tone: WaitTone?,
    val paused: Boolean,
    val icon: ImageVector,
    val title: String,
    val detail: String,
)

private fun waitIcon(kind: WaitKind) =
    when (kind) {
        WaitKind.TIMER -> CairnIcons.Hourglass
        WaitKind.CONDITION -> CairnIcons.Workflow
        WaitKind.RUN,
        WaitKind.TASK -> CairnIcons.Orbit
        WaitKind.GROUP -> CairnIcons.Layers
    }

/**
 * What a result answers, for a card that has no scheduled wakeup to name it. A plugin result only
 * names itself when its plugin gives a short wording.
 */
internal fun observedTarget(observation: JsonObject?): String =
    when {
        observation == null -> ""
        groupResults(observation) != null ->
            groupResults(observation)!!.size.let {
                if (observation.string("mode") == "any") "Premier de $it résultats"
                else "$it résultats"
            }
        observation.string("type") == "task_finished" -> "Tâche déléguée"
        observation.string("type") == "run_finished" -> "Autre exécution"
        observation.string("type") == "time" -> "Heure prévue"
        else ->
            observation["display"].asObject()?.get("short").text().ifBlank {
                if (
                    observation.string("type").isNotEmpty() ||
                        observation.string("pluginId").isNotEmpty()
                )
                    "Une condition"
                else ""
            }
    }

/** Where a wait card stands, from its history and, when known, the conversation's live state. */
internal data class WaitProgress(
    /** Its wakeup is the one the conversation still honours. */
    val current: Boolean,
    /** What Cairn observed, from the history or the pending wakeup; null before any result. */
    val observation: JsonObject?,
    /** Its result arrived or the conversation resumed. */
    val finished: Boolean,
    val replaced: Boolean,
    val cancelled: Boolean,
    val paused: Boolean,
    /** It still counts down. */
    val pending: Boolean,
    /**
     * Once its turn ended, the conversation can be paused before it resumes, even with the result
     * in.
     */
    val pausable: Boolean,
)

internal fun waitProgress(wait: ActivityWait, live: LiveWait?): WaitProgress {
    val wakeup = wait.wakeup
    val current = wakeup != null && live?.wakeup?.id == wakeup.id
    // Plugin condition polling records the result on the pending wakeup too, which matters when
    // its event is unloaded. Another run's result is only computed when the conversation resumes.
    val observation =
        if (wait.observed) wait.observation else if (current) live?.wakeup?.observation else null
    val finished = wait.finished || observation != null
    val replaced = wait.ended == "replaced"
    // Live state, when known, is authoritative: stopping a wait leaves no event in the history.
    val cancelled = if (live != null) !current && !replaced else wait.ended == "cancelled"
    val paused = current && live?.paused == true
    return WaitProgress(
        current,
        observation,
        finished,
        replaced,
        cancelled,
        paused,
        pending = current && !paused && !finished,
        pausable =
            current && !paused && wait.resumedAt == null && live?.settled == true && live.canPause,
    )
}

/** Time left in the Fil, which refreshes every 30 s: seconds would read stale. */
internal fun waitLeft(
    wakeAt: Long,
    condition: Boolean,
    resolved: Boolean,
    now: Long = System.currentTimeMillis(),
): String {
    val left = wakeAt - now
    if (resolved || left <= 0) return "bientôt"
    if (left < 60_000) return "< 1 min"
    return if (condition) "≤ ${timeLeft(left)}" else timeLeft(left)
}

/** One card per wait in the conversation, live while it is the conversation's pending wakeup. */
@OptIn(ExperimentalLayoutApi::class)
@Composable
internal fun WaitCard(wait: ActivityWait) {
    val live = LocalLiveWait.current
    val wakeup = wait.wakeup
    val state = waitProgress(wait, live)
    val current = state.current
    val observation = state.observation
    val finished = state.finished
    val cancelled = state.cancelled
    val paused = state.paused
    val pending = state.pending
    val pausable = state.pausable
    var now by remember { mutableLongStateOf(System.currentTimeMillis()) }
    LaunchedEffect(pending) {
        now = System.currentTimeMillis()
        while (pending) {
            delay(1000)
            now = System.currentTimeMillis()
        }
    }
    val reason = wakeup?.reason ?: wait.reason
    // Cairn updates the pending wakeup, and its plugin's words, as it polls; the event keeps the
    // original.
    val latest = if (current) live?.wakeup else wakeup
    // A grouped wait lists its conditions: the final results once it ended, else Cairn's progress.
    val groupMode =
        waitGroupMode(latest?.condition ?: wakeup?.condition)
            ?: observation
                ?.takeIf { it.string("type") == "group" }
                ?.string("mode")
                ?.takeIf {
                    it == "all" || it == "any"
                }
    val members =
        if (groupMode == null) emptyList()
        else waitMembers(latest ?: wakeup, if (finished) observation else null)
    fun duration() =
        if (wakeup != null && wait.resumedAt != null)
            "Attente de ${waited(wait.resumedAt - wakeup.createdAt)}"
        else ""
    val view =
        when {
            finished && wakeup?.condition == null && observation == null ->
                WaitView(
                    WaitTone.NEUTRAL,
                    false,
                    CairnIcons.Clock,
                    "Reprise à l’heure prévue",
                    duration(),
                )
            finished -> {
                val outcome = waitOutcome(observation)
                val icon =
                    when (outcome.tone) {
                        WaitTone.OK -> CairnIcons.Check
                        WaitTone.ERROR -> CairnIcons.Close
                        WaitTone.WARNING -> CairnIcons.Alert
                        WaitTone.NEUTRAL -> CairnIcons.Clock
                    }
                val time =
                    when {
                        wakeup == null -> "Le résultat était déjà disponible"
                        wait.resumedAt != null -> duration()
                        state.replaced -> "Remplacée par un nouvel appel d’attente"
                        cancelled -> "Attente annulée"
                        paused -> "Reprise quand vous reprendrez la conversation"
                        else -> "Reprise selon la capacité disponible"
                    }
                WaitView(
                    outcome.tone,
                    false,
                    icon,
                    outcome.title,
                    // A finished card still says why, above this, and what it waited for; a
                    // timer's outcome says the latter.
                    listOf(
                            // A group's outcome counts its results already.
                            if (groupMode != null) ""
                            else if (wakeup == null) observedTarget(observation)
                            else if (wakeup.condition != null)
                                waitTarget(latest ?: wakeup, now).short
                            else "",
                            outcome.detail,
                            time,
                        )
                        .filter { it.isNotBlank() }
                        .joinToString(" · "),
                )
            }
            state.replaced ->
                WaitView(
                    null,
                    true,
                    CairnIcons.Hourglass,
                    "Remplacée par un nouvel appel d’attente",
                    "",
                )
            cancelled -> WaitView(null, true, CairnIcons.Hourglass, "Attente annulée", "")
            paused ->
                WaitView(
                    null,
                    true,
                    CairnIcons.Pause,
                    waitTarget(latest ?: wakeup!!, now).title,
                    "",
                )
            // A run's history cannot tell whether the wait still stands: it is shown as recorded.
            else -> {
                val target = waitTarget(latest ?: wakeup!!, now)
                WaitView(
                    if (live != null) null else WaitTone.NEUTRAL,
                    false,
                    waitIcon(target.kind),
                    if (live != null) target.title else recordedTitle(wakeup!!, now),
                    "",
                )
            }
        }
    val (container, content) =
        when {
            view.paused || view.tone == WaitTone.NEUTRAL ->
                MaterialTheme.colorScheme.surfaceContainerHigh to
                    MaterialTheme.colorScheme.onSurfaceVariant
            view.tone == WaitTone.OK -> signal.success.copy(alpha = 0.14f) to signal.success
            view.tone == WaitTone.ERROR ->
                MaterialTheme.colorScheme.errorContainer to MaterialTheme.colorScheme.error
            view.tone == WaitTone.WARNING -> signal.warningSoft to signal.warning
            else ->
                MaterialTheme.colorScheme.primaryContainer to
                    MaterialTheme.colorScheme.onPrimaryContainer
        }
    Surface(
        modifier = Modifier.fillMaxWidth().testTag("chat-wait"),
        color = MaterialTheme.colorScheme.surface,
        shape = RoundedCornerShape(14.dp),
        border =
            androidx.compose.foundation.BorderStroke(
                1.dp,
                MaterialTheme.colorScheme.outlineVariant,
            ),
    ) {
        Column {
            Row(Modifier.padding(12.dp), verticalAlignment = Alignment.Top) {
                Box {
                    Box(
                        Modifier.size(34.dp).clip(RoundedCornerShape(10.dp)).background(container),
                        contentAlignment = Alignment.Center,
                    ) {
                        Icon(view.icon, null, Modifier.size(18.dp), tint = content)
                    }
                    if (pending)
                        Box(
                            Modifier.align(Alignment.TopEnd)
                                .offset(2.dp, (-2).dp)
                                .size(9.dp)
                                .clip(CircleShape)
                                .background(MaterialTheme.colorScheme.primary)
                        )
                }
                Spacer(Modifier.width(12.dp))
                Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
                    Text(view.title, style = MaterialTheme.typography.titleSmall)
                    if (reason.isNotBlank()) WaitReason(reason)
                    if (view.detail.isNotBlank())
                        Text(
                            view.detail,
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    val chips =
                        if (!finished && !cancelled && !state.replaced && wakeup != null)
                            waitTarget(latest ?: wakeup, now).details
                        else emptyList()
                    val retrying = pending && !latest?.lastError.isNullOrBlank()
                    // Plugin conditions are polled: say when Cairn looks again.
                    // Cairn checks them only once the turn has ended, and until the deadline.
                    val polled =
                        pending &&
                            live?.settled == true &&
                            latest != null &&
                            !waitDue(latest, now) &&
                            !retrying &&
                            wakeup?.condition != null &&
                            // A group times each of its conditions in its own list.
                            wakeup.condition.string("type") !in setOf("run_finished", "group")
                    val next = latest?.nextCheckAt
                    val checked =
                        if (!polled) ""
                        else if (next != null && next > now) "Contrôle dans ${timeLeft(next - now)}"
                        else "Contrôle en cours…"
                    if (chips.isNotEmpty() || retrying || checked.isNotEmpty())
                        FlowRow(
                            Modifier.padding(top = 6.dp),
                            horizontalArrangement = Arrangement.spacedBy(6.dp),
                            verticalArrangement = Arrangement.spacedBy(4.dp),
                        ) {
                            chips.forEach { WaitChip(it) }
                            if (checked.isNotEmpty())
                                WaitChip(
                                    checked,
                                    MaterialTheme.colorScheme.primaryContainer,
                                    MaterialTheme.colorScheme.onPrimaryContainer,
                                )
                            if (retrying)
                                WaitChip(
                                    "Contrôle échoué · nouvel essai",
                                    signal.warningSoft,
                                    signal.warning,
                                )
                        }
                }
                if (pending && wakeup != null) {
                    Spacer(Modifier.width(10.dp))
                    Column(horizontalAlignment = Alignment.End) {
                        Text(
                            waitCountdown(wakeup, now),
                            style = MaterialTheme.typography.titleSmall,
                            fontWeight = FontWeight.Bold,
                            color = MaterialTheme.colorScheme.primary,
                        )
                        // A timer names its time in the title already.
                        if (wakeup.condition != null)
                            Text(
                                "échéance ${wakeTime(wakeup.wakeAt, now)}",
                                style = MaterialTheme.typography.labelSmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                    }
                } else if (paused && !finished && wakeup != null) {
                    Spacer(Modifier.width(10.dp))
                    Column(horizontalAlignment = Alignment.End) {
                        Text(
                            "En pause",
                            style = MaterialTheme.typography.titleSmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                        if (wakeup.condition != null)
                            Text(
                                "échéance ${wakeTime(wakeup.wakeAt, now)}",
                                style = MaterialTheme.typography.labelSmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                    }
                } else if (!finished && live == null && wakeup?.condition != null) {
                    Spacer(Modifier.width(10.dp))
                    Text(
                        "Échéance ${wakeTime(wakeup.wakeAt, now)}",
                        style = MaterialTheme.typography.labelSmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
            if (groupMode != null && members.isNotEmpty())
                WaitMembers(
                    members,
                    groupMode,
                    live = pending,
                    finished = finished,
                    timedOut = observation.string("state") == "timed_out",
                    folded = finished && !current,
                    now = now,
                    openRun = live?.openRun,
                    openChat = live?.openChat,
                    modifier = Modifier.padding(start = 58.dp, end = 12.dp, bottom = 12.dp),
                )
            if (pausable && live != null)
                TextButton(
                    onClick = live.pause,
                    enabled = !live.busy,
                    modifier = Modifier.align(Alignment.End).padding(end = 4.dp),
                ) {
                    Icon(CairnIcons.Pause, null, Modifier.size(14.dp))
                    Spacer(Modifier.width(6.dp))
                    Text("Mettre en pause")
                }
            if (pending && wakeup != null) {
                val total = (wakeup.wakeAt - wakeup.createdAt).coerceAtLeast(1)
                LinearProgressIndicator(
                    progress = { ((now - wakeup.createdAt).toFloat() / total).coerceIn(0f, 1f) },
                    modifier = Modifier.fillMaxWidth().height(3.dp),
                    drawStopIndicator = {},
                    gapSize = 0.dp,
                )
            }
        }
    }
}

@Composable
private fun WaitChip(
    text: String,
    container: Color = MaterialTheme.colorScheme.surfaceContainerHigh,
    content: Color = MaterialTheme.colorScheme.onSurfaceVariant,
) {
    Text(
        text,
        Modifier.clip(CircleShape)
            .background(container)
            .padding(horizontal = 8.dp, vertical = 2.dp),
        style = MaterialTheme.typography.labelSmall,
        color = content,
        maxLines = 1,
    )
}

/** Why the agent waits: a long reason is clamped so the card stays small, and can be expanded. */
@Composable
private fun WaitReason(reason: String) {
    var expanded by rememberSaveable(reason) { mutableStateOf(false) }
    var clamped by remember(reason) { mutableStateOf(false) }
    Text(
        reason,
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        maxLines = if (expanded) Int.MAX_VALUE else 3,
        overflow = TextOverflow.Ellipsis,
        onTextLayout = { if (!expanded) clamped = it.hasVisualOverflow },
    )
    if (clamped || expanded)
        Text(
            if (expanded) "Voir moins" else "Voir plus",
            style = MaterialTheme.typography.labelMedium,
            fontWeight = FontWeight.SemiBold,
            color = MaterialTheme.colorScheme.primary,
            modifier =
                Modifier.clickable(role = Role.Button) { expanded = !expanded }
                    .padding(vertical = 4.dp),
        )
}

/**
 * The conditions of a grouped wait, each with its own result as it arrives. A segmented meter sums
 * them up; the list stays open until the conversation moves past the wait.
 */
@Composable
private fun WaitMembers(
    members: List<WaitMember>,
    mode: String,
    live: Boolean,
    finished: Boolean,
    timedOut: Boolean,
    folded: Boolean,
    now: Long,
    openRun: ((String) -> Unit)?,
    openChat: ((String) -> Unit)?,
    modifier: Modifier = Modifier,
) {
    var open by rememberSaveable(folded) { mutableStateOf(!folded) }
    val received = remember(members) { members.count { it.outcome != null } }
    val pageSize = 10
    var selectedPage by rememberSaveable { mutableIntStateOf(0) }
    val pageCount = (members.size - 1) / pageSize + 1
    val page = selectedPage.coerceIn(0, pageCount - 1)
    val start = page * pageSize
    val visible = members.subList(start, minOf(start + pageSize, members.size))
    // Small groups retain individual segments; large groups aggregate by outcome.
    val segments =
        remember(members) {
            if (members.size <= pageSize) members.map { it.outcome?.tone to 1 }
            else members.groupingBy { it.outcome?.tone }.eachCount().map { it.key to it.value }
        }
    // With `any`, the first result in the agent's order is the one that resumed the conversation.
    val first =
        remember(members, mode, finished) {
            if (mode == "any" && finished) members.indexOfFirst { it.outcome != null } else -1
        }
    val pulse by
        rememberInfiniteTransition(label = "wait-members")
            .animateFloat(
                0.25f,
                0.7f,
                infiniteRepeatable(tween(1200), RepeatMode.Reverse),
                label = "pending",
            )
    Column(modifier.testTag("wait-members")) {
        Row(
            Modifier.fillMaxWidth()
                .clip(RoundedCornerShape(8.dp))
                .clickable(
                    role = Role.Button,
                    onClickLabel =
                        if (open) "Masquer les conditions" else "Afficher les conditions",
                ) {
                    open = !open
                }
                .padding(vertical = 6.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            if (mode == "any") WaitChip("Un seul", signal.attentionSoft, signal.attention)
            else
                WaitChip(
                    "Tous",
                    MaterialTheme.colorScheme.primaryContainer,
                    MaterialTheme.colorScheme.onPrimaryContainer,
                )
            Row(Modifier.weight(1f), horizontalArrangement = Arrangement.spacedBy(4.dp)) {
                segments.forEachIndexed { index, (tone, count) ->
                    val color =
                        when (tone) {
                            WaitTone.OK -> signal.success
                            WaitTone.ERROR -> MaterialTheme.colorScheme.error
                            WaitTone.WARNING -> signal.warning
                            WaitTone.NEUTRAL -> MaterialTheme.colorScheme.outline
                            null ->
                                if (live) MaterialTheme.colorScheme.primary.copy(alpha = pulse)
                                else MaterialTheme.colorScheme.surfaceContainerHighest
                        }
                    Box(
                        Modifier.weight(count.toFloat())
                            .height(6.dp)
                            .clip(CircleShape)
                            .background(color)
                            .then(
                                if (members.size <= pageSize && index == first)
                                    Modifier.border(1.dp, signal.attention, CircleShape)
                                else Modifier
                            )
                    )
                }
            }
            Text(
                "$received/${members.size}",
                style = MaterialTheme.typography.labelMedium,
                fontWeight = FontWeight.SemiBold,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            Icon(
                CairnIcons.Down,
                null,
                Modifier.size(16.dp).rotate(if (open) 180f else 0f),
                tint = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        AnimatedVisibility(open) {
            Surface(
                Modifier.fillMaxWidth().padding(top = 4.dp),
                color = MaterialTheme.colorScheme.surfaceContainerLow,
                shape = RoundedCornerShape(12.dp),
                border =
                    androidx.compose.foundation.BorderStroke(
                        1.dp,
                        MaterialTheme.colorScheme.outlineVariant,
                    ),
            ) {
                Column {
                    visible.forEachIndexed { index, member ->
                        if (index > 0)
                            HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
                        WaitMemberRow(
                            member,
                            live,
                            finished,
                            timedOut,
                            start + index == first,
                            now,
                            openRun,
                            openChat,
                        )
                    }
                    if (pageCount > 1) {
                        HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
                        Row(
                            Modifier.fillMaxWidth().padding(horizontal = 4.dp),
                            horizontalArrangement = Arrangement.SpaceBetween,
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            TextButton(onClick = { selectedPage = page - 1 }, enabled = page > 0) {
                                Text("Précédent")
                            }
                            Text(
                                "${start + 1}–${minOf(start + pageSize, members.size)} sur ${members.size}",
                                style = MaterialTheme.typography.labelSmall,
                            )
                            TextButton(
                                onClick = { selectedPage = page + 1 },
                                enabled = page + 1 < pageCount,
                            ) {
                                Text("Suivant")
                            }
                        }
                    }
                }
            }
        }
    }
}

@Composable
private fun WaitMemberRow(
    member: WaitMember,
    live: Boolean,
    finished: Boolean,
    timedOut: Boolean,
    first: Boolean,
    now: Long,
    openRun: ((String) -> Unit)?,
    openChat: ((String) -> Unit)?,
) {
    val outcome = member.outcome
    val status =
        when {
            outcome != null ->
                listOf(outcome.title, outcome.detail).filter { it.isNotBlank() }.joinToString(" · ")
            finished ->
                if (timedOut) "Aucun résultat avant l’échéance"
                else "Inutile : un autre résultat est arrivé avant"
            !live -> "Résultat attendu"
            member.lastError != null -> "Contrôle échoué · nouvel essai"
            member.kind in setOf(WaitKind.RUN, WaitKind.TASK) -> "Attente de sa fin"
            member.nextCheckAt != null ->
                if (member.nextCheckAt > now) "Contrôle dans ${timeLeft(member.nextCheckAt - now)}"
                else "Contrôle en cours…"
            else -> "Résultat attendu"
        }
    val tint =
        when {
            outcome?.tone == WaitTone.OK -> signal.success
            outcome?.tone == WaitTone.ERROR -> MaterialTheme.colorScheme.error
            outcome?.tone == WaitTone.WARNING -> signal.warning
            live && member.lastError != null -> signal.warning
            else -> MaterialTheme.colorScheme.onSurfaceVariant
        }
    val openTarget =
        when {
            member.chatId != null && openChat != null -> ({ openChat(member.chatId) })
            member.runId != null && openRun != null -> ({ openRun(member.runId) })
            else -> null
        }
    val openLabel = if (member.chatId != null) "Ouvrir la tâche" else "Ouvrir l’exécution"
    Row(
        Modifier.fillMaxWidth()
            .testTag("wait-member")
            .then(
                if (openTarget != null)
                    Modifier.clickable(
                        role = Role.Button,
                        onClickLabel = openLabel,
                        onClick = openTarget,
                    )
                else Modifier
            )
            .alpha(if (finished && outcome == null) 0.6f else 1f)
            .padding(horizontal = 12.dp, vertical = 9.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        Box(Modifier.size(22.dp), contentAlignment = Alignment.Center) {
            when {
                outcome == null && live ->
                    CircularProgressIndicator(
                        Modifier.size(18.dp),
                        strokeWidth = 2.dp,
                        trackColor = MaterialTheme.colorScheme.outlineVariant,
                    )
                outcome == null ->
                    Box(
                        Modifier.size(18.dp)
                            .border(1.dp, MaterialTheme.colorScheme.outline, CircleShape)
                    )
                else -> {
                    val (container, content, icon) =
                        when (outcome.tone) {
                            WaitTone.OK ->
                                Triple(
                                    signal.success,
                                    MaterialTheme.colorScheme.surface,
                                    CairnIcons.Check,
                                )
                            WaitTone.ERROR ->
                                Triple(
                                    MaterialTheme.colorScheme.error,
                                    MaterialTheme.colorScheme.onError,
                                    CairnIcons.Close,
                                )
                            WaitTone.WARNING ->
                                Triple(signal.warningSoft, signal.warning, CairnIcons.Alert)
                            WaitTone.NEUTRAL ->
                                Triple(
                                    MaterialTheme.colorScheme.surfaceContainerHighest,
                                    MaterialTheme.colorScheme.onSurfaceVariant,
                                    CairnIcons.Check,
                                )
                        }
                    Box(
                        Modifier.size(22.dp).clip(CircleShape).background(container),
                        contentAlignment = Alignment.Center,
                    ) {
                        Icon(icon, null, Modifier.size(13.dp), tint = content)
                    }
                }
            }
        }
        Column(Modifier.weight(1f)) {
            Row(
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(6.dp),
            ) {
                Text(
                    member.title,
                    Modifier.weight(1f, fill = false),
                    style = MaterialTheme.typography.labelLarge,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
                member.details.take(2).forEach { WaitChip(it) }
                if (first) WaitChip("Premier", signal.attentionSoft, signal.attention)
            }
            Text(
                status,
                style = MaterialTheme.typography.bodySmall,
                color = tint,
                maxLines = 2,
                overflow = TextOverflow.Ellipsis,
            )
        }
        if (openTarget != null)
            Icon(
                CairnIcons.Right,
                null,
                Modifier.size(16.dp),
                tint = MaterialTheme.colorScheme.onSurfaceVariant,
            )
    }
}

package build.cairn.app.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import build.cairn.app.data.ChatWakeup
import build.cairn.app.data.RunEvent
import build.cairn.app.data.wireJson
import kotlinx.coroutines.delay
import kotlinx.serialization.json.*

/** What a conversation waits for: a timer, PR checks, a workflow run or another Cairn run. */
internal enum class WaitKind {
    TIMER,
    PR,
    WORKFLOW,
    RUN,
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

/** A durable wait the agent scheduled with `sleep` or `wait_for`, then how it ended. */
internal data class ActivityWait(
    /** Null when the condition had already completed and the agent never waited. */
    val wakeup: ChatWakeup?,
    /** What Cairn observed once the condition completed or the deadline passed. */
    val observation: JsonObject? = null,
    val observed: Boolean = false,
    /** When the conversation resumed. */
    val resumedAt: Long? = null,
    /** A message from the user ("message"), or another wait ("replaced"), ended it first. */
    val ended: String? = null,
) {
    val finished
        get() = resumedAt != null || observed
}

/** The conversation's pending wakeup, which keeps its wait card live, and how to pause it. */
internal data class LiveWait(
    val wakeup: ChatWakeup?,
    val paused: Boolean,
    val pause: () -> Unit = {},
)

internal val LocalLiveWait = compositionLocalOf<LiveWait?> { null }

private fun shortSha(value: String) = value.take(7)

private fun JsonObject?.number(key: String) = (this?.get(key) as? JsonPrimitive)?.longOrNull

// Codex names the workspace server; Claude and OpenCode prefix the tool with it.
private val workspaceWaitTool = Regex("^(?:mcp__)?cairn_workspace(?:__|_|\\.)(?:sleep|wait_for)$")

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
            java.util.Locale.getDefault(),
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

internal fun waitTarget(wakeup: ChatWakeup, now: Long = System.currentTimeMillis()): WaitTarget {
    val condition = wakeup.condition
    return when (condition.string("type")) {
        "github_pr_checks" -> {
            val number = condition.number("prNumber")
            WaitTarget(
                WaitKind.PR,
                "Attente des checks requis de la PR #$number",
                "Checks PR #$number",
                listOf(shortSha(condition.string("headSha"))),
            )
        }
        "github_workflow" ->
            WaitTarget(
                WaitKind.WORKFLOW,
                "Attente d’un workflow GitHub",
                "Workflow GitHub",
                listOf(
                    "Exécution ${condition.number("runId")}",
                    shortSha(condition.string("headSha")),
                ),
            )
        "run_finished" ->
            WaitTarget(
                WaitKind.RUN,
                "Attente de la fin d’une autre exécution",
                "Autre exécution",
                emptyList(),
            )
        else -> {
            val at = "Reprise à ${wakeTime(wakeup.wakeAt, now)}"
            WaitTarget(WaitKind.TIMER, at, at, emptyList())
        }
    }
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

/** What Cairn observed when a wait ended; null after a plain timer. */
internal fun waitOutcome(observation: JsonObject?): WaitOutcome {
    val state = observation.string("state")
    val finished = WaitOutcome(WaitTone.NEUTRAL, "Attente terminée", "")
    if (observation == null || state.isEmpty()) return finished
    when (state) {
        "timed_out" -> return WaitOutcome(WaitTone.WARNING, "Échéance atteinte sans résultat", "")
        "unavailable" ->
            return WaitOutcome(
                WaitTone.WARNING,
                "Résultat indisponible",
                observation.string("message"),
            )
        "superseded" ->
            return WaitOutcome(
                WaitTone.WARNING,
                "Un nouveau commit a été poussé",
                "${shortSha(observation.string("headSha"))} → ${shortSha(observation.string("currentHeadSha"))}",
            )
        "closed" -> {
            val merged = observation.string("pullRequestState") == "MERGED"
            return WaitOutcome(
                WaitTone.WARNING,
                "La PR #${observation.number("prNumber")} a été ${if (merged) "fusionnée" else "fermée"}",
                "",
            )
        }
    }
    return when (observation.string("type")) {
        "run_finished" ->
            when (val status = observation.string("status")) {
                "succeeded" ->
                    WaitOutcome(
                        WaitTone.OK,
                        "L’exécution est terminée",
                        when (observation.string("outcomeStatus")) {
                            "blocked" -> "Bloquée"
                            "needs_input" -> "Attend une réponse"
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
        "github_workflow" -> {
            val conclusion = observation.string("conclusion")
            val attempt = observation.number("runAttempt")
            val detail = if (attempt != null && attempt > 1) "Tentative $attempt" else ""
            when (conclusion) {
                "success" -> WaitOutcome(WaitTone.OK, "Workflow réussi", detail)
                "failure",
                "startup_failure",
                "timed_out" ->
                    WaitOutcome(
                        WaitTone.ERROR,
                        "Workflow en échec",
                        listOf(friendlyStatus(conclusion), detail)
                            .filter { it.isNotBlank() }
                            .joinToString(" · "),
                    )
                else ->
                    WaitOutcome(
                        WaitTone.WARNING,
                        "Workflow : ${friendlyStatus(conclusion)}",
                        detail,
                    )
            }
        }
        "github_pr_checks" -> {
            val checks = observation["checks"].asArray().orEmpty().mapNotNull { it.asObject() }
            fun named(wanted: String): String {
                val names = checks.filter { it.string("state") == wanted }.map { it.string("name") }
                return if (names.size > 3) "${names.take(3).joinToString(", ")} +${names.size - 3}"
                else names.joinToString(", ")
            }
            when (observation.string("conclusion")) {
                "success" -> {
                    val count = observation.number("requiredCount") ?: 0
                    WaitOutcome(
                        WaitTone.OK,
                        "Checks requis réussis",
                        if (count > 0) "$count check${if (count > 1) "s" else ""}" else "",
                    )
                }
                "failure" -> WaitOutcome(WaitTone.ERROR, "Checks requis en échec", named("failure"))
                "cancelled" ->
                    WaitOutcome(WaitTone.WARNING, "Checks requis annulés", named("cancelled"))
                "no_required_checks" ->
                    WaitOutcome(WaitTone.WARNING, "Aucun check requis sur cette PR", "")
                else -> finished
            }
        }
        else -> finished
    }
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
        WaitKind.PR -> CairnIcons.PullRequest
        WaitKind.WORKFLOW -> CairnIcons.Workflow
        WaitKind.RUN -> CairnIcons.Orbit
    }

/** One card per wait in the conversation, live while it is the conversation's pending wakeup. */
@OptIn(ExperimentalLayoutApi::class)
@Composable
internal fun WaitCard(wait: ActivityWait) {
    val live = LocalLiveWait.current
    val wakeup = wait.wakeup
    val current = wakeup != null && live?.wakeup?.id == wakeup.id
    val pending = current && live?.paused != true && !wait.finished
    var now by remember { mutableLongStateOf(System.currentTimeMillis()) }
    LaunchedEffect(pending) {
        now = System.currentTimeMillis()
        while (pending) {
            delay(1000)
            now = System.currentTimeMillis()
        }
    }
    val reason = wakeup?.reason.orEmpty()
    fun duration() =
        if (wakeup != null && wait.resumedAt != null)
            "Attente de ${waited(wait.resumedAt - wakeup.createdAt)}"
        else ""
    val view =
        when {
            wait.finished && wakeup?.condition == null && wait.observation == null ->
                WaitView(
                    WaitTone.NEUTRAL,
                    false,
                    CairnIcons.Clock,
                    "Reprise à l’heure prévue",
                    duration(),
                )
            wait.finished -> {
                val outcome = waitOutcome(wait.observation)
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
                        else -> "Reprise selon la capacité disponible"
                    }
                WaitView(
                    outcome.tone,
                    false,
                    icon,
                    outcome.title,
                    listOf(outcome.detail, time).filter { it.isNotBlank() }.joinToString(" · "),
                )
            }
            wait.ended == "message" ->
                WaitView(
                    null,
                    true,
                    CairnIcons.Hourglass,
                    "Attente annulée par votre message",
                    reason,
                )
            wait.ended == "replaced" ->
                WaitView(
                    null,
                    true,
                    CairnIcons.Hourglass,
                    "Remplacée par une nouvelle attente",
                    reason,
                )
            live != null && !current ->
                WaitView(null, true, CairnIcons.Hourglass, "Attente annulée", reason)
            current && live?.paused == true ->
                WaitView(
                    null,
                    true,
                    CairnIcons.Pause,
                    "Attente en pause",
                    "L’échéance reste valable. Reprenez la conversation pour continuer.",
                )
            else -> {
                val target = waitTarget(wakeup!!, now)
                WaitView(null, false, waitIcon(target.kind), target.title, reason)
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
                    if (view.detail.isNotBlank())
                        Text(
                            view.detail,
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    val chips =
                        if (pending && wakeup != null)
                            waitTarget(wakeup, now).details.filter { it.isNotBlank() }
                        else emptyList()
                    // Cairn updates the pending wakeup as it polls; the event keeps the original.
                    val latest = if (current) live?.wakeup else wakeup
                    val retrying = pending && !latest?.lastError.isNullOrBlank()
                    // GitHub conditions are polled: say when Cairn looks again.
                    val polled =
                        pending &&
                            !retrying &&
                            wakeup?.condition.string("type") in
                                setOf("github_pr_checks", "github_workflow")
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
                                    "GitHub injoignable · nouvel essai",
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
                        Text(
                            if (wakeup.condition != null) "échéance ${wakeTime(wakeup.wakeAt, now)}"
                            else wakeTime(wakeup.wakeAt, now),
                            style = MaterialTheme.typography.labelSmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                } else if (!wait.finished && live == null && wakeup != null) {
                    Spacer(Modifier.width(10.dp))
                    Text(
                        "${if (wakeup.condition != null) "Échéance" else "Reprise"} ${wakeTime(wakeup.wakeAt, now)}",
                        style = MaterialTheme.typography.labelSmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
            if (pending && live != null)
                TextButton(
                    onClick = live.pause,
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

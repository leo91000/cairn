package build.cairn.app.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
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
)

internal val LocalLiveWait = compositionLocalOf<LiveWait?> { null }

private fun shortSha(value: String) = value.take(7)

private fun JsonObject?.number(key: String) = (this?.get(key) as? JsonPrimitive)?.longOrNull

// Codex names the workspace server; Claude and OpenCode prefix the tool with it.
private val workspaceWaitTool = Regex("^(?:mcp__)?cairn_workspace(?:__|_|\\.)(?:sleep|wait_for)$")

/** A tool call's arguments, which some providers record as JSON text. */
internal fun toolArguments(item: JsonObject?): JsonObject? =
    when (val value = item?.get("arguments")) {
        is JsonObject -> value
        is JsonPrimitive ->
            value.contentOrNull?.let {
                runCatching { wireJson.parseToJsonElement(it).jsonObject }.getOrNull()
            }
        else -> null
    }

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
internal const val IMMEDIATE_WAIT_RESULT = "Wait condition already completed"

/** What Cairn observed when a wait ended; a plain timer observes nothing and reads as neutral. */
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
        "github_workflow" -> {
            val conclusion = observation.string("conclusion")
            val attempt = observation.number("runAttempt")
            val detail = if (attempt != null && attempt > 1) "Tentative $attempt" else ""
            // GitHub's conclusions, in French: a plain failure needs no detail.
            when (conclusion) {
                "success" -> WaitOutcome(WaitTone.OK, "Workflow réussi", detail)
                "failure",
                "startup_failure",
                "timed_out" ->
                    WaitOutcome(
                        WaitTone.ERROR,
                        "Workflow en échec",
                        listOf(
                                when (conclusion) {
                                    "startup_failure" -> "échec au démarrage"
                                    "timed_out" -> "délai dépassé"
                                    else -> ""
                                },
                                detail,
                            )
                            .filter { it.isNotBlank() }
                            .joinToString(" · "),
                    )
                else ->
                    WaitOutcome(
                        WaitTone.WARNING,
                        when (conclusion) {
                            "cancelled" -> "Workflow annulé"
                            "skipped" -> "Workflow ignoré"
                            "neutral" -> "Workflow neutre"
                            "action_required" -> "Workflow : action requise"
                            "stale" -> "Workflow périmé"
                            else -> "Workflow : ${friendlyStatus(conclusion)}"
                        },
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

/** What a result answers, for a card that has no scheduled wakeup to name it. */
internal fun observedTarget(observation: JsonObject?): String =
    when (observation.string("type")) {
        "github_pr_checks" -> "Checks PR #${observation.number("prNumber")}"
        "github_workflow" -> "Workflow GitHub"
        "run_finished" -> "Autre exécution"
        else -> ""
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
    // GitHub polling records the result on the pending wakeup too, which matters when its event
    // is unloaded. Another run's result is only computed when the conversation resumes.
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
        pausable = current && !paused && wait.resumedAt == null && live?.settled == true,
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
                            if (wakeup == null) observedTarget(observation)
                            else if (wakeup.condition != null) waitTarget(wakeup, now).short
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
            paused -> WaitView(null, true, CairnIcons.Pause, waitTarget(wakeup!!, now).title, "")
            // A run's history cannot tell whether the wait still stands: it is shown as recorded.
            else -> {
                val target = waitTarget(wakeup!!, now)
                WaitView(
                    if (live != null) null else WaitTone.NEUTRAL,
                    false,
                    waitIcon(target.kind),
                    if (live != null) target.title else recordedTitle(wakeup, now),
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
                            waitTarget(wakeup, now).details.filter { it.isNotBlank() }
                        else emptyList()
                    // Cairn updates the pending wakeup as it polls; the event keeps the original.
                    val latest = if (current) live?.wakeup else wakeup
                    val retrying = pending && !latest?.lastError.isNullOrBlank()
                    // GitHub conditions are polled: say when Cairn looks again.
                    // Cairn checks GitHub only once the turn has ended, and until the deadline.
                    val polled =
                        pending &&
                            live?.settled == true &&
                            latest != null &&
                            !waitDue(latest, now) &&
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

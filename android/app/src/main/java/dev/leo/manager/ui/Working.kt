package dev.leo.manager.ui

import androidx.compose.animation.core.FastOutSlowInEasing
import androidx.compose.animation.core.LinearEasing
import androidx.compose.animation.core.RepeatMode
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.tween
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.scale
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.RoundRect
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.PathEffect
import androidx.compose.ui.graphics.PathMeasure
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import dev.leo.manager.data.RunEvent
import kotlinx.coroutines.delay

/**
 * What the working indicator says: the step in progress, or the last one reached. [waiting] lists
 * the background tasks an idle agent waits for; [since] is then when it began waiting.
 */
internal data class WorkingStep(
    val title: String,
    val detail: String,
    val since: Long?,
    val waiting: List<String>? = null,
)

/** The background tasks an idle agent waits for, and when it announced them. */
internal data class BackgroundWait(val tasks: List<String>, val since: Long)

/** Events that show the agent active again after it announced a background wait. */
private fun RunEvent.resumesWork() =
    type == "chat.user" ||
        type == "thread.started" ||
        type.startsWith("turn.") ||
        type.startsWith("item.")

/**
 * The background tasks the agent waits for, when its latest announcement still holds: the agent
 * finished responding and its run stays open until those tasks notify it.
 */
internal fun backgroundWait(events: List<RunEvent>): BackgroundWait? {
    val index = events.indexOfLast { it.type == "turn.waiting" }
    if (index < 0 || (index + 1 until events.size).any { events[it].resumesWork() }) return null

    val announcement = events[index]
    val tasks =
        announcement.activityData()?.get("tasks").asArray().orEmpty().map {
            it.asObject()?.get("description").plain().trim().ifEmpty { "Tâche en arrière-plan" }
        }
    return if (tasks.isEmpty()) null else BackgroundWait(tasks, announcement.createdAt)
}

/** The indicator for an agent that only waits for its background tasks. */
internal fun waitingStep(wait: BackgroundWait): WorkingStep {
    val count = wait.tasks.size
    return WorkingStep(
        if (count == 1) "En attente d’une tâche en arrière-plan"
        else "En attente de $count tâches en arrière-plan",
        wait.tasks.joinToString(" · "),
        wait.since,
        waiting = wait.tasks,
    )
}

/**
 * The agent's current step since the latest user message: an in-progress action is named with its
 * command or files; otherwise the agent is simply working, with its last completed step.
 */
internal fun workingStep(events: List<RunEvent>, agent: String, fallbackStart: Long?): WorkingStep {
    val turnStart = events.indexOfLast { it.type == "chat.user" }
    val turn = if (turnStart >= 0) events.drop(turnStart + 1) else events
    val since = events.getOrNull(turnStart)?.createdAt?.takeIf { it > 0 } ?: fallbackStart
    val latest =
        turn
            .asReversed()
            .asSequence()
            .filter { !it.isMessage() }
            .map(::presentActivity)
            .firstOrNull { it.kind != ActivityKind.NOTICE }
    val name = agent.ifBlank { "L’agent" }
    return when {
        latest == null -> WorkingStep("$name travaille", "", since)
        latest.state == "En cours" && !latest.failed ->
            WorkingStep(latest.title, latest.command.ifBlank { latest.subtitle }, since)
        else -> WorkingStep("$name travaille", "Dernière étape : ${latest.title}", since)
    }
}

/** Seconds precision for the live indicator: "45 s", "2 min 14 s", "1 h 05". */
internal fun liveElapsed(start: Long?, now: Long = System.currentTimeMillis()): String {
    if (start == null || start <= 0) return ""
    val seconds = ((now - start).coerceAtLeast(0) / 1000)
    return when {
        seconds < 60 -> "$seconds s"
        seconds < 3600 -> "${seconds / 60} min ${(seconds % 60).toString().padStart(2, '0')} s"
        else -> "${seconds / 3600} h ${((seconds % 3600) / 60).toString().padStart(2, '0')}"
    }
}

/** The current time, refreshed every second for elapsed-time labels. */
@Composable
private fun rememberNow(key: Any?): Long {
    var now by remember { mutableLongStateOf(System.currentTimeMillis()) }
    LaunchedEffect(key) {
        while (true) {
            now = System.currentTimeMillis()
            delay(1000)
        }
    }
    return now
}

/**
 * "Souffle": a breathing halo, a light sweep across the current step and the elapsed time.
 * Animations follow the system animation scale, so they stop when motion is turned off. "Veille"
 * replaces it while the agent only waits for its background tasks.
 */
@Composable
internal fun WorkingIndicator(step: WorkingStep, modifier: Modifier = Modifier) {
    if (step.waiting != null) {
        WaitingIndicator(step, step.waiting, modifier)
        return
    }
    val now = rememberNow(step.since)
    val primary = MaterialTheme.colorScheme.primary
    val transition = rememberInfiniteTransition(label = "working")
    val ring by
        transition.animateFloat(
            0f,
            1f,
            infiniteRepeatable(tween(1600, easing = FastOutSlowInEasing)),
            label = "ring",
        )
    val breath by
        transition.animateFloat(
            0.92f,
            1.06f,
            infiniteRepeatable(tween(800, easing = FastOutSlowInEasing), RepeatMode.Reverse),
            label = "breath",
        )
    val sweep by
        transition.animateFloat(
            -520f,
            1040f,
            infiniteRepeatable(tween(1400, easing = LinearEasing)),
            label = "sweep",
        )
    val elapsed = liveElapsed(step.since, now)
    val base = MaterialTheme.colorScheme.onSurface
    Row(
        modifier.fillMaxWidth().testTag("agent-working").clearAndSetSemantics {
            contentDescription =
                listOf(step.title, step.detail).filter { it.isNotBlank() }.joinToString(" : ")
            liveRegion = LiveRegionMode.Polite
        },
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Box(Modifier.size(36.dp), contentAlignment = Alignment.Center) {
            Canvas(Modifier.fillMaxSize()) {
                val radius = size.minDimension / 2
                drawCircle(
                    primary.copy(alpha = 0.35f * (1 - ring)),
                    radius * (0.55f + 0.45f * ring),
                    style = Stroke(2.dp.toPx()),
                )
                drawCircle(primary.copy(alpha = 0.12f), radius * 0.62f)
            }
            Box(
                Modifier.size(20.dp).scale(breath).clip(CircleShape).background(primary),
                contentAlignment = Alignment.Center,
            ) {
                Icon(
                    LeoIcons.Steer,
                    null,
                    Modifier.size(11.dp),
                    tint = MaterialTheme.colorScheme.onPrimary,
                )
            }
        }
        Spacer(Modifier.width(10.dp))
        Column(Modifier.weight(1f)) {
            Text(
                "${step.title}…",
                style =
                    MaterialTheme.typography.titleSmall.copy(
                        brush =
                            Brush.linearGradient(
                                listOf(base.copy(alpha = 0.55f), base, base.copy(alpha = 0.55f)),
                                start = Offset(sweep - 170f, 0f),
                                end = Offset(sweep + 170f, 0f),
                            )
                    ),
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            if (step.detail.isNotBlank())
                Text(
                    step.detail,
                    style = MaterialTheme.typography.labelSmall,
                    fontFamily =
                        if (step.detail.startsWith("Dernière étape")) null
                        else FontFamily.Monospace,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
        }
        if (elapsed.isNotBlank()) {
            Spacer(Modifier.width(8.dp))
            Text(
                elapsed,
                style = MaterialTheme.typography.labelMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
}

private const val SHOWN_TASKS = 3

/**
 * "Veille": the agent finished responding and only waits for its background tasks. A slow dashed
 * ring and a calm dot replace the breath, so a long wait reads as a process running rather than an
 * agent thinking for hours. The time counts from the start of the wait.
 */
@Composable
private fun WaitingIndicator(step: WorkingStep, tasks: List<String>, modifier: Modifier) {
    val now = rememberNow(step.since)
    val transition = rememberInfiniteTransition(label = "waiting")
    val turn by
        transition.animateFloat(
            0f,
            360f,
            infiniteRepeatable(tween(9000, easing = LinearEasing)),
            label = "turn",
        )
    val glow by
        transition.animateFloat(
            0.35f,
            1f,
            infiniteRepeatable(tween(1200, easing = FastOutSlowInEasing), RepeatMode.Reverse),
            label = "glow",
        )
    val ring = MaterialTheme.colorScheme.outline.copy(alpha = 0.7f)
    val accent = MaterialTheme.colorScheme.primary
    val muted = MaterialTheme.colorScheme.onSurfaceVariant
    val resumes =
        if (tasks.size == 1) "L’agent reprend quand elle se termine."
        else "L’agent reprend quand elles se terminent."
    val elapsed = liveElapsed(step.since, now)
    Row(
        modifier.fillMaxWidth().testTag("agent-waiting").clearAndSetSemantics {
            contentDescription = "${step.title} : ${step.detail}. $resumes"
            liveRegion = LiveRegionMode.Polite
        },
        verticalAlignment = Alignment.Top,
    ) {
        Box(Modifier.size(36.dp), contentAlignment = Alignment.Center) {
            // The ring turns in the draw phase, so the slow rotation never recomposes the row.
            Canvas(Modifier.fillMaxSize().graphicsLayer { rotationZ = turn }) {
                val stroke = 1.5.dp.toPx()
                drawCircle(
                    ring,
                    size.minDimension / 2 - 2.dp.toPx() - stroke / 2,
                    style =
                        Stroke(
                            stroke,
                            pathEffect =
                                PathEffect.dashPathEffect(floatArrayOf(3.dp.toPx(), 3.dp.toPx())),
                        ),
                )
            }
            Box(
                Modifier.size(20.dp)
                    .clip(CircleShape)
                    .background(MaterialTheme.colorScheme.surfaceVariant),
                contentAlignment = Alignment.Center,
            ) {
                Icon(
                    LeoIcons.Terminal,
                    null,
                    Modifier.size(11.dp),
                    tint = MaterialTheme.colorScheme.onSurface,
                )
            }
        }
        Spacer(Modifier.width(10.dp))
        Column(Modifier.weight(1f).padding(top = 2.dp)) {
            Text(
                step.title,
                style = MaterialTheme.typography.titleSmall,
                color = MaterialTheme.colorScheme.onSurface,
                maxLines = 2,
                overflow = TextOverflow.Ellipsis,
            )
            FlowRow(
                Modifier.padding(top = 6.dp),
                horizontalArrangement = Arrangement.spacedBy(6.dp),
                verticalArrangement = Arrangement.spacedBy(6.dp),
            ) {
                tasks.take(SHOWN_TASKS).forEach { task ->
                    WaitingTask {
                        Box(
                            Modifier.size(6.dp)
                                .clip(CircleShape)
                                .background(accent.copy(alpha = glow))
                        )
                        Spacer(Modifier.width(6.dp))
                        Text(
                            task,
                            style = MaterialTheme.typography.labelMedium,
                            color = MaterialTheme.colorScheme.onSurface,
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis,
                        )
                    }
                }
                if (tasks.size > SHOWN_TASKS)
                    WaitingTask {
                        Text(
                            "+${tasks.size - SHOWN_TASKS}",
                            style = MaterialTheme.typography.labelMedium,
                            color = MaterialTheme.colorScheme.onSurface,
                        )
                    }
            }
            Text(
                resumes,
                Modifier.padding(top = 6.dp),
                style = MaterialTheme.typography.labelSmall,
                color = muted,
            )
        }
        if (elapsed.isNotBlank()) {
            Spacer(Modifier.width(8.dp))
            Text(
                elapsed,
                Modifier.padding(top = 2.dp),
                style = MaterialTheme.typography.labelMedium,
                color = muted,
            )
        }
    }
}

/** A background task pill of the "Veille" indicator. */
@Composable
private fun WaitingTask(content: @Composable RowScope.() -> Unit) {
    Row(
        Modifier.clip(CircleShape)
            .background(MaterialTheme.colorScheme.surfaceContainerHigh)
            .border(1.dp, MaterialTheme.colorScheme.outlineVariant, CircleShape)
            .padding(horizontal = 8.dp, vertical = 2.dp),
        verticalAlignment = Alignment.CenterVertically,
        content = content,
    )
}

/**
 * List avatar of an agent at work, as on the web: a comet with a glowing head travels along the
 * edge of the avatar over a faint outline that breathes. The ring itself stays still, so the
 * rounded square never appears to spin. Follows the system animation scale like the conversation
 * indicator.
 */
@Composable
internal fun WorkingAvatar(name: String, key: String, size: Dp) {
    val primary = MaterialTheme.colorScheme.primary
    val transition = rememberInfiniteTransition(label = "avatar-working")
    val travel by
        transition.animateFloat(
            0f,
            1f,
            infiniteRepeatable(tween(1800, easing = LinearEasing)),
            label = "orbit",
        )
    val glow by
        transition.animateFloat(
            0.09f,
            0.22f,
            infiniteRepeatable(tween(900, easing = FastOutSlowInEasing), RepeatMode.Reverse),
            label = "glow",
        )
    val ring = remember { Path() }
    val measure = remember { PathMeasure() }
    val piece = remember { Path() }
    // The ring overflows the avatar so working rows stay aligned with the others.
    Box(Modifier.size(size).testTag("chat-working-avatar"), contentAlignment = Alignment.Center) {
        Canvas(Modifier.requiredSize(size + 8.dp)) {
            val stroke = 2.dp.toPx()
            val inset = stroke / 2
            val corner = CornerRadius((size * 0.3f + 4.dp).toPx())
            ring.reset()
            // Clockwise, like the web orbit.
            ring.addRoundRect(
                RoundRect(inset, inset, this.size.width - inset, this.size.height - inset, corner),
                Path.Direction.Clockwise,
            )
            drawPath(ring, primary.copy(alpha = glow), style = Stroke(stroke))
            measure.setPath(ring, false)
            val length = measure.length
            val head = travel * length
            val tail = length * 0.4f
            // The comet's tail fades and thins behind its head, following the corners exactly.
            val steps = 24
            for (index in 0 until steps) {
                val start = head - tail * (index + 1) / steps
                val end = head - tail * index / steps
                val fade = 1f - index.toFloat() / steps
                piece.reset()
                when {
                    end <= 0f -> measure.getSegment(start + length, end + length, piece, true)
                    start < 0f -> {
                        measure.getSegment(start + length, length, piece, true)
                        measure.getSegment(0f, end, piece, true)
                    }
                    else -> measure.getSegment(start, end, piece, true)
                }
                // Flat ends keep the overlapping pieces from beading; only the head is rounded.
                drawPath(
                    piece,
                    primary.copy(alpha = fade * fade),
                    style =
                        Stroke(
                            stroke * (0.55f + 0.45f * fade),
                            cap = if (index == 0) StrokeCap.Round else StrokeCap.Butt,
                        ),
                )
            }
            // A soft halo marks the head of the comet.
            val position = measure.getPosition(head)
            drawCircle(
                Brush.radialGradient(
                    listOf(primary.copy(alpha = 0.55f), primary.copy(alpha = 0f)),
                    position,
                    stroke * 3.2f,
                ),
                stroke * 3.2f,
                position,
            )
            drawCircle(primary, stroke * 0.9f, position)
        }
        AgentAvatar(name, key, size)
    }
}

/** "En cours" followed by three dots rising in a wave. */
@Composable
internal fun WorkingLabel(text: String, modifier: Modifier = Modifier) {
    val transition = rememberInfiniteTransition(label = "label-working")
    val phase by
        transition.animateFloat(
            0f,
            1f,
            infiniteRepeatable(tween(1200, easing = LinearEasing)),
            label = "wave",
        )
    val color = MaterialTheme.colorScheme.primary
    Row(
        modifier.semantics(mergeDescendants = true) {},
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(text, style = MaterialTheme.typography.bodySmall, color = color, maxLines = 1)
        Spacer(Modifier.width(5.dp))
        Canvas(Modifier.size(18.dp, 10.dp).clearAndSetSemantics {}) {
            val radius = 1.6.dp.toPx()
            repeat(3) { index ->
                // Each dot lifts in turn, then rests for the remainder of the cycle.
                val local = ((phase - index * 0.18f) % 1f + 1f) % 1f
                val lift =
                    if (local < 0.4f) kotlin.math.sin(local / 0.4f * Math.PI).toFloat() else 0f
                drawCircle(
                    color.copy(alpha = 0.45f + 0.55f * lift),
                    radius,
                    Offset(
                        radius + index * (size.width - 2 * radius) / 2,
                        size.height - radius - lift * (size.height - 2 * radius),
                    ),
                )
            }
        }
    }
}

@file:OptIn(
    androidx.compose.foundation.layout.ExperimentalLayoutApi::class,
    androidx.compose.material3.ExperimentalMaterial3Api::class,
)

package build.cairn.app.ui

import androidx.compose.animation.core.FastOutSlowInEasing
import androidx.compose.animation.core.RepeatMode
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.tween
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.scale
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.PathEffect
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.role
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextDecoration
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp

private fun steps(count: Int) = if (count == 1) "1 étape" else "$count étapes"

@Composable
private fun pulse(): Float {
    val transition = rememberInfiniteTransition(label = "plan")
    val value by
        transition.animateFloat(
            1f,
            0.35f,
            infiniteRepeatable(tween(900, easing = FastOutSlowInEasing), RepeatMode.Reverse),
            label = "pulse",
        )
    return value
}

/** A step's mark: done, in progress, dropped or still to do. */
@Composable
private fun StepMark(state: PlanStepState) {
    val modifier = Modifier.size(18.dp)
    when (state) {
        PlanStepState.DONE ->
            Box(modifier.clip(CircleShape).background(signal.success), Alignment.Center) {
                Icon(
                    CairnIcons.Check,
                    null,
                    Modifier.size(11.dp),
                    tint = MaterialTheme.colorScheme.surface,
                )
            }
        PlanStepState.ACTIVE -> {
            val scale = pulse()
            Box(
                modifier.border(2.dp, MaterialTheme.colorScheme.primary, CircleShape),
                Alignment.Center,
            ) {
                Box(
                    Modifier.size(8.dp)
                        .scale(scale)
                        .clip(CircleShape)
                        .background(MaterialTheme.colorScheme.primary)
                )
            }
        }
        PlanStepState.CANCELLED ->
            Box(
                modifier.clip(CircleShape).background(MaterialTheme.colorScheme.surfaceVariant),
                Alignment.Center,
            ) {
                Icon(
                    CairnIcons.Close,
                    null,
                    Modifier.size(10.dp),
                    tint = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        PlanStepState.PENDING -> {
            val color = MaterialTheme.colorScheme.outline
            Canvas(modifier) {
                drawCircle(
                    color,
                    radius = size.minDimension / 2 - 1.dp.toPx(),
                    style =
                        Stroke(
                            1.5.dp.toPx(),
                            pathEffect = PathEffect.dashPathEffect(floatArrayOf(4f, 4f)),
                        ),
                )
            }
        }
    }
}

/** The steps of the agent's plan, with the one in progress and what the agent does during it. */
@Composable
internal fun PlanSteps(steps: List<PlanStep>) {
    Column {
        steps.forEach { step ->
            Row(
                Modifier.fillMaxWidth().padding(vertical = 6.dp).testTag("plan-step").semantics(
                    mergeDescendants = true
                ) {
                    stateDescription = step.state.label
                },
                verticalAlignment = Alignment.Top,
            ) {
                Box(Modifier.padding(top = 1.dp)) { StepMark(step.state) }
                Spacer(Modifier.width(10.dp))
                Column(Modifier.weight(1f)) {
                    Text(
                        step.text,
                        style = MaterialTheme.typography.bodyMedium,
                        fontWeight =
                            if (step.state == PlanStepState.ACTIVE) FontWeight.SemiBold else null,
                        color =
                            when (step.state) {
                                PlanStepState.DONE,
                                PlanStepState.CANCELLED ->
                                    MaterialTheme.colorScheme.onSurfaceVariant
                                else -> MaterialTheme.colorScheme.onSurface
                            },
                        textDecoration =
                            if (step.state == PlanStepState.CANCELLED) TextDecoration.LineThrough
                            else null,
                    )
                    if (
                        step.state == PlanStepState.ACTIVE &&
                            step.activeText.isNotBlank() &&
                            step.activeText != step.text
                    )
                        Text(
                            step.activeText,
                            style = MaterialTheme.typography.labelSmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                }
            }
        }
    }
}

/** One segment per step: done, in progress, dropped or still to do. */
@Composable
internal fun PlanSegments(steps: List<PlanStep>, modifier: Modifier = Modifier) {
    val alpha = if (steps.any { it.state == PlanStepState.ACTIVE }) pulse() else 1f
    Row(modifier.clearAndSetSemantics {}, horizontalArrangement = Arrangement.spacedBy(3.dp)) {
        steps.forEach { step ->
            Box(
                Modifier.weight(1f)
                    .height(5.dp)
                    .alpha(if (step.state == PlanStepState.ACTIVE) alpha else 1f)
                    .clip(CircleShape)
                    .background(
                        when (step.state) {
                            PlanStepState.DONE -> signal.success
                            PlanStepState.ACTIVE -> MaterialTheme.colorScheme.primary
                            PlanStepState.CANCELLED ->
                                MaterialTheme.colorScheme.surfaceVariant.copy(alpha = 0.5f)
                            PlanStepState.PENDING -> MaterialTheme.colorScheme.surfaceVariant
                        }
                    )
            )
        }
    }
}

@Composable
private fun PlanRing(progress: PlanProgress, size: Dp = 22.dp) {
    val done = signal.success
    val rest = MaterialTheme.colorScheme.surfaceVariant
    val sweep = if (progress.total == 0) 0f else 360f * progress.done / progress.total
    Canvas(Modifier.size(size).clearAndSetSemantics {}) {
        val stroke = 4.dp.toPx()
        val inset = stroke / 2
        val arc = Size(this.size.width - stroke, this.size.height - stroke)
        drawArc(rest, 0f, 360f, false, Offset(inset, inset), arc, style = Stroke(stroke))
        drawArc(done, -90f, sweep, false, Offset(inset, inset), arc, style = Stroke(stroke))
    }
}

@Composable
private fun Explanation(text: String) {
    if (text.isBlank()) return
    Text(
        text,
        Modifier.fillMaxWidth()
            .padding(vertical = 4.dp)
            .clip(RoundedCornerShape(10.dp))
            .background(MaterialTheme.colorScheme.surfaceContainerLow)
            .padding(horizontal = 10.dp, vertical = 8.dp),
        style = MaterialTheme.typography.labelMedium,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
    )
}

/**
 * The agent's plan where it first wrote it, kept up to date by its later updates. A finished plan
 * folds into its summary.
 */
@Composable
internal fun PlanCard(plan: AgentPlan, live: Boolean) {
    val finished = plan.steps.finished()
    val progress = plan.steps.progress()
    // A finished plan says how long the agent took to follow it.
    val took =
        if (plan.updatedAt - plan.time >= 1000) liveElapsed(plan.time, plan.updatedAt) else ""
    var toggled by rememberSaveable(plan.time) { mutableStateOf<Boolean?>(null) }
    val expanded = toggled ?: !finished
    Surface(
        Modifier.fillMaxWidth().testTag("agent-plan"),
        shape = RoundedCornerShape(18.dp),
        color = MaterialTheme.colorScheme.surface,
        border = BorderStroke(1.dp, MaterialTheme.colorScheme.outlineVariant),
    ) {
        Column {
            Surface(
                onClick = { toggled = !expanded },
                color = Color.Transparent,
                modifier =
                    Modifier.semantics {
                        role = Role.Button
                        stateDescription = if (expanded) "Plan affiché" else "Plan masqué"
                    },
            ) {
                Row(
                    Modifier.fillMaxWidth()
                        .heightIn(min = 52.dp)
                        .padding(horizontal = 12.dp, vertical = 10.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Box(
                        Modifier.size(30.dp)
                            .clip(RoundedCornerShape(10.dp))
                            .background(
                                if (finished) MaterialTheme.colorScheme.surfaceVariant
                                else MaterialTheme.colorScheme.primary.copy(alpha = 0.12f)
                            ),
                        contentAlignment = Alignment.Center,
                    ) {
                        Icon(
                            if (finished) CairnIcons.Check else CairnIcons.Tasks,
                            null,
                            Modifier.size(15.dp),
                            tint =
                                if (finished) signal.success else MaterialTheme.colorScheme.primary,
                        )
                    }
                    Spacer(Modifier.width(10.dp))
                    Column(Modifier.weight(1f)) {
                        Text(
                            if (finished) "Plan terminé" else "Plan",
                            style = MaterialTheme.typography.titleSmall,
                        )
                        Text(
                            if (finished)
                                listOf(steps(progress.total), took)
                                    .filter { it.isNotBlank() }
                                    .joinToString(" · ")
                            else "${steps(progress.total)} · ${progress.done} terminée(s)",
                            style = MaterialTheme.typography.labelSmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                    if (live)
                        Text(
                            "EN DIRECT",
                            Modifier.padding(start = 8.dp)
                                .clip(CircleShape)
                                .background(MaterialTheme.colorScheme.primary.copy(alpha = 0.12f))
                                .padding(horizontal = 8.dp, vertical = 2.dp),
                            style = MaterialTheme.typography.labelSmall,
                            fontWeight = FontWeight.Bold,
                            color = MaterialTheme.colorScheme.primary,
                        )
                    Icon(
                        CairnIcons.Down,
                        null,
                        Modifier.padding(start = 8.dp)
                            .size(18.dp)
                            .scale(1f, if (expanded) -1f else 1f),
                        tint = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
            if (expanded)
                Column(Modifier.padding(start = 14.dp, end = 14.dp, bottom = 8.dp)) {
                    if (!finished) PlanSegments(plan.steps, Modifier.padding(bottom = 4.dp))
                    Explanation(plan.explanation)
                    PlanSteps(plan.steps)
                }
        }
    }
}

/** Where the agent changed its plan: what it finished, started, added or dropped. */
@Composable
internal fun PlanUpdateRow(update: PlanUpdate) {
    val change = update.change
    Surface(
        Modifier.fillMaxWidth().testTag("plan-update").clearAndSetSemantics {
            contentDescription =
                "${change.label()}, ${update.progress.done} sur ${update.progress.total} terminée(s)"
        },
        shape = RoundedCornerShape(14.dp),
        color = MaterialTheme.colorScheme.surfaceContainerLow,
        border = BorderStroke(1.dp, MaterialTheme.colorScheme.outlineVariant),
    ) {
        Row(
            Modifier.padding(horizontal = 12.dp, vertical = 7.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            FlowRow(
                Modifier.weight(1f),
                horizontalArrangement = Arrangement.spacedBy(10.dp),
                verticalArrangement = Arrangement.spacedBy(2.dp),
                itemVerticalAlignment = Alignment.CenterVertically,
            ) {
                Text(
                    "PLAN",
                    style = MaterialTheme.typography.labelSmall,
                    fontWeight = FontWeight.Bold,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                val muted = MaterialTheme.colorScheme.onSurfaceVariant
                change.done.forEach { ChangeChip(CairnIcons.Check, it, signal.success) }
                change.started.forEach {
                    ChangeChip(CairnIcons.Right, it, MaterialTheme.colorScheme.primary, bold = true)
                }
                change.added.forEach { ChangeChip(CairnIcons.Plus, it, muted) }
                change.cancelled.forEach {
                    ChangeChip(CairnIcons.Close, it, muted, struck = true)
                }
            }
            Text(
                "${update.progress.done}/${update.progress.total}",
                Modifier.padding(start = 8.dp),
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
}

@Composable
private fun ChangeChip(
    icon: androidx.compose.ui.graphics.vector.ImageVector,
    text: String,
    color: Color,
    bold: Boolean = false,
    struck: Boolean = false,
) {
    Row(verticalAlignment = Alignment.CenterVertically) {
        Icon(icon, null, Modifier.size(12.dp), tint = color)
        Spacer(Modifier.width(4.dp))
        Text(
            text,
            style = MaterialTheme.typography.labelMedium,
            fontWeight = if (bold) FontWeight.SemiBold else null,
            color = color,
            textDecoration = if (struck) TextDecoration.LineThrough else null,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
        )
    }
}

/** What the bar says the agent does: the step in progress, or the next one. */
internal fun AgentPlan.barLabel(): String {
    val current = steps.currentStep() ?: return ""
    return if (current.active) current.step.activeText.ifBlank { current.step.text }
    else "Suivant : ${current.step.text}"
}

/**
 * « Cap »: the plan of the current request above the composer, so where the agent stands stays in
 * sight while the conversation scrolls. It opens the whole plan in a sheet.
 */
@Composable
internal fun PlanBar(plan: AgentPlan) {
    var open by rememberSaveable { mutableStateOf(false) }
    val progress = plan.steps.progress()
    val label = plan.barLabel()
    BoxWithConstraints(Modifier.fillMaxWidth().padding(horizontal = 12.dp).padding(top = 4.dp)) {
        val wide = maxWidth >= 600.dp
        Surface(
            onClick = { open = true },
            shape = RoundedCornerShape(16.dp),
            color = MaterialTheme.colorScheme.surface,
            border = BorderStroke(1.dp, MaterialTheme.colorScheme.outlineVariant),
            modifier =
                Modifier.fillMaxWidth().testTag("plan-bar").clearAndSetSemantics {
                    role = Role.Button
                    contentDescription =
                        "Plan, ${progress.done} sur ${progress.total} terminée(s)" +
                            (if (label.isBlank()) "" else ". $label") +
                            ". Afficher le plan"
                },
        ) {
            Row(
                Modifier.heightIn(min = 48.dp).padding(horizontal = 10.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                PlanRing(progress)
                Spacer(Modifier.width(10.dp))
                if (wide) {
                    Text(
                        "Plan",
                        style = MaterialTheme.typography.labelMedium,
                        fontWeight = FontWeight.SemiBold,
                    )
                    Spacer(Modifier.width(10.dp))
                    PlanSegments(plan.steps, Modifier.width(96.dp))
                    Spacer(Modifier.width(10.dp))
                }
                Text(
                    label,
                    Modifier.weight(1f),
                    style = MaterialTheme.typography.labelMedium,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
                Text(
                    "${progress.done}/${progress.total}",
                    Modifier.padding(horizontal = 8.dp),
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                Icon(
                    CairnIcons.Up,
                    null,
                    Modifier.size(16.dp),
                    tint = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
    }
    if (open) PlanSheet(plan) { open = false }
}

/** The whole plan of the current request, its explanation and each step's state. */
@Composable
internal fun PlanSheet(plan: AgentPlan, close: () -> Unit) {
    val progress = plan.steps.progress()
    ModalBottomSheet(
        onDismissRequest = close,
        sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true),
        containerColor = MaterialTheme.colorScheme.background,
    ) {
        Column(
            Modifier.fillMaxWidth()
                .verticalScroll(rememberScrollState())
                .padding(start = 20.dp, end = 20.dp, bottom = 24.dp)
                .testTag("plan-sheet")
        ) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Icon(
                    CairnIcons.Tasks,
                    null,
                    Modifier.size(18.dp),
                    tint = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                Spacer(Modifier.width(8.dp))
                Text("Plan", Modifier.weight(1f), style = MaterialTheme.typography.titleMedium)
                Text(
                    "${progress.done} sur ${progress.total}",
                    style = MaterialTheme.typography.labelMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            PlanSegments(plan.steps, Modifier.fillMaxWidth().padding(vertical = 10.dp))
            Explanation(plan.explanation)
            PlanSteps(plan.steps)
        }
    }
}

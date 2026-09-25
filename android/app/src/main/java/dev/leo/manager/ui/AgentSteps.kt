@file:OptIn(androidx.compose.material3.ExperimentalMaterial3Api::class)

package dev.leo.manager.ui

import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.HorizontalDivider
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
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.scale
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.role
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive

/** One line of the agent's timeline; consecutive reads fold into a single step. */
internal data class AgentStep(
    val items: List<ActivityPresentation>,
    val title: String,
    val detail: String,
) {
    val lead
        get() = items.first()

    val failed
        get() = items.any { it.failed }

    val running
        get() = items.any { it.isRunning() }
}

internal fun ActivityPresentation.isRunning() = state == "En cours" && !failed

private fun ActivityPresentation.changedFiles(): List<String> =
    (item?.get("changes") as? JsonArray).orEmpty().mapNotNull {
        ((it as? JsonObject)?.get("path") as? JsonPrimitive)?.content?.substringAfterLast('/')
    }

internal fun ActivityKind.glyph(): ImageVector =
    when (this) {
        ActivityKind.COMMAND,
        ActivityKind.OUTPUT -> LeoIcons.Terminal
        ActivityKind.READ -> LeoIcons.File
        ActivityKind.FILES -> LeoIcons.Pencil
        ActivityKind.BROWSE -> LeoIcons.Folder
        ActivityKind.SEARCH -> LeoIcons.Search
        ActivityKind.PLAN -> LeoIcons.Check
        ActivityKind.THINKING -> LeoIcons.Spark
        ActivityKind.TOOL -> LeoIcons.Plug
        ActivityKind.NOTICE -> LeoIcons.Clock
    }

/** "A lu 2 fichiers, cherché 1 fois, modifié 2 fichiers, lancé 1 commande". */
internal fun actionSentence(items: List<ActivityPresentation>): String {
    fun count(value: Int, one: String, many: String) =
        if (value == 1) one else many.replace("#", value.toString())
    val parts = mutableListOf<String>()
    val reads =
        items.filter { it.kind == ActivityKind.READ }.sumOf { it.paths.size.coerceAtLeast(1) }
    if (reads > 0) parts += count(reads, "lu 1 fichier", "lu # fichiers")
    val searches = items.count { it.kind == ActivityKind.SEARCH || it.kind == ActivityKind.BROWSE }
    if (searches > 0) parts += count(searches, "cherché 1 fois", "cherché # fois")
    val edits =
        items
            .filter { it.kind == ActivityKind.FILES }
            .sumOf { it.changedFiles().size.coerceAtLeast(1) }
    if (edits > 0) parts += count(edits, "modifié 1 fichier", "modifié # fichiers")
    val commands = items.count { it.kind == ActivityKind.COMMAND || it.kind == ActivityKind.OUTPUT }
    if (commands > 0) parts += count(commands, "lancé 1 commande", "lancé # commandes")
    val tools = items.count { it.kind == ActivityKind.TOOL }
    if (tools > 0) parts += count(tools, "utilisé 1 outil", "utilisé # outils")
    if (items.any { it.kind == ActivityKind.PLAN }) parts += "mis à jour le plan"
    return when {
        parts.isNotEmpty() -> "A " + parts.joinToString(", ")
        items.any { it.kind == ActivityKind.THINKING } -> "A réfléchi"
        else -> "Suivi de l’exécution"
    }
}

internal fun agentSteps(items: List<ActivityPresentation>): List<AgentStep> {
    val steps = mutableListOf<AgentStep>()
    var index = 0
    while (index < items.size) {
        val p = items[index]
        if (p.kind == ActivityKind.READ && !p.failed && !p.isRunning()) {
            var end = index
            while (
                end + 1 < items.size &&
                    items[end + 1].let {
                        it.kind == ActivityKind.READ && !it.failed && !it.isRunning()
                    }
            ) end++
            val group = items.subList(index, end + 1)
            val files = group.flatMap { it.paths }.map { it.substringAfterLast('/') }.distinct()
            steps +=
                AgentStep(
                    group.toList(),
                    if (group.size > 1) "Lire ${files.size} fichiers" else p.title,
                    files.joinToString(" · "),
                )
            index = end + 1
            continue
        }
        val detail =
            when (p.kind) {
                ActivityKind.FILES -> p.changedFiles().joinToString(" · ").ifBlank { p.subtitle }
                ActivityKind.THINKING ->
                    p.output.lineSequence().firstOrNull { it.isNotBlank() }.orEmpty()
                ActivityKind.NOTICE -> p.subtitle
                else -> p.command.ifBlank { p.subtitle }
            }
        steps += AgentStep(listOf(p), p.title, detail)
        index++
    }
    return steps
}

/**
 * The agent's actions between two messages: a one-sentence summary that expands into a timeline.
 * While the agent works, the step in progress is left to the working indicator below.
 */
@Composable
internal fun AgentActions(
    key: String,
    presentations: List<ActivityPresentation>,
    hideRunning: Boolean,
) {
    val shown = if (hideRunning) presentations.filter { !it.isRunning() } else presentations
    if (shown.isEmpty()) return
    var expanded by rememberSaveable(key) { mutableStateOf(false) }
    var opened by rememberSaveable(key) { mutableStateOf<Int?>(null) }
    val steps = remember(shown) { agentSteps(shown) }
    val sentence = remember(shown) { actionSentence(shown.filter { !it.isRunning() }) }
    val failures = steps.count { it.failed }
    val shape = RoundedCornerShape(18.dp)
    Surface(
        shape = shape,
        color =
            if (expanded) MaterialTheme.colorScheme.surface
            else MaterialTheme.colorScheme.surfaceVariant,
        border =
            if (expanded) BorderStroke(1.dp, MaterialTheme.colorScheme.outlineVariant) else null,
        modifier = Modifier.fillMaxWidth(),
    ) {
        Column {
            Surface(
                onClick = { expanded = !expanded },
                color = Color.Transparent,
                modifier =
                    Modifier.testTag("agent-actions").semantics {
                        role = Role.Button
                        stateDescription = if (expanded) "Étapes affichées" else "Étapes masquées"
                    },
            ) {
                Row(
                    Modifier.fillMaxWidth()
                        .heightIn(min = 48.dp)
                        .padding(horizontal = 12.dp, vertical = 10.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    if (!expanded) {
                        Row(
                            Modifier.clearAndSetSemantics {},
                            horizontalArrangement = Arrangement.spacedBy((-8).dp),
                        ) {
                            shown
                                .map { it.kind }
                                .distinct()
                                .filter { it != ActivityKind.NOTICE }
                                .take(4)
                                .forEach { kind ->
                                    Box(
                                        Modifier.size(26.dp)
                                            .clip(CircleShape)
                                            .background(MaterialTheme.colorScheme.surface)
                                            .border(
                                                2.dp,
                                                MaterialTheme.colorScheme.surfaceVariant,
                                                CircleShape,
                                            ),
                                        contentAlignment = Alignment.Center,
                                    ) {
                                        Icon(
                                            kind.glyph(),
                                            null,
                                            Modifier.size(13.dp),
                                            tint = MaterialTheme.colorScheme.onSurfaceVariant,
                                        )
                                    }
                                }
                        }
                        Spacer(Modifier.width(10.dp))
                    }
                    Column(Modifier.weight(1f)) {
                        Text(
                            sentence,
                            style =
                                if (expanded) MaterialTheme.typography.labelMedium
                                else MaterialTheme.typography.bodyMedium,
                            color =
                                if (expanded) MaterialTheme.colorScheme.onSurfaceVariant
                                else MaterialTheme.colorScheme.onSurface,
                            maxLines = if (expanded) 1 else 2,
                            overflow = TextOverflow.Ellipsis,
                        )
                        if (!expanded)
                            Row(verticalAlignment = Alignment.CenterVertically) {
                                Text(
                                    if (steps.size == 1) "1 étape" else "${steps.size} étapes",
                                    style = MaterialTheme.typography.labelSmall,
                                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                                )
                                if (failures > 0) {
                                    Text(
                                        "  ·  ",
                                        style = MaterialTheme.typography.labelSmall,
                                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                                    )
                                    Box(
                                        Modifier.size(6.dp)
                                            .clip(CircleShape)
                                            .background(signal.attention)
                                    )
                                    Text(
                                        if (failures == 1) " 1 échec" else " $failures échecs",
                                        style = MaterialTheme.typography.labelSmall,
                                        color = signal.attention,
                                    )
                                }
                            }
                    }
                    Icon(
                        LeoIcons.Down,
                        null,
                        Modifier.size(18.dp).scale(1f, if (expanded) -1f else 1f),
                        tint = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
            if (expanded)
                Column(Modifier.padding(start = 14.dp, end = 10.dp, bottom = 12.dp)) {
                    steps.forEachIndexed { index, step ->
                        StepRow(step, last = index == steps.lastIndex) { opened = index }
                    }
                }
        }
    }
    opened?.let { index ->
        val step = steps.getOrNull(index)
        if (step == null) LaunchedEffect(index) { opened = null }
        else StepSheet(step, index, steps.size) { opened = null }
    }
}

@Composable
private fun StepRow(step: AgentStep, last: Boolean, open: () -> Unit) {
    val p = step.lead
    val tint =
        when {
            step.failed -> signal.attention
            step.running -> MaterialTheme.colorScheme.primary
            else -> MaterialTheme.colorScheme.onSurfaceVariant
        }
    val status =
        when {
            step.failed ->
                step.items
                    .firstNotNullOfOrNull { it.exitCode }
                    ?.takeIf { it != 0 }
                    ?.let { "Code $it" } ?: "Échec"
            step.running -> "en cours"
            else -> ""
        }
    Surface(
        onClick = open,
        color = Color.Transparent,
        shape = RoundedCornerShape(12.dp),
        modifier =
            Modifier.fillMaxWidth().testTag("agent-step").semantics {
                contentDescription =
                    listOf(step.title, status, step.detail)
                        .filter { it.isNotBlank() }
                        .joinToString(", ")
            },
    ) {
        Row(Modifier.fillMaxWidth().height(IntrinsicSize.Min)) {
            Column(horizontalAlignment = Alignment.CenterHorizontally) {
                Box(
                    Modifier.size(26.dp)
                        .clip(CircleShape)
                        .background(
                            when {
                                step.failed -> signal.attentionSoft
                                step.running -> MaterialTheme.colorScheme.primary
                                else -> MaterialTheme.colorScheme.surfaceVariant
                            }
                        ),
                    contentAlignment = Alignment.Center,
                ) {
                    Icon(
                        p.kind.glyph(),
                        null,
                        Modifier.size(13.dp),
                        tint = if (step.running) MaterialTheme.colorScheme.onPrimary else tint,
                    )
                }
                if (!last)
                    Box(
                        Modifier.width(2.dp)
                            .weight(1f)
                            .background(MaterialTheme.colorScheme.outlineVariant)
                    )
            }
            Spacer(Modifier.width(12.dp))
            Column(Modifier.weight(1f).padding(top = 3.dp, bottom = if (last) 2.dp else 12.dp)) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text(
                        step.title,
                        Modifier.weight(1f),
                        style = MaterialTheme.typography.titleSmall,
                        color = MaterialTheme.colorScheme.onSurface,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                    if (status.isNotBlank())
                        Text(
                            status,
                            style = MaterialTheme.typography.labelSmall,
                            color = tint,
                            fontWeight = FontWeight.Bold,
                        )
                    else
                        Icon(
                            LeoIcons.Right,
                            null,
                            Modifier.size(14.dp),
                            tint = MaterialTheme.colorScheme.outline.copy(alpha = 0.6f),
                        )
                }
                if (step.detail.isNotBlank())
                    Text(
                        step.detail,
                        style = MaterialTheme.typography.labelSmall,
                        fontFamily =
                            if (p.kind == ActivityKind.THINKING || p.kind == ActivityKind.NOTICE)
                                null
                            else FontFamily.Monospace,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
            }
        }
    }
}

/** Full detail of one step: command, output, diff or structured result, and the raw source. */
@Composable
private fun StepSheet(step: AgentStep, index: Int, total: Int, close: () -> Unit) {
    ModalBottomSheet(
        onDismissRequest = close,
        sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true),
        containerColor = MaterialTheme.colorScheme.background,
    ) {
        Column(
            Modifier.fillMaxWidth()
                .testTag("agent-step-sheet")
                .verticalScroll(rememberScrollState())
                .padding(horizontal = 20.dp)
                .padding(bottom = 24.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            val tint =
                when {
                    step.failed -> signal.attention
                    step.running -> MaterialTheme.colorScheme.primary
                    else -> MaterialTheme.colorScheme.onSurfaceVariant
                }
            Row(verticalAlignment = Alignment.CenterVertically) {
                Box(
                    Modifier.size(40.dp)
                        .clip(RoundedCornerShape(12.dp))
                        .background(
                            if (step.failed) signal.attentionSoft
                            else MaterialTheme.colorScheme.surfaceVariant
                        ),
                    contentAlignment = Alignment.Center,
                ) {
                    Icon(step.lead.kind.glyph(), null, Modifier.size(20.dp), tint = tint)
                }
                Spacer(Modifier.width(12.dp))
                Column(Modifier.weight(1f)) {
                    Text(step.title, style = MaterialTheme.typography.titleMedium)
                    Text(
                        listOfNotNull(
                                if (step.failed) "Échec" else step.lead.state,
                                step.items
                                    .firstNotNullOfOrNull { it.exitCode }
                                    ?.takeIf { step.failed }
                                    ?.let { "code $it" },
                                "étape ${index + 1} sur $total",
                            )
                            .joinToString(" · "),
                        style = MaterialTheme.typography.labelMedium,
                        color = tint,
                    )
                }
                ActionIcon("Fermer", LeoIcons.Close, onClick = close)
            }
            step.items.forEachIndexed { position, p ->
                if (step.items.size > 1) {
                    if (position > 0)
                        HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
                    Text(p.title, style = MaterialTheme.typography.titleSmall)
                }
                if (p.subtitle.isNotBlank() && p.kind == ActivityKind.NOTICE)
                    Text(
                        p.subtitle,
                        style = MaterialTheme.typography.bodyMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                ActivityBody(p)
                if (p.output.isNotBlank()) CopyButton("Copier la sortie", p.output)
                AdvancedSource(p.raw)
            }
        }
    }
}

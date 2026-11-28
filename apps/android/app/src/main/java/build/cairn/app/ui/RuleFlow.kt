@file:OptIn(androidx.compose.foundation.layout.ExperimentalLayoutApi::class)

package build.cairn.app.ui

import androidx.compose.animation.core.Animatable
import androidx.compose.animation.core.tween
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.filled.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.composed
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.TransformOrigin
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.layout.Layout
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.Constraints
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import build.cairn.app.data.*
import build.cairn.contracts.HookRuleCatalog
import build.cairn.contracts.HookRuleProblem
import kotlinx.coroutines.flow.first
import kotlinx.serialization.json.*

@Composable
internal fun RuleFlowTree(
    nodes: List<JsonElement>,
    container: String,
    catalog: HookRuleCatalog?,
    agentName: (String) -> String,
    selected: String? = null,
    editable: Boolean = false,
    select: (String) -> Unit = {},
    insert: (String, Int, String) -> Unit = { _, _, _ -> },
    remove: (String) -> Unit = {},
    move: (String, Int) -> Unit = { _, _ -> },
    depth: Int = 0,
    problems: List<HookRuleProblem> = emptyList(),
    executionAgentId: String? = null,
    /** The step to draw attention to: it shakes once each time `attentionCount` grows. */
    attention: String? = null,
    attentionCount: Int = 0,
    /** Replaces the « Événement reçu » terminal with the rule's own starting node. */
    start: (@Composable () -> Unit)? = null,
    /** The graph fills the space it is given and scrolls both ways, as a canvas. */
    fill: Boolean = false,
    modifier: Modifier = Modifier,
) {
    val graphWidth = ruleFlowWidth(nodes)
    val root = container == "/steps"
    val content: @Composable () -> Unit = {
        Column(Modifier.width(graphWidth.dp), horizontalAlignment = Alignment.CenterHorizontally) {
            if (root) start?.invoke() ?: RuleGraphTerminal("Événement reçu")
            if (editable) RuleInsertPoint(container, 0, depth, insert, catalog, executionAgentId)
            else if (root) RuleGraphLink()
            nodes.forEachIndexed { index, raw ->
                val node = raw.jsonObject
                val path = "$container/$index"
                val kind =
                    if (node.text("type") == "action") node.obj("action").text("type")
                    else node.text("type")
                val nodeProblems = problemsUnder(problems, path)
                val tone =
                    when (kind) {
                        "if" -> MaterialTheme.colorScheme.tertiary
                        "forEach" -> MaterialTheme.colorScheme.secondary
                        "cancelConversation" -> MaterialTheme.colorScheme.error
                        else -> MaterialTheme.colorScheme.primary
                    }
                val icon =
                    when (kind) {
                        "if" -> CairnIcons.Branch
                        "forEach" -> CairnIcons.Retry
                        "wait" -> CairnIcons.Hourglass
                        "storeValue" -> CairnIcons.Book
                        "callTool" -> CairnIcons.Plug
                        "answerQuestion" -> CairnIcons.SideQuestion
                        "cancelConversation" -> CairnIcons.Stop
                        else -> CairnIcons.Orbit
                    }
                val summary =
                    when (node.text("type")) {
                        "if" ->
                            "Si / sinon" to
                                describeCondition(node.obj("condition")).ifBlank {
                                    "Choisir les conditions"
                                }
                        "forEach" ->
                            "Pour chaque ${node.text("itemName")}" to
                                describeValue(node.obj("items"))
                        else -> describeAction(node.obj("action"), catalog, agentName)
                    }
                Surface(
                    shape = RoundedCornerShape(20.dp),
                    color = MaterialTheme.colorScheme.surfaceContainerLow,
                    border =
                        BorderStroke(
                            if (selected == path) 2.dp else 1.dp,
                            if (nodeProblems.isNotEmpty()) MaterialTheme.colorScheme.error
                            else if (selected == path) tone
                            else MaterialTheme.colorScheme.outlineVariant,
                        ),
                    modifier =
                        Modifier.width(240.dp).ruleAttention(attention == path, attentionCount),
                ) {
                    Column {
                        Row(
                            Modifier.fillMaxWidth()
                                .then(
                                    if (editable) Modifier.clickable { select(path) } else Modifier
                                )
                                .padding(14.dp),
                            verticalAlignment = Alignment.Top,
                        ) {
                            Surface(
                                shape = RoundedCornerShape(12.dp),
                                color = tone.copy(alpha = 0.12f),
                            ) {
                                Icon(icon, null, Modifier.padding(10.dp).size(19.dp), tint = tone)
                            }
                            Spacer(Modifier.width(10.dp))
                            Column(
                                Modifier.weight(1f),
                                verticalArrangement = Arrangement.spacedBy(3.dp),
                            ) {
                                Text(
                                    node.text("label").ifBlank { summary.first },
                                    style = MaterialTheme.typography.titleSmall,
                                    fontSize = 15.sp,
                                    maxLines = 2,
                                    fontWeight = FontWeight.SemiBold,
                                )
                                Text(
                                    summary.second,
                                    style = MaterialTheme.typography.bodySmall,
                                    maxLines = 2,
                                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                                )
                            }
                            if (editable) {
                                var actions by remember(node.text("id")) { mutableStateOf(false) }
                                Box {
                                    IconButton(
                                        onClick = { actions = true },
                                        modifier =
                                            Modifier.size(28.dp).semantics {
                                                contentDescription = "Actions de l’étape"
                                            },
                                    ) {
                                        Text("⋯", fontSize = 22.sp)
                                    }
                                    DropdownMenu(actions, { actions = false }) {
                                        DropdownMenuItem(
                                            text = { Text("Monter") },
                                            enabled = index > 0,
                                            onClick = {
                                                actions = false
                                                move(path, -1)
                                            },
                                        )
                                        DropdownMenuItem(
                                            text = { Text("Descendre") },
                                            enabled = index < nodes.lastIndex,
                                            onClick = {
                                                actions = false
                                                move(path, 1)
                                            },
                                        )
                                        DropdownMenuItem(
                                            text = { Text("Supprimer l’étape") },
                                            onClick = {
                                                actions = false
                                                remove(path)
                                            },
                                        )
                                    }
                                }
                            }
                        }
                        if (nodeProblems.isNotEmpty())
                            Text(
                                "${nodeProblems.size} points à corriger · ${nodeProblems.first().message}",
                                Modifier.padding(horizontal = 14.dp, vertical = 6.dp).clickable {
                                    select(path)
                                },
                                color = MaterialTheme.colorScheme.error,
                                style = MaterialTheme.typography.bodySmall,
                                maxLines = 2,
                            )
                    }
                }

                when (node.text("type")) {
                    "if" -> {
                        val left = ruleFlowWidth(node.list("then"))
                        val right = ruleFlowWidth(node.list("otherwise"))
                        Column(Modifier.width((left + 64 + right).dp)) {
                            RuleGraphLink("split", left, right)
                            Row(Modifier.height(IntrinsicSize.Min)) {
                                for ((branch, width) in
                                    listOf("then" to left, "otherwise" to right)) {
                                    Column(
                                        Modifier.width(width.dp).fillMaxHeight(),
                                        horizontalAlignment = Alignment.CenterHorizontally,
                                    ) {
                                        RuleFlowTree(
                                            node.list(branch),
                                            "$path/$branch",
                                            catalog,
                                            agentName,
                                            selected,
                                            editable,
                                            select,
                                            insert,
                                            remove,
                                            move,
                                            depth + 1,
                                            problems,
                                            executionAgentId,
                                            attention,
                                            attentionCount,
                                        )
                                        Box(
                                            Modifier.weight(1f)
                                                .width(1.dp)
                                                .background(MaterialTheme.colorScheme.outline)
                                        )
                                    }
                                    if (branch == "then") Spacer(Modifier.width(64.dp))
                                }
                            }
                            RuleGraphLink("merge", left, right)
                        }
                    }
                    "forEach" -> {
                        val loopWidth = ruleFlowWidth(node.list("body")) + 96
                        val line = MaterialTheme.colorScheme.outline
                        Box(
                            Modifier.width(loopWidth.dp).drawBehind {
                                val cx = size.width / 2
                                val top = -48.dp.toPx()
                                val right = size.width - 8.dp.toPx()
                                val bottom = size.height - 64.dp.toPx()
                                drawPath(
                                    Path().apply {
                                        moveTo(cx, bottom)
                                        lineTo(right, bottom)
                                        lineTo(right, top)
                                        lineTo(cx + 120.dp.toPx(), top)
                                    },
                                    line,
                                    style = Stroke(1.5.dp.toPx()),
                                )
                                drawLine(
                                    line,
                                    Offset(cx + 120.dp.toPx(), top),
                                    Offset(cx + 126.dp.toPx(), top - 5.dp.toPx()),
                                    1.5.dp.toPx(),
                                )
                                drawLine(
                                    line,
                                    Offset(cx + 120.dp.toPx(), top),
                                    Offset(cx + 126.dp.toPx(), top + 5.dp.toPx()),
                                    1.5.dp.toPx(),
                                )
                                drawPath(
                                    Path().apply {
                                        moveTo(cx - 120.dp.toPx(), top)
                                        lineTo(8.dp.toPx(), top)
                                        lineTo(8.dp.toPx(), size.height)
                                        lineTo(cx, size.height)
                                    },
                                    line,
                                    style = Stroke(1.5.dp.toPx()),
                                )
                            }
                        ) {
                            Column(
                                Modifier.align(Alignment.TopCenter).padding(bottom = 64.dp),
                                horizontalAlignment = Alignment.CenterHorizontally,
                            ) {
                                RuleGraphLink()
                                RuleFlowTree(
                                    node.list("body"),
                                    "$path/body",
                                    catalog,
                                    agentName,
                                    selected,
                                    editable,
                                    select,
                                    insert,
                                    remove,
                                    move,
                                    depth + 1,
                                    problems,
                                    executionAgentId,
                                    attention,
                                    attentionCount,
                                )
                            }
                            Text(
                                "Élément suivant",
                                Modifier.align(Alignment.CenterEnd)
                                    .background(MaterialTheme.colorScheme.surface)
                                    .padding(3.dp),
                                style = MaterialTheme.typography.labelSmall,
                            )
                            Text(
                                "Terminé",
                                Modifier.align(Alignment.CenterStart)
                                    .background(MaterialTheme.colorScheme.surface)
                                    .padding(3.dp),
                                style = MaterialTheme.typography.labelSmall,
                            )
                        }
                    }
                }
                if (editable)
                    RuleInsertPoint(container, index + 1, depth, insert, catalog, executionAgentId)
                else if (index < nodes.lastIndex) RuleGraphLink()
            }
            if (!editable && nodes.isEmpty()) RuleGraphTerminal("Aucune action")
            if (root) {
                RuleGraphLink()
                RuleGraphTerminal("Fin")
            }
        }
    }
    if (root) RuleGraphViewport(graphWidth, editable, content, fill, modifier) else content()
}

/** Shakes once when `active` and each time `count` grows, to show where something is missing. */
internal fun Modifier.ruleAttention(active: Boolean, count: Int): Modifier = composed {
    val shift = remember { Animatable(0f) }
    LaunchedEffect(active, count) {
        if (!active) return@LaunchedEffect
        for (target in listOf(-6f, 6f, -6f, 6f, 0f)) shift.animateTo(target, tween(55))
    }
    offset { IntOffset(shift.value.dp.roundToPx(), 0) }
}

internal fun ruleFlowWidth(nodes: List<JsonElement>): Float =
    nodes
        .maxOfOrNull { raw ->
            val node = raw.jsonObject
            when (node.text("type")) {
                "if" ->
                    ruleFlowWidth(node.list("then")) + 64 + ruleFlowWidth(node.list("otherwise"))
                "forEach" -> ruleFlowWidth(node.list("body")) + 96
                else -> 240f
            }
        }
        ?.coerceAtLeast(240f) ?: 240f

@Composable
private fun RuleGraphTerminal(text: String) {
    Surface(
        shape = CircleShape,
        border = BorderStroke(1.dp, MaterialTheme.colorScheme.outlineVariant),
    ) {
        Text(
            text,
            Modifier.padding(horizontal = 16.dp, vertical = 7.dp),
            style = MaterialTheme.typography.labelMedium,
        )
    }
}

@Composable
private fun RuleGraphLink(kind: String = "straight", left: Float = 240f, right: Float = 240f) {
    val color = MaterialTheme.colorScheme.outline
    val height = if (kind == "straight") 32.dp else 64.dp
    Box(Modifier.fillMaxWidth().height(height)) {
        Canvas(Modifier.matchParentSize()) {
            val cx = size.width / 2
            val lx = size.width * (left / 2) / (left + 64 + right)
            val rx = size.width * (left + 64 + right / 2) / (left + 64 + right)
            val middle = 24.dp.toPx()
            val stroke = 1.5.dp.toPx()
            fun arrow(x: Float) {
                drawLine(
                    color,
                    Offset(x, size.height),
                    Offset(x - 4.dp.toPx(), size.height - 6.dp.toPx()),
                    stroke,
                )
                drawLine(
                    color,
                    Offset(x, size.height),
                    Offset(x + 4.dp.toPx(), size.height - 6.dp.toPx()),
                    stroke,
                )
            }
            if (kind == "split") {
                drawPath(
                    Path().apply {
                        moveTo(cx, 0f)
                        lineTo(cx, middle)
                        moveTo(lx, size.height)
                        lineTo(lx, middle)
                        lineTo(rx, middle)
                        lineTo(rx, size.height)
                    },
                    color,
                    style = Stroke(stroke),
                )
                arrow(lx)
                arrow(rx)
            } else if (kind == "merge") {
                drawPath(
                    Path().apply {
                        moveTo(lx, 0f)
                        lineTo(lx, middle)
                        lineTo(rx, middle)
                        lineTo(rx, 0f)
                        moveTo(cx, middle)
                        lineTo(cx, size.height)
                    },
                    color,
                    style = Stroke(stroke),
                )
                arrow(cx)
            } else {
                drawLine(color, Offset(cx, 0f), Offset(cx, size.height), stroke)
                arrow(cx)
            }
        }
        if (kind == "split")
            Row(Modifier.fillMaxWidth().padding(top = 27.dp)) {
                Box(Modifier.weight(left)) {
                    Text(
                        "ALORS",
                        Modifier.align(Alignment.Center)
                            .background(MaterialTheme.colorScheme.surface)
                            .padding(horizontal = 5.dp),
                        style = MaterialTheme.typography.labelSmall,
                        color = MaterialTheme.colorScheme.primary,
                    )
                }
                Spacer(Modifier.width(64.dp))
                Box(Modifier.weight(right)) {
                    Text(
                        "SINON",
                        Modifier.align(Alignment.Center)
                            .background(MaterialTheme.colorScheme.surface)
                            .padding(horizontal = 5.dp),
                        style = MaterialTheme.typography.labelSmall,
                    )
                }
            }
    }
}

@Composable
private fun RuleGraphViewport(
    width: Float,
    editable: Boolean,
    content: @Composable () -> Unit,
    fill: Boolean = false,
    modifier: Modifier = Modifier,
) {
    var manualZoom by remember { mutableStateOf<Float?>(null) }
    var availableWidth by remember { mutableFloatStateOf(360f) }
    val zoom = manualZoom ?: ((availableWidth - 24) / width).coerceIn(0.25f, 1f)
    val density = LocalDensity.current
    val horizontal = rememberScrollState()
    val vertical = rememberScrollState()
    Column(
        modifier
            .fillMaxWidth()
            .onSizeChanged { availableWidth = it.width / density.density }
            .then(
                if (fill) Modifier
                else
                    Modifier.border(
                        1.dp,
                        MaterialTheme.colorScheme.outlineVariant,
                        RoundedCornerShape(16.dp),
                    )
            )
    ) {
        Row(
            Modifier.fillMaxWidth().padding(horizontal = 8.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text("Graphe", Modifier.weight(1f), style = MaterialTheme.typography.labelLarge)
            TextButton(onClick = { manualZoom = (zoom - 0.15f).coerceAtLeast(0.25f) }) { Text("−") }
            Text("${(zoom * 100).toInt()} %", style = MaterialTheme.typography.labelSmall)
            TextButton(onClick = { manualZoom = (zoom + 0.15f).coerceAtMost(1.5f) }) { Text("+") }
            TextButton(onClick = { manualZoom = null }) {
                Text("Tout voir")
            }
        }
        val dots = MaterialTheme.colorScheme.outlineVariant
        BoxWithConstraints(
            Modifier.fillMaxWidth()
                .then(if (fill) Modifier.weight(1f) else Modifier.heightIn(min = 240.dp))
                .background(MaterialTheme.colorScheme.surfaceContainerLow)
        ) {
            val available = maxWidth.value
            LaunchedEffect(width, available, zoom) {
                if (width * zoom > available - 24 + 1f) {
                    androidx.compose.runtime.snapshotFlow { horizontal.maxValue }.first { it > 0 }
                    horizontal.scrollTo(horizontal.maxValue / 2)
                }
            }
            Box(
                Modifier.fillMaxSize()
                    .drawBehind {
                        val gap = 20.dp.toPx()
                        var x = 0f
                        while (x < size.width) {
                            var y = 0f
                            while (y < size.height) {
                                drawCircle(dots, 1.dp.toPx(), Offset(x, y))
                                y += gap
                            }
                            x += gap
                        }
                    }
                    .then(if (fill) Modifier.verticalScroll(vertical) else Modifier)
                    .then(
                        if (width * zoom > available - 24 + 1f)
                            Modifier.horizontalScroll(horizontal)
                        else Modifier
                    )
                    .padding(12.dp)
                    .then(if (fill) Modifier.padding(bottom = 96.dp) else Modifier)
            ) {
                Layout(content = content) { measurables, constraints ->
                    val child =
                        measurables.single().measure(Constraints.fixedWidth(width.dp.roundToPx()))
                    val scaledWidth = (child.width * zoom).toInt()
                    val canvasWidth = maxOf(scaledWidth, (availableWidth - 24).dp.roundToPx())
                    layout(canvasWidth, (child.height * zoom).toInt()) {
                        child.placeWithLayer((canvasWidth - scaledWidth) / 2, 0) {
                            scaleX = zoom
                            scaleY = zoom
                            transformOrigin = TransformOrigin(0f, 0f)
                        }
                    }
                }
            }
        }
        Text(
            if (editable) "Déplacez le graphe et touchez un nœud pour le modifier."
            else "Suivez les flèches de l’événement à la fin.",
            Modifier.padding(horizontal = 12.dp, vertical = 6.dp),
            style = MaterialTheme.typography.labelSmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

@Composable
private fun RuleInsertPoint(
    container: String,
    index: Int,
    depth: Int,
    insert: (String, Int, String) -> Unit,
    catalog: HookRuleCatalog?,
    executionAgentId: String?,
) {
    var expanded by remember { mutableStateOf(false) }
    val line = MaterialTheme.colorScheme.outline
    Box(
        Modifier.fillMaxWidth().height(64.dp).drawBehind {
            val x = size.width / 2
            drawLine(line, Offset(x, 0f), Offset(x, size.height), 1.5.dp.toPx())
            drawLine(
                line,
                Offset(x, size.height),
                Offset(x - 4.dp.toPx(), size.height - 6.dp.toPx()),
                1.5.dp.toPx(),
            )
            drawLine(
                line,
                Offset(x, size.height),
                Offset(x + 4.dp.toPx(), size.height - 6.dp.toPx()),
                1.5.dp.toPx(),
            )
        },
        contentAlignment = Alignment.Center,
    ) {
        OutlinedButton(
            onClick = { expanded = true },
            colors =
                ButtonDefaults.outlinedButtonColors(
                    containerColor = MaterialTheme.colorScheme.surface
                ),
            contentPadding = PaddingValues(horizontal = 12.dp, vertical = 2.dp),
            shape = CircleShape,
            modifier = Modifier.heightIn(min = 36.dp),
        ) {
            Icon(CairnIcons.Plus, null, Modifier.size(16.dp))
            Spacer(Modifier.width(4.dp))
            Text("Étape", style = MaterialTheme.typography.labelMedium)
        }
        DropdownMenu(expanded, { expanded = false }) {
            ruleStepChoices
                .filter {
                    it.kind != "callTool" ||
                        catalog?.tools?.any { tool ->
                            executionAgentId == null || executionAgentId in tool.agentIds
                        } == true
                }
                .filter {
                    !(it.kind == "wait" && container.contains("/body")) &&
                        !(depth >= 7 && it.kind in setOf("if", "forEach"))
                }
                .forEach { choice ->
                    DropdownMenuItem(
                        text = {
                            Column {
                                Text(choice.title)
                                Text(
                                    choice.description,
                                    style = MaterialTheme.typography.bodySmall,
                                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                                )
                            }
                        },
                        onClick = {
                            expanded = false
                            insert(container, index, choice.kind)
                        },
                    )
                }
        }
    }
}

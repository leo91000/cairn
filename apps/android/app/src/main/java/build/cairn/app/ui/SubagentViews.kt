@file:OptIn(androidx.compose.material3.ExperimentalMaterial3Api::class)

package build.cairn.app.ui

import androidx.compose.animation.core.FastOutSlowInEasing
import androidx.compose.animation.core.RepeatMode
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.tween
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.PrimaryScrollableTabRow
import androidx.compose.material3.Surface
import androidx.compose.material3.Tab
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.scale
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalConfiguration
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.role
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

/** Opens a subagent's full activity, when the screen hosts it for the bar and every team. */
internal val LocalOpenSubagent = compositionLocalOf<((String) -> Unit)?> { null }

private fun count(state: SubagentState, value: Int): String =
    "$value " +
        when (state) {
            SubagentState.RUNNING -> "en cours"
            SubagentState.DONE -> if (value == 1) "terminé" else "terminés"
            SubagentState.FAILED -> if (value == 1) "échec" else "échecs"
            SubagentState.STOPPED -> if (value == 1) "arrêté" else "arrêtés"
        }

private fun subagents(value: Int) = if (value == 1) "1 sous-agent" else "$value sous-agents"

@Composable
private fun SubagentState.color(): Color =
    when (this) {
        SubagentState.RUNNING -> MaterialTheme.colorScheme.primary
        SubagentState.DONE -> signal.success
        SubagentState.FAILED -> signal.attention
        SubagentState.STOPPED -> MaterialTheme.colorScheme.onSurfaceVariant
    }

/** The same colour for a subagent in the conversation, the bar and its activity. */
@Composable
internal fun SubagentAvatar(agent: Subagent, size: Dp = 28.dp, modifier: Modifier = Modifier) {
    Box(
        modifier
            .size(size)
            .clip(RoundedCornerShape(size * 0.3f))
            .background(identityColor(agent.id))
            .clearAndSetSemantics {},
        contentAlignment = Alignment.Center,
    ) {
        Text(
            subagentInitials(agent.name),
            fontSize = (size.value * 0.375f).sp,
            fontWeight = FontWeight.Bold,
            color = Color.White,
            maxLines = 1,
        )
    }
}

@Composable
private fun StateDot(state: SubagentState, size: Dp = 6.dp) {
    val pulse =
        if (state == SubagentState.RUNNING) {
            val transition = rememberInfiniteTransition(label = "subagent")
            val value by
                transition.animateFloat(
                    1f,
                    0.35f,
                    infiniteRepeatable(
                        tween(900, easing = FastOutSlowInEasing),
                        RepeatMode.Reverse,
                    ),
                    label = "pulse",
                )
            value
        } else 1f
    Box(Modifier.size(size).alpha(pulse).clip(CircleShape).background(state.color()))
}

@Composable
private fun StatePill(state: SubagentState) {
    Row(
        Modifier.clip(CircleShape)
            .background(
                when (state) {
                    SubagentState.RUNNING -> MaterialTheme.colorScheme.primary.copy(alpha = 0.12f)
                    SubagentState.FAILED -> signal.attentionSoft
                    else -> MaterialTheme.colorScheme.surfaceVariant
                }
            )
            .padding(horizontal = 8.dp, vertical = 2.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        if (state == SubagentState.RUNNING) {
            StateDot(state)
            Spacer(Modifier.width(5.dp))
        }
        Text(
            state.label,
            style = MaterialTheme.typography.labelSmall,
            fontWeight = FontWeight.Bold,
            color = state.color(),
        )
    }
}

/**
 * The subagents the agent started together: who works, on what, and for how long. Each row opens
 * the subagent's full activity. The card folds once every subagent has finished. [identity] stays
 * stable when pagination reveals earlier teammates.
 */
@Composable
internal fun SubagentTeam(agents: List<Subagent>, active: Boolean, identity: String) {
    val host = LocalOpenSubagent.current
    // Outside a hosting screen, the team hosts the activity of its own subagents.
    var opened by rememberSaveable(identity) { mutableStateOf<String?>(null) }
    val open = host ?: { id: String -> opened = id }
    val states = agents.map { it.state(active) }
    val running = states.count { it == SubagentState.RUNNING }
    val failed = states.count { it == SubagentState.FAILED }
    var toggled by rememberSaveable(identity) { mutableStateOf<Boolean?>(null) }
    val expanded = toggled ?: (running > 0 || failed > 0)
    val now = rememberNow(running > 0)
    Surface(
        Modifier.fillMaxWidth().testTag("subagent-team"),
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
                        stateDescription =
                            if (expanded) "Sous-agents affichés" else "Sous-agents masqués"
                    },
            ) {
                Row(
                    Modifier.fillMaxWidth()
                        .heightIn(min = 52.dp)
                        .padding(horizontal = 12.dp, vertical = 10.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Box(
                        Modifier.size(26.dp)
                            .clip(CircleShape)
                            .background(MaterialTheme.colorScheme.surfaceVariant),
                        contentAlignment = Alignment.Center,
                    ) {
                        Icon(
                            CairnIcons.Branch,
                            null,
                            Modifier.size(13.dp),
                            tint = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                    Spacer(Modifier.width(10.dp))
                    Column(Modifier.weight(1f)) {
                        Text(subagents(agents.size), style = MaterialTheme.typography.titleSmall)
                        Row(
                            horizontalArrangement = Arrangement.spacedBy(8.dp),
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            SubagentState.entries.forEach { state ->
                                val value = states.count { it == state }
                                if (value > 0)
                                    Row(verticalAlignment = Alignment.CenterVertically) {
                                        if (state == SubagentState.RUNNING) {
                                            StateDot(state)
                                            Spacer(Modifier.width(4.dp))
                                        }
                                        Text(
                                            count(state, value),
                                            style = MaterialTheme.typography.labelSmall,
                                            color =
                                                if (
                                                    state == SubagentState.RUNNING ||
                                                        state == SubagentState.FAILED
                                                )
                                                    state.color()
                                                else MaterialTheme.colorScheme.onSurfaceVariant,
                                        )
                                    }
                            }
                        }
                    }
                    Icon(
                        CairnIcons.Down,
                        null,
                        Modifier.size(18.dp).scale(1f, if (expanded) -1f else 1f),
                        tint = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
            if (expanded)
                agents.forEach { agent ->
                    HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
                    SubagentRow(agent, active, now) { open(agent.id) }
                }
        }
    }
    if (host == null)
        opened?.let { id ->
            SubagentSheet(agents.withNestedTeams(), id, active, { opened = it }) { opened = null }
        }
}

@Composable
private fun SubagentRow(agent: Subagent, active: Boolean, now: Long, open: () -> Unit) {
    val state = agent.state(active)
    val summary = agent.summary(active)
    val elapsed = agent.elapsed(active, now)
    Surface(
        onClick = open,
        color = Color.Transparent,
        modifier =
            Modifier.fillMaxWidth().testTag("subagent").semantics {
                contentDescription =
                    listOf(agent.name, state.label, summary, "Ouvrir son activité")
                        .joinToString(", ")
            },
    ) {
        Row(
            Modifier.fillMaxWidth()
                .heightIn(min = 56.dp)
                .padding(horizontal = 12.dp, vertical = 10.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            SubagentAvatar(agent)
            Spacer(Modifier.width(12.dp))
            Column(Modifier.weight(1f)) {
                Row(verticalAlignment = Alignment.Bottom) {
                    Text(
                        agent.name,
                        Modifier.weight(1f, fill = false),
                        style = MaterialTheme.typography.titleSmall,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                    val kind =
                        listOf(
                                agent.type,
                                if (agent.background) "arrière-plan" else "",
                                if (agent.resumed) "relancé" else "",
                            )
                            .filter { it.isNotBlank() }
                            .joinToString(" · ")
                    // Phones keep the room for the name; the sheet names the type.
                    if (kind.isNotBlank() && LocalConfiguration.current.screenWidthDp >= 600)
                        Text(
                            "  $kind",
                            style = MaterialTheme.typography.labelSmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis,
                        )
                }
                Text(
                    summary,
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
            }
            if (elapsed.isNotBlank())
                Text(
                    elapsed,
                    Modifier.padding(horizontal = 8.dp),
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            StatePill(state)
        }
    }
}

/**
 * The subagents of the current request, above the composer so they stay in sight while the
 * conversation scrolls. Each one opens its full activity; on phones they fold into one button.
 */
@Composable
internal fun SubagentBar(agents: List<Subagent>, active: Boolean, open: (String) -> Unit) {
    val running = agents.filter { it.state(active) == SubagentState.RUNNING }
    val first = running.firstOrNull() ?: agents.firstOrNull() ?: return
    val now = rememberNow(running.size)
    val label = if (running.size == 1) "1 sous-agent" else "${running.size} sous-agents"
    BoxWithConstraints(
        Modifier.fillMaxWidth()
            .padding(horizontal = 12.dp)
            .padding(top = 4.dp)
            .testTag("subagent-bar")
    ) {
        val wide = maxWidth >= 600.dp
        Surface(
            shape = RoundedCornerShape(16.dp),
            color = MaterialTheme.colorScheme.surface,
            border = BorderStroke(1.dp, MaterialTheme.colorScheme.outlineVariant),
        ) {
            if (wide)
                Row(
                    Modifier.fillMaxWidth().padding(horizontal = 10.dp, vertical = 6.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Text(
                        "Sous-agents",
                        style = MaterialTheme.typography.labelMedium,
                        fontWeight = FontWeight.SemiBold,
                    )
                    Spacer(Modifier.width(8.dp))
                    Row(
                        Modifier.weight(1f).horizontalScroll(rememberScrollState()),
                        horizontalArrangement = Arrangement.spacedBy(6.dp),
                    ) {
                        agents.forEach { agent ->
                            val state = agent.state(active)
                            Surface(
                                onClick = { open(agent.id) },
                                shape = RoundedCornerShape(10.dp),
                                color = MaterialTheme.colorScheme.surfaceVariant,
                                modifier =
                                    Modifier.testTag("subagent-chip").semantics {
                                        contentDescription =
                                            "${agent.name}, ${state.label}. Ouvrir son activité"
                                    },
                            ) {
                                Row(
                                    Modifier.heightIn(min = 40.dp)
                                        .padding(start = 4.dp, end = 8.dp),
                                    verticalAlignment = Alignment.CenterVertically,
                                ) {
                                    SubagentAvatar(agent, 22.dp)
                                    Spacer(Modifier.width(6.dp))
                                    Text(
                                        agent.name,
                                        Modifier.widthIn(max = 160.dp),
                                        style = MaterialTheme.typography.labelMedium,
                                        maxLines = 1,
                                        overflow = TextOverflow.Ellipsis,
                                    )
                                    Spacer(Modifier.width(6.dp))
                                    StateDot(state)
                                    if (state == SubagentState.RUNNING)
                                        Text(
                                            " ${agent.elapsed(active, now)}",
                                            style = MaterialTheme.typography.labelSmall,
                                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                                        )
                                }
                            }
                        }
                    }
                    Spacer(Modifier.width(8.dp))
                    Text(
                        "${running.size} en cours",
                        style = MaterialTheme.typography.labelSmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            else
                Surface(
                    onClick = { open(first.id) },
                    color = Color.Transparent,
                    modifier =
                        Modifier.fillMaxWidth().semantics {
                            contentDescription = "$label au travail. Ouvrir leur activité"
                        },
                ) {
                    Row(
                        Modifier.heightIn(min = 48.dp).padding(horizontal = 10.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        Row(horizontalArrangement = Arrangement.spacedBy((-6).dp)) {
                            agents.take(3).forEach { agent ->
                                SubagentAvatar(
                                    agent,
                                    24.dp,
                                    Modifier.border(
                                        2.dp,
                                        MaterialTheme.colorScheme.surface,
                                        RoundedCornerShape(7.dp),
                                    ),
                                )
                            }
                        }
                        Spacer(Modifier.width(10.dp))
                        Text(
                            label,
                            style = MaterialTheme.typography.labelLarge,
                            fontWeight = FontWeight.SemiBold,
                        )
                        Text(
                            " au travail",
                            Modifier.weight(1f),
                            style = MaterialTheme.typography.labelLarge,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis,
                        )
                        StateDot(SubagentState.RUNNING)
                    }
                }
        }
    }
}

/**
 * A subagent's full activity: its prompt, every step with its output, and its report. Subagents
 * started together share the sheet as tabs; a nested team opens in its place.
 */
@Composable
internal fun SubagentSheet(
    teams: List<List<Subagent>>,
    selected: String,
    active: Boolean,
    select: (String) -> Unit,
    close: () -> Unit,
) {
    val team = teams.firstOrNull { members -> members.any { it.id == selected } }
    if (team == null) {
        // The subagent left the loaded history.
        LaunchedEffect(selected) { close() }
        return
    }
    val agent = team.first { it.id == selected }
    val state = agent.state(active)
    val now = rememberNow(state)
    var promptOpen by remember(selected) { mutableStateOf(false) }
    var opened by remember(selected) { mutableStateOf<Int?>(null) }
    ModalBottomSheet(
        onDismissRequest = close,
        sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true),
        containerColor = MaterialTheme.colorScheme.background,
    ) {
        Column(Modifier.fillMaxWidth().fillMaxHeight(0.9f).testTag("subagent-sheet")) {
            Row(
                Modifier.fillMaxWidth().padding(start = 8.dp, end = 8.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                if (team.size > 1)
                    PrimaryScrollableTabRow(
                        selectedTabIndex = team.indexOf(agent),
                        modifier = Modifier.weight(1f),
                        containerColor = MaterialTheme.colorScheme.background,
                        edgePadding = 0.dp,
                    ) {
                        team.forEach { item ->
                            Tab(
                                selected = item.id == agent.id,
                                onClick = { select(item.id) },
                                text = {
                                    Row(verticalAlignment = Alignment.CenterVertically) {
                                        SubagentAvatar(item, 16.dp)
                                        Spacer(Modifier.width(6.dp))
                                        Text(
                                            item.name,
                                            Modifier.widthIn(max = 128.dp),
                                            maxLines = 1,
                                            overflow = TextOverflow.Ellipsis,
                                        )
                                        Spacer(Modifier.width(6.dp))
                                        StateDot(item.state(active))
                                    }
                                },
                            )
                        }
                    }
                else
                    Text(
                        "Activité du sous-agent",
                        Modifier.weight(1f).padding(start = 12.dp),
                        style = MaterialTheme.typography.labelLarge,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                ActionIcon("Fermer l’activité du sous-agent", CairnIcons.Close, onClick = close)
            }
            HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
            Column(
                Modifier.fillMaxWidth()
                    .weight(1f)
                    .verticalScroll(rememberScrollState())
                    .padding(horizontal = 20.dp)
                    .padding(top = 16.dp, bottom = 24.dp),
                verticalArrangement = Arrangement.spacedBy(12.dp),
            ) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    SubagentAvatar(agent, 36.dp)
                    Spacer(Modifier.width(12.dp))
                    Column(Modifier.weight(1f)) {
                        Text(
                            agent.name,
                            style = MaterialTheme.typography.titleMedium,
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis,
                        )
                        Text(
                            listOf(
                                    agent.type,
                                    if (agent.background) "arrière-plan" else "",
                                    if (agent.resumed) "relancé par l’agent"
                                    else "lancé par l’agent",
                                )
                                .filter { it.isNotBlank() }
                                .joinToString(" · "),
                            style = MaterialTheme.typography.labelSmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis,
                        )
                    }
                    StatePill(state)
                }
                Text(
                    listOf(
                            agent.elapsed(active, now),
                            if (agent.steps.size == 1) "1 étape" else "${agent.steps.size} étapes",
                            agent.details(),
                        )
                        .filter { it.isNotBlank() }
                        .joinToString(" · "),
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                if (agent.prompt.isNotBlank())
                    Surface(
                        shape = RoundedCornerShape(12.dp),
                        color = MaterialTheme.colorScheme.surfaceVariant,
                    ) {
                        Column(
                            Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 8.dp)
                        ) {
                            Text(
                                "Consigne · ${agent.prompt}",
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                                maxLines = if (promptOpen) Int.MAX_VALUE else 3,
                                overflow = TextOverflow.Ellipsis,
                            )
                            if (agent.prompt.length > 160 || agent.prompt.lines().size > 3)
                                TextButton(onClick = { promptOpen = !promptOpen }) {
                                    Text(if (promptOpen) "Réduire" else "Tout afficher")
                                }
                        }
                    }
                // The subagent's own steps between its delegations, in their order.
                val runs = remember(agent.steps) { stepRuns(agent.steps) }
                val steps = remember(runs) { runs.mapNotNull { it.actions }.flatten() }
                val teamKeys = remember(selected) { SubagentTeamKeys() }
                val teamIdentities =
                    remember(runs, teamKeys) {
                        teamKeys.stabilize(runs.filter { it.agents.isNotEmpty() }.map { it.agents })
                    }
                if (agent.steps.isEmpty())
                    Text(
                        if (state == SubagentState.RUNNING) "Aucune étape pour l’instant."
                        else "Ce sous-agent n’a signalé aucune étape.",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                else
                    Column(Modifier.testTag("subagent-steps")) {
                        var position = 0
                        var teamPosition = 0
                        runs.forEach { run ->
                            val actions = run.actions
                            if (actions == null) {
                                val identity = teamIdentities[teamPosition++]
                                key(identity) {
                                    CompositionLocalProvider(LocalOpenSubagent provides select) {
                                        Box(Modifier.padding(vertical = 6.dp)) {
                                            SubagentTeam(run.agents, active, identity)
                                        }
                                    }
                                }
                            } else {
                                val start = position
                                actions.forEachIndexed { index, step ->
                                    StepRow(step, last = index == actions.lastIndex) {
                                        opened = start + index
                                    }
                                }
                                position += actions.size
                            }
                        }
                    }
                val report = agent.result.ifBlank { agent.message }
                if (report.isNotBlank())
                    Surface(
                        shape = RoundedCornerShape(12.dp),
                        color = MaterialTheme.colorScheme.background,
                        border = BorderStroke(1.dp, MaterialTheme.colorScheme.outlineVariant),
                    ) {
                        Column(
                            Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 8.dp)
                        ) {
                            Text(
                                if (agent.result.isNotBlank()) "Rapport" else "Dernier message",
                                style = MaterialTheme.typography.labelSmall,
                                fontWeight = FontWeight.SemiBold,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                            Markdown(report)
                        }
                    }
                opened?.let { index ->
                    val step = steps.getOrNull(index)
                    if (step == null) LaunchedEffect(index) { opened = null }
                    else StepSheet(step, index, steps.size) { opened = null }
                }
            }
        }
    }
}

/** Consecutive actions, or consecutive delegations, in the order the subagent took them. */
private class StepRun(val agents: List<Subagent>, val actions: List<AgentStep>?)

private fun stepRuns(steps: List<SubagentStep>): List<StepRun> {
    val runs = mutableListOf<MutableList<SubagentStep>>()
    for (step in steps) {
        val last = runs.lastOrNull()
        if (last != null && (last.first() is SubagentStep.Agent) == (step is SubagentStep.Agent))
            last.add(step)
        else runs.add(mutableListOf(step))
    }
    return runs.map { run ->
        val agents = run.filterIsInstance<SubagentStep.Agent>().map { it.agent }
        StepRun(
            agents,
            if (agents.isNotEmpty()) null
            else agentSteps(run.filterIsInstance<SubagentStep.Action>().map { it.activity }),
        )
    }
}

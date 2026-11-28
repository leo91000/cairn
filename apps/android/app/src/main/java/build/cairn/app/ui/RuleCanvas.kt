@file:OptIn(androidx.compose.material3.ExperimentalMaterial3Api::class)

package build.cairn.app.ui

import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.core.Animatable
import androidx.compose.animation.core.RepeatMode
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.spring
import androidx.compose.animation.core.tween
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.slideInVertically
import androidx.compose.animation.slideOutVertically
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import build.cairn.app.data.*
import build.cairn.contracts.HookRuleCatalog
import build.cairn.contracts.RuleCatalogHook
import kotlinx.serialization.json.*

// The rule's canvas around its steps: the node of what starts it, the sheets that choose an event
// and whose rights, a short message when saving is premature, and the moment a rule is created.

/**
 * The rule's first node: empty and breathing until an event is chosen, then the event and agent.
 */
@Composable
internal fun RuleStartNode(
    chosen: Boolean,
    hook: RuleCatalogHook?,
    fallback: String,
    agentId: String,
    agentName: String,
    problems: List<String>,
    attention: Boolean,
    attentionCount: Int,
    enabled: Boolean,
    chooseEvent: () -> Unit,
    chooseAgent: () -> Unit,
) {
    val accent = MaterialTheme.colorScheme.primary
    val empty = !chosen
    val breath =
        if (empty)
            rememberInfiniteTransition(label = "start")
                .animateFloat(
                    0f,
                    1f,
                    infiniteRepeatable(tween(1200), RepeatMode.Reverse),
                    label = "breath",
                )
                .value
        else 0f
    Surface(
        shape = RoundedCornerShape(20.dp),
        color =
            if (empty) accent.copy(alpha = 0.06f)
            else MaterialTheme.colorScheme.surfaceContainerLow,
        border =
            BorderStroke(
                if (empty) 2.dp else 1.dp,
                if (problems.isNotEmpty()) MaterialTheme.colorScheme.error
                else if (empty) accent.copy(alpha = 0.55f + 0.45f * breath)
                else MaterialTheme.colorScheme.outlineVariant,
            ),
        modifier =
            Modifier.width(256.dp).ruleAttention(attention, attentionCount).testTag("rule-start"),
    ) {
        Column {
            Row(
                Modifier.fillMaxWidth()
                    .clickable(enabled = enabled, role = Role.Button, onClick = chooseEvent)
                    .semantics {
                        contentDescription =
                            if (!chosen) "Choisir ce qui déclenche la règle"
                            else "Changer ce qui déclenche la règle : ${hook?.title ?: fallback}"
                    }
                    .padding(14.dp),
                horizontalArrangement = Arrangement.spacedBy(12.dp),
            ) {
                if (hook?.pluginId != null) PluginMark(hook.pluginId, 36.dp, hook.brand)
                else
                    Box(
                        Modifier.size(36.dp)
                            .clip(RoundedCornerShape(11.dp))
                            .background(accent.copy(alpha = 0.12f)),
                        contentAlignment = Alignment.Center,
                    ) {
                        Icon(CairnIcons.Orbit, null, Modifier.size(18.dp), tint = accent)
                    }
                Column(Modifier.weight(1f)) {
                    Text(
                        "QUAND",
                        style = MaterialTheme.typography.labelSmall,
                        color = accent,
                        fontWeight = FontWeight.Bold,
                    )
                    Text(
                        if (!chosen) "Choisir ce qui déclenche la règle"
                        else hook?.title ?: "$fallback (plus disponible)",
                        style = MaterialTheme.typography.titleSmall,
                        fontWeight = FontWeight.SemiBold,
                    )
                    Text(
                        if (!chosen) "Un message, une pull request, un mail…"
                        else hook?.instanceName ?: "Conversations Cairn",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
            }
            if (chosen)
                Row(
                    Modifier.padding(start = 14.dp, end = 14.dp, bottom = 12.dp)
                        .clip(CircleShape)
                        .background(MaterialTheme.colorScheme.surfaceContainerHigh)
                        .clickable(enabled = enabled, role = Role.Button, onClick = chooseAgent)
                        .semantics { contentDescription = "Avec les droits de $agentName" }
                        .padding(start = 2.dp, end = 10.dp, top = 2.dp, bottom = 2.dp),
                    horizontalArrangement = Arrangement.spacedBy(6.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    AgentAvatar(agentName, agentId, 20.dp)
                    Text(
                        agentName,
                        style = MaterialTheme.typography.labelMedium,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
            problems.forEach {
                Text(
                    it,
                    Modifier.padding(start = 14.dp, end = 14.dp, bottom = 12.dp),
                    color = MaterialTheme.colorScheme.error,
                    style = MaterialTheme.typography.bodySmall,
                )
            }
        }
    }
}

/** Every event the person's agents may listen to, grouped by the plugin instance it comes from. */
@Composable
internal fun RuleEventPicker(
    catalog: HookRuleCatalog,
    current: String?,
    query: String,
    onQuery: (String) -> Unit,
    dismiss: () -> Unit,
    choose: (RuleCatalogHook) -> Unit,
) {
    val needle = query.trim().lowercase()
    val groups =
        catalog.hooks
            .filter {
                needle.isEmpty() ||
                    "${it.title} ${it.description} ${it.instanceName.orEmpty()}"
                        .lowercase()
                        .contains(needle)
            }
            .groupBy { wireJson.encodeToJsonElement(it.trigger).text("instanceId") }
    ModalBottomSheet(
        onDismissRequest = dismiss,
        sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true),
    ) {
        Column(Modifier.fillMaxWidth().fillMaxHeight(0.9f).padding(horizontal = 16.dp)) {
            Text(
                "Ce qui déclenche la règle",
                Modifier.padding(horizontal = 4.dp),
                style = MaterialTheme.typography.titleLarge,
                fontWeight = FontWeight.Bold,
            )
            OutlinedTextField(
                query,
                onQuery,
                Modifier.fillMaxWidth().padding(vertical = 10.dp),
                placeholder = { Text("Rechercher : message, pull request, mail…") },
                leadingIcon = { Icon(CairnIcons.Search, null) },
                singleLine = true,
                shape = RoundedCornerShape(14.dp),
            )
            LazyColumn(Modifier.weight(1f), contentPadding = PaddingValues(bottom = 24.dp)) {
                groups.forEach { (_, hooks) ->
                    val first = hooks.first()
                    item {
                        Row(
                            Modifier.padding(start = 4.dp, top = 12.dp, bottom = 4.dp),
                            horizontalArrangement = Arrangement.spacedBy(8.dp),
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            if (first.pluginId != null)
                                PluginMark(first.pluginId, 18.dp, first.brand)
                            else
                                Icon(
                                    CairnIcons.Orbit,
                                    null,
                                    Modifier.size(16.dp),
                                    tint = MaterialTheme.colorScheme.primary,
                                )
                            Text(
                                first.instanceName ?: "Conversations Cairn",
                                style = MaterialTheme.typography.labelLarge,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        }
                    }
                    items(hooks, key = { triggerKey(wireJson.encodeToJsonElement(it.trigger)) }) {
                        hook ->
                        val key = triggerKey(wireJson.encodeToJsonElement(hook.trigger))
                        Row(
                            Modifier.fillMaxWidth()
                                .clip(RoundedCornerShape(14.dp))
                                .clickable(role = Role.Button) { choose(hook) }
                                .padding(horizontal = 8.dp, vertical = 10.dp),
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            Column(Modifier.weight(1f)) {
                                Text(hook.title, style = MaterialTheme.typography.titleSmall)
                                Text(
                                    hook.description,
                                    style = MaterialTheme.typography.bodySmall,
                                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                                )
                            }
                            if (key == current)
                                Icon(
                                    CairnIcons.Check,
                                    "Choisi",
                                    tint = MaterialTheme.colorScheme.primary,
                                )
                        }
                    }
                }
                if (groups.isEmpty())
                    item {
                        Text(
                            "Aucun événement ne correspond. Les agents n’offrent que les hooks que le propriétaire leur a accordés.",
                            Modifier.padding(vertical = 24.dp),
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
            }
        }
    }
}

/** The agents the person may use; only those holding the event's hook can be chosen. */
@Composable
internal fun RuleAgentPicker(
    catalog: HookRuleCatalog,
    hook: RuleCatalogHook?,
    current: String,
    agentName: (String) -> String,
    dismiss: () -> Unit,
    choose: (String) -> Unit,
) {
    ModalBottomSheet(onDismissRequest = dismiss) {
        Column(Modifier.fillMaxWidth().padding(horizontal = 16.dp).padding(bottom = 28.dp)) {
            Text(
                "Avec les droits de…",
                Modifier.padding(horizontal = 4.dp, vertical = 6.dp),
                style = MaterialTheme.typography.titleLarge,
                fontWeight = FontWeight.Bold,
            )
            catalog.agents.forEach { agent ->
                val allowed = hook == null || agent.agentId in hook.agentIds
                Row(
                    Modifier.fillMaxWidth()
                        .clip(RoundedCornerShape(14.dp))
                        .clickable(enabled = allowed, role = Role.Button) { choose(agent.agentId) }
                        .graphicsLayer { alpha = if (allowed) 1f else 0.45f }
                        .padding(horizontal = 8.dp, vertical = 10.dp),
                    horizontalArrangement = Arrangement.spacedBy(12.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    AgentAvatar(
                        agent.name.ifBlank { agentName(agent.agentId) },
                        agent.agentId,
                        34.dp,
                    )
                    Column(Modifier.weight(1f)) {
                        Text(agent.name, style = MaterialTheme.typography.titleSmall)
                        if (!allowed)
                            Text(
                                "Ne peut pas écouter cet événement",
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                    }
                    if (agent.agentId == current)
                        Icon(CairnIcons.Check, "Choisi", tint = MaterialTheme.colorScheme.primary)
                }
            }
            Text(
                "La règle agit avec les droits de cet agent : les outils qu’elle appelle sont les siens.",
                Modifier.padding(horizontal = 8.dp, vertical = 10.dp),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
}

/** What is missing, said once over the canvas when saving comes too early. */
@Composable
internal fun RuleNudge(message: String, modifier: Modifier = Modifier) {
    var shown by remember { mutableStateOf("") }
    if (message.isNotBlank()) shown = message
    AnimatedVisibility(
        message.isNotBlank(),
        modifier.padding(bottom = 24.dp, start = 24.dp, end = 24.dp),
        enter = fadeIn() + slideInVertically { it / 2 },
        exit = fadeOut() + slideOutVertically { it / 2 },
    ) {
        Surface(
            shape = RoundedCornerShape(14.dp),
            color = MaterialTheme.colorScheme.inverseSurface,
            shadowElevation = 6.dp,
        ) {
            Text(
                shown,
                Modifier.padding(horizontal = 16.dp, vertical = 11.dp),
                color = MaterialTheme.colorScheme.inverseOnSurface,
                style = MaterialTheme.typography.labelLarge,
            )
        }
    }
}

/** Three stones settle on each other: the rule is created. */
@Composable
internal fun RuleCelebration() {
    val stones =
        listOf(44.dp to 15.dp, 32.dp to 13.dp, 22.dp to 11.dp).mapIndexed { index, size ->
            val drop = remember { Animatable(0f) }
            LaunchedEffect(Unit) {
                kotlinx.coroutines.delay(100L + index * 200L)
                drop.animateTo(1f, spring(dampingRatio = 0.55f, stiffness = 400f))
            }
            Triple(size, drop.value, index)
        }
    Box(
        Modifier.fillMaxSize()
            .background(MaterialTheme.colorScheme.surface.copy(alpha = 0.94f))
            .clickable(enabled = false) {}
            .semantics { contentDescription = "Votre règle est active" },
        contentAlignment = Alignment.Center,
    ) {
        Column(horizontalAlignment = Alignment.CenterHorizontally) {
            Column(
                horizontalAlignment = Alignment.CenterHorizontally,
                verticalArrangement = Arrangement.spacedBy(3.dp),
            ) {
                stones.reversed().forEach { (size, progress, index) ->
                    Box(
                        Modifier.size(size.first, size.second)
                            .graphicsLayer {
                                alpha = progress.coerceIn(0f, 1f)
                                translationY = (1f - progress) * -40f
                            }
                            .clip(CircleShape)
                            .background(
                                if (index == 2) MaterialTheme.colorScheme.tertiary
                                else MaterialTheme.colorScheme.onSurface
                            )
                    )
                }
            }
            Spacer(Modifier.height(20.dp))
            Text(
                "Votre règle est active",
                style = MaterialTheme.typography.titleLarge,
                fontWeight = FontWeight.Bold,
            )
        }
    }
}

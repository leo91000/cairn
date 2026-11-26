@file:OptIn(
    androidx.compose.material3.ExperimentalMaterial3Api::class,
    androidx.compose.foundation.layout.ExperimentalLayoutApi::class,
    androidx.compose.foundation.ExperimentalFoundationApi::class,
)

package build.cairn.app.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.filled.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.role
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import build.cairn.app.data.*
import kotlinx.serialization.json.*

@Composable
internal fun ConversationList(
    activeChats: List<Chat>,
    selected: String?,
    loading: Boolean,
    error: String?,
    open: (String) -> Unit,
    create: () -> Unit,
    vm: CairnViewModel,
) {
    var view by rememberSaveable { mutableStateOf("active") }
    var menu by remember { mutableStateOf(false) }
    val usableAgents = vm.state.collectAsState().value.usableAgents
    var other by remember { mutableStateOf<List<Chat>>(emptyList()) }
    var revision by remember { mutableIntStateOf(0) }
    var listError by remember { mutableStateOf<String?>(null) }
    var fetching by remember { mutableStateOf(false) }
    LaunchedEffect(view, revision) {
        other = emptyList()
        if (view != "active") {
            fetching = true
            try {
                other = vm.api.get("/chats?view=$view")
            } catch (e: Exception) {
                listError = e.message
            } finally {
                fetching = false
            }
        }
    }
    // The list can sit in a bottom sheet, above the app snackbar: it hosts its own undo.
    val snackbar = remember { SnackbarHostState() }
    val trash = rememberConversationTrash(vm, snackbar) { revision++ }
    val chats =
        when (view) {
            "active" -> activeChats.filter { it.id !in trash.hidden }
            "trash" -> other
            else -> other.filter { it.id !in trash.hidden }
        }
    var query by rememberSaveable { mutableStateOf("") }
    val today =
        java.time.LocalDate.now()
            .atStartOfDay(java.time.ZoneId.systemDefault())
            .toInstant()
            .toEpochMilli()
    val yesterday =
        java.time.LocalDate.now()
            .minusDays(1)
            .atStartOfDay(java.time.ZoneId.systemDefault())
            .toInstant()
            .toEpochMilli()
    val filtered =
        chats
            .filter {
                "${it.title} ${it.agentName} ${it.projectName.orEmpty()}"
                    .contains(query.trim(), true)
            }
            .sortedByDescending { it.updatedAt }
    val groups =
        listOf(
            "Aujourd’hui" to filtered.filter { it.updatedAt >= today },
            "Hier" to filtered.filter { it.updatedAt in yesterday until today },
            "Plus tôt" to filtered.filter { it.updatedAt < yesterday },
        )
    Box(Modifier.fillMaxSize()) {
        Column(Modifier.fillMaxSize().padding(horizontal = 16.dp)) {
            Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
                Text(
                    "Conversations",
                    Modifier.weight(1f),
                    style = MaterialTheme.typography.headlineSmall,
                )
                Box {
                    ActionIcon("Afficher les conversations", CairnIcons.More) { menu = true }
                    DropdownMenu(menu, { menu = false }) {
                        listOf("active" to "Actives", "trash" to "Corbeille").forEach { (key, label)
                            ->
                            DropdownMenuItem(
                                text = { Text(label) },
                                onClick = {
                                    view = key
                                    menu = false
                                },
                            )
                        }
                    }
                }
                ActionIcon("Nouvelle conversation", CairnIcons.Plus, onClick = create)
            }
            if (view != "active") Text("Corbeille", style = MaterialTheme.typography.labelMedium)
            SearchField("Rechercher une conversation", query) { query = it }
            if (loading || fetching) LinearProgressIndicator(Modifier.fillMaxWidth())
            (listError ?: error)?.let { Text(it, color = MaterialTheme.colorScheme.error) }
            LazyColumn(
                Modifier.weight(1f),
                contentPadding = PaddingValues(vertical = 12.dp),
                verticalArrangement = Arrangement.spacedBy(4.dp),
            ) {
                if (!loading && filtered.isEmpty())
                    item {
                        Empty(
                            if (query.isBlank()) "Aucune conversation" else "Aucun résultat",
                            if (query.isBlank()) "Confiez une mission à votre agent pour commencer."
                            else "Essayez un autre titre, agent ou projet.",
                        )
                    }
                groups
                    .filter { it.second.isNotEmpty() }
                    .forEach { (label, items) ->
                        item(key = label) {
                            Text(
                                label,
                                Modifier.padding(top = 12.dp, bottom = 8.dp, start = 8.dp),
                                style = MaterialTheme.typography.labelMedium,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        }
                        items(items, key = { it.id }) { chat ->
                            Box(Modifier.animateItem()) {
                                key(view) {
                                    SwipeToTrashRow(
                                        // Deleting needs access to the conversation's agent.
                                        enabled =
                                            view != "trash" &&
                                                canUseAgent(usableAgents, chat.agentId),
                                        onTrash = { trash.trash(chat.id) },
                                    ) { swipe ->
                                        Surface(
                                            modifier = swipe,
                                            onClick = { open(chat.id) },
                                            color =
                                                if (chat.id == selected)
                                                    MaterialTheme.colorScheme.primaryContainer
                                                else
                                                    MaterialTheme.colorScheme.surfaceContainerLow
                                                        .copy(alpha = 0f),
                                            shape = RoundedCornerShape(16.dp),
                                        ) {
                                            Row(
                                                Modifier.fillMaxWidth()
                                                    .heightIn(min = 64.dp)
                                                    .padding(horizontal = 8.dp, vertical = 8.dp),
                                                verticalAlignment = Alignment.CenterVertically,
                                            ) {
                                                val working =
                                                    chat.pendingQuestions == 0L &&
                                                        !chat.paused &&
                                                        !chat.exclusiveSuspended &&
                                                        chat.status == "running"
                                                if (working)
                                                    WorkingAvatar(
                                                        chat.agentName.ifBlank { chat.title },
                                                        chat.agentId,
                                                        40.dp,
                                                    )
                                                else
                                                    AgentAvatar(
                                                        chat.agentName.ifBlank { chat.title },
                                                        chat.agentId,
                                                        40.dp,
                                                        when {
                                                            chat.pendingQuestions > 0 ||
                                                                chat.status in
                                                                    setOf(
                                                                        "failed",
                                                                        "interrupted",
                                                                    ) -> AvatarBadge.ATTENTION
                                                            !chat.paused &&
                                                                !chat.exclusiveSuspended &&
                                                                chat.status == "queued" ->
                                                                AvatarBadge.LIVE
                                                            else -> null
                                                        },
                                                    )
                                                Spacer(Modifier.width(12.dp))
                                                Column(Modifier.weight(1f)) {
                                                    Text(
                                                        chat.title,
                                                        style = MaterialTheme.typography.titleSmall,
                                                        maxLines = 2,
                                                        overflow = TextOverflow.Ellipsis,
                                                    )
                                                    if (working)
                                                        WorkingLabel(statusLabel(chat.status))
                                                    else
                                                        Text(
                                                            when {
                                                                chat.pendingQuestions > 0 ->
                                                                    "${chat.pendingQuestions} question(s) en attente"
                                                                chat.paused -> "En pause"
                                                                chat.exclusiveSuspended ->
                                                                    "Suspendue (exclusivité)"
                                                                chat.status in
                                                                    listOf(
                                                                        "running",
                                                                        "queued",
                                                                        "failed",
                                                                        "interrupted",
                                                                    ) -> statusLabel(chat.status)
                                                                chat.wakeup != null &&
                                                                    chat.status == "succeeded" ->
                                                                    waitTarget(chat.wakeup).short
                                                                else ->
                                                                    listOfNotNull(
                                                                            chat.agentName.ifBlank {
                                                                                null
                                                                            },
                                                                            chat.projectName,
                                                                        )
                                                                        .joinToString(" · ")
                                                            },
                                                            style =
                                                                MaterialTheme.typography.bodySmall,
                                                            color =
                                                                if (
                                                                    chat.pendingQuestions > 0 ||
                                                                        chat.status in
                                                                            setOf(
                                                                                "failed",
                                                                                "interrupted",
                                                                            )
                                                                )
                                                                    signal.attention
                                                                else
                                                                    MaterialTheme.colorScheme
                                                                        .onSurfaceVariant,
                                                            maxLines = 1,
                                                            overflow = TextOverflow.Ellipsis,
                                                        )
                                                }
                                                Spacer(Modifier.width(8.dp))
                                                Text(
                                                    shortStamp(chat.updatedAt),
                                                    style = MaterialTheme.typography.labelSmall,
                                                    color =
                                                        MaterialTheme.colorScheme.onSurfaceVariant,
                                                )
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
            }
        }
        SnackbarHost(snackbar, Modifier.align(Alignment.BottomCenter))
    }
}

/**
 * A saved conversation is current when the Fil shows nothing new since it was saved: the same
 * finished turn, no work in progress and no later change, as on the web.
 */
@Composable
internal fun NewConversationIntro(
    agents: List<Agent>,
    agent: String,
    projects: List<Project>,
    project: String,
    enabled: Boolean,
    chooseAgent: (String) -> Unit,
    chooseProject: (String) -> Unit,
) {
    Column(Modifier.fillMaxWidth().testTag("new-conversation")) {
        Text("On lance quoi ?", style = MaterialTheme.typography.headlineLarge)
        Eyebrow("Avec", Modifier.padding(top = 20.dp, bottom = 10.dp))
        androidx.compose.foundation.lazy.LazyRow(
            horizontalArrangement = Arrangement.spacedBy(10.dp)
        ) {
            items(agents, key = { it.id }) { item ->
                AgentTile(item, item.id == agent, enabled) { chooseAgent(item.id) }
            }
        }
        Eyebrow("Dans", Modifier.padding(top = 20.dp, bottom = 4.dp))
        FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            SignalChip("Tous les projets autorisés", project.isEmpty()) {
                if (enabled) chooseProject("")
            }
            projects.forEach {
                SignalChip(it.name, project == it.id, leading = identityColor(it.id)) {
                    if (enabled) chooseProject(it.id)
                }
            }
        }
        Text(
            "Écrivez votre message ou joignez un fichier pour commencer.",
            Modifier.padding(top = 12.dp),
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

@Composable
internal fun AgentTile(agent: Agent, selected: Boolean, enabled: Boolean, choose: () -> Unit) {
    val shape = RoundedCornerShape(22.dp)
    Surface(
        onClick = choose,
        enabled = enabled,
        shape = shape,
        color =
            if (selected) MaterialTheme.colorScheme.surface
            else MaterialTheme.colorScheme.background,
        border =
            androidx.compose.foundation.BorderStroke(
                if (selected) 2.dp else 1.dp,
                if (selected) MaterialTheme.colorScheme.primary
                else MaterialTheme.colorScheme.outlineVariant,
            ),
        modifier =
            Modifier.width(104.dp).semantics {
                this.selected = selected
                role = androidx.compose.ui.semantics.Role.RadioButton
            },
    ) {
        Column(
            Modifier.padding(vertical = 14.dp, horizontal = 8.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
        ) {
            Box {
                AgentAvatar(agent.name, agent.id, 48.dp)
                if (selected)
                    Box(
                        Modifier.align(Alignment.BottomEnd)
                            .offset(6.dp, 6.dp)
                            .size(20.dp)
                            .clip(androidx.compose.foundation.shape.CircleShape)
                            .background(MaterialTheme.colorScheme.surface)
                            .padding(2.dp)
                            .clip(androidx.compose.foundation.shape.CircleShape)
                            .background(MaterialTheme.colorScheme.primary),
                        contentAlignment = Alignment.Center,
                    ) {
                        Icon(
                            CairnIcons.Check,
                            null,
                            Modifier.size(11.dp),
                            tint = MaterialTheme.colorScheme.onPrimary,
                        )
                    }
            }
            Text(
                agent.name,
                Modifier.padding(top = 10.dp),
                style = MaterialTheme.typography.titleSmall,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            Text(
                providerLabel(agent.provider),
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                maxLines = 1,
            )
        }
    }
}

package dev.leo.manager.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.unit.dp
import dev.leo.manager.data.*

internal enum class SearchKind(val label: String) {
    CHAT("Conversations"),
    MISSION("Missions"),
    AGENT("Agents"),
    PROJECT("Projets"),
    SKILL("Skills"),
}

internal data class SearchHit(
    val kind: SearchKind,
    val id: String,
    val title: String,
    val detail: String,
    val key: String = id,
    /** Name shown in the identity avatar for conversations and agents. */
    val avatar: String = title,
)

/** Matches titles, agents, projects, tags and descriptions; titles first. */
internal fun searchWorkspace(query: String, chats: List<Chat>, state: Workspace): List<SearchHit> {
    val q = query.trim()
    if (q.isEmpty()) return emptyList()
    fun String.hit() = contains(q, ignoreCase = true)
    val hits = mutableListOf<Pair<Int, SearchHit>>()
    chats.forEach {
        val extra = "${it.agentName} ${it.projectName.orEmpty()}"
        if (it.title.hit() || extra.hit())
            hits +=
                (if (it.title.hit()) 0 else 1) to
                    SearchHit(
                        SearchKind.CHAT,
                        it.id,
                        it.title,
                        listOfNotNull(it.agentName.ifBlank { null }, it.projectName)
                            .joinToString(" · "),
                        it.agentId,
                        it.agentName.ifBlank { it.title },
                    )
    }
    state.tasks
        .filter { !it.archived }
        .forEach {
            if (it.name.hit() || it.tags.any { tag -> tag.hit() } || it.prompt.hit())
                hits +=
                    (if (it.name.hit()) 0 else 1) to
                        SearchHit(
                            SearchKind.MISSION,
                            it.id,
                            it.name,
                            describeSchedule(it),
                            it.agentId,
                        )
        }
    state.agents.forEach {
        if (it.name.hit() || it.description.hit())
            hits +=
                (if (it.name.hit()) 0 else 1) to
                    SearchHit(
                        SearchKind.AGENT,
                        it.id,
                        it.name,
                        listOf(providerLabel(it.provider), it.description)
                            .filter { d -> d.isNotBlank() }
                            .joinToString(" · "),
                    )
    }
    state.projects.forEach {
        if (it.name.hit() || it.description.hit())
            hits +=
                (if (it.name.hit()) 0 else 1) to
                    SearchHit(
                        SearchKind.PROJECT,
                        it.id,
                        it.name,
                        it.description.ifBlank { it.path },
                    )
    }
    state.skills.forEach {
        if (it.name.hit() || it.description.hit())
            hits +=
                (if (it.name.hit()) 0 else 1) to
                    SearchHit(SearchKind.SKILL, "${it.scope}/${it.name}", it.name, it.description)
    }
    return hits.sortedBy { it.first }.map { it.second }
}

@Composable
fun SearchScreen(
    vm: LeoViewModel,
    state: Workspace,
    back: () -> Unit,
    openChat: (String) -> Unit,
    openRun: (String) -> Unit,
    openMission: () -> Unit,
    newChat: (String) -> Unit,
    open: (String) -> Unit,
) {
    val live = rememberLive(vm, state, "/chats/stream")
    var query by rememberSaveable { mutableStateOf("") }
    var scope by rememberSaveable { mutableStateOf<String?>(null) }
    val focus = remember { FocusRequester() }
    LaunchedEffect(Unit) { runCatching { focus.requestFocus() } }
    val hits =
        remember(query, live.state?.chats, state) {
            searchWorkspace(query, live.state?.chats.orEmpty(), state)
        }
    val shown = hits.filter { scope == null || it.kind.name == scope }
    val topMission = hits.firstOrNull { it.kind == SearchKind.MISSION }
    val topAgent = hits.firstOrNull { it.kind == SearchKind.AGENT }
    fun launch(taskId: String) = vm.perform {
        openRun(api.send<Run>("POST", "/tasks/${segment(taskId)}/run").id)
    }
    Column(Modifier.fillMaxSize()) {
        Row(
            Modifier.fillMaxWidth().padding(start = 16.dp, end = 8.dp, top = 8.dp, bottom = 4.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Row(
                Modifier.weight(1f)
                    .height(52.dp)
                    .clip(CircleShape)
                    .background(MaterialTheme.colorScheme.surface)
                    .border(1.5.dp, MaterialTheme.colorScheme.primary, CircleShape)
                    .padding(horizontal = 16.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Icon(
                    LeoIcons.Search,
                    null,
                    Modifier.size(20.dp),
                    tint = MaterialTheme.colorScheme.primary,
                )
                Spacer(Modifier.width(10.dp))
                BasicTextField(
                    query,
                    { query = it.take(200) },
                    Modifier.weight(1f).focusRequester(focus).testTag("search-field"),
                    singleLine = true,
                    textStyle =
                        MaterialTheme.typography.bodyLarge.copy(
                            color = MaterialTheme.colorScheme.onSurface
                        ),
                    cursorBrush = SolidColor(MaterialTheme.colorScheme.primary),
                    keyboardOptions = InputKeyboards.Search,
                    decorationBox = { inner ->
                        Box {
                            if (query.isEmpty())
                                Text(
                                    "Rechercher dans Leo",
                                    style = MaterialTheme.typography.bodyLarge,
                                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                                )
                            inner()
                        }
                    },
                )
                if (query.isNotEmpty())
                    androidx.compose.material3.IconButton(
                        onClick = { query = "" },
                        Modifier.offset(x = 12.dp),
                    ) {
                        Icon(
                            LeoIcons.Close,
                            "Effacer la recherche",
                            Modifier.size(18.dp),
                            tint = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
            }
            TextButton(onClick = back) { Text("Annuler") }
        }
        Row(
            Modifier.horizontalScroll(rememberScrollState()).padding(horizontal = 12.dp),
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            SignalChip("Tout", scope == null) { scope = null }
            SearchKind.entries.forEach { kind ->
                SignalChip(
                    kind.label,
                    scope == kind.name,
                    hits.count { it.kind == kind }.takeIf { query.isNotBlank() },
                ) {
                    scope = if (scope == kind.name) null else kind.name
                }
            }
        }
        LazyColumn(
            Modifier.weight(1f).imePadding(),
            contentPadding = PaddingValues(start = 16.dp, end = 16.dp, bottom = 24.dp),
        ) {
            if (query.isBlank())
                item {
                    Text(
                        "Conversations, missions, agents, projets et skills.",
                        Modifier.padding(horizontal = 4.dp, vertical = 16.dp),
                        style = MaterialTheme.typography.bodyMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            if (scope == null && (topMission != null || topAgent != null))
                item(key = "actions") {
                    Eyebrow("Actions", Modifier.padding(start = 4.dp, top = 16.dp, bottom = 8.dp))
                    SignalCard(Modifier.fillMaxWidth(), padding = PaddingValues(6.dp)) {
                        topMission?.let {
                            ActionRow(
                                LeoIcons.Play,
                                signal.ink,
                                signal.onInk,
                                "Lancer « ${it.title} »",
                                "Mission",
                                !state.busy,
                            ) {
                                launch(it.id)
                            }
                        }
                        topAgent?.let {
                            ActionRow(
                                LeoIcons.Plus,
                                MaterialTheme.colorScheme.primary,
                                MaterialTheme.colorScheme.onPrimary,
                                "Nouvelle conversation avec ${it.title}",
                                "Agent",
                            ) {
                                newChat(it.id)
                            }
                        }
                    }
                }
            if (query.isNotBlank() && shown.isEmpty())
                item {
                    Text(
                        "Aucun résultat pour « ${query.trim()} ».",
                        Modifier.padding(horizontal = 4.dp, vertical = 24.dp),
                        style = MaterialTheme.typography.bodyMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            SearchKind.entries.forEach { kind ->
                val group = shown.filter { it.kind == kind }
                if (group.isNotEmpty()) {
                    item(key = "group:${kind.name}") {
                        Eyebrow(
                            kind.label,
                            Modifier.padding(start = 4.dp, top = 18.dp, bottom = 4.dp),
                        )
                    }
                    items(group, key = { "${kind.name}:${it.id}" }) { hit ->
                        ResultRow(hit, query.trim()) {
                            when (hit.kind) {
                                SearchKind.CHAT -> openChat(hit.id)
                                SearchKind.MISSION -> openMission()
                                SearchKind.AGENT -> open("agents")
                                SearchKind.PROJECT -> open("projects")
                                SearchKind.SKILL -> open("skills")
                            }
                        }
                    }
                }
            }
        }
    }
}

@Composable
private fun ActionRow(
    icon: androidx.compose.ui.graphics.vector.ImageVector,
    container: Color,
    content: Color,
    title: String,
    detail: String,
    enabled: Boolean = true,
    onClick: () -> Unit,
) {
    Surface(
        onClick = onClick,
        enabled = enabled,
        shape = RoundedCornerShape(17.dp),
        color = Color.Transparent,
    ) {
        Row(
            Modifier.fillMaxWidth().heightIn(min = 56.dp).padding(10.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Box(
                Modifier.size(38.dp).clip(CircleShape).background(container),
                contentAlignment = Alignment.Center,
            ) {
                Icon(icon, null, Modifier.size(17.dp), tint = content)
            }
            Spacer(Modifier.width(12.dp))
            Column(Modifier.weight(1f)) {
                Text(
                    title,
                    style = MaterialTheme.typography.titleSmall,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
                Text(
                    detail,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
    }
}

internal fun highlight(text: String, query: String, style: SpanStyle): AnnotatedString =
    buildAnnotatedString {
        val start = if (query.isBlank()) -1 else text.indexOf(query, ignoreCase = true)
        if (start < 0) append(text)
        else {
            append(text.substring(0, start))
            withStyle(style) { append(text.substring(start, start + query.length)) }
            append(text.substring(start + query.length))
        }
    }

@Composable
private fun ResultRow(hit: SearchHit, query: String, onClick: () -> Unit) {
    val mark =
        SpanStyle(
            background = MaterialTheme.colorScheme.primaryContainer,
            color = MaterialTheme.colorScheme.onPrimaryContainer,
            fontWeight = FontWeight.Bold,
        )
    Surface(onClick = onClick, color = Color.Transparent, shape = RoundedCornerShape(14.dp)) {
        Row(
            Modifier.fillMaxWidth()
                .heightIn(min = 64.dp)
                .padding(horizontal = 4.dp, vertical = 8.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            when (hit.kind) {
                SearchKind.CHAT,
                SearchKind.AGENT -> AgentAvatar(hit.avatar, hit.key, 40.dp)
                else ->
                    Box(
                        Modifier.size(40.dp)
                            .clip(RoundedCornerShape(13.dp))
                            .background(MaterialTheme.colorScheme.surfaceVariant),
                        contentAlignment = Alignment.Center,
                    ) {
                        Icon(
                            when (hit.kind) {
                                SearchKind.MISSION -> LeoIcons.Orbit
                                SearchKind.PROJECT -> LeoIcons.Folder
                                else -> LeoIcons.Spark
                            },
                            null,
                            Modifier.size(20.dp),
                            tint = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
            }
            Spacer(Modifier.width(12.dp))
            Column(Modifier.weight(1f)) {
                Text(
                    highlight(hit.title, query, mark),
                    style = MaterialTheme.typography.titleSmall,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
                if (hit.detail.isNotBlank())
                    Text(
                        highlight(hit.detail, query, mark),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
            }
        }
    }
}

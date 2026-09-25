@file:OptIn(androidx.compose.material3.ExperimentalMaterial3Api::class)

package dev.leo.manager.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.luminance
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.compose.currentStateAsState
import dev.leo.manager.data.*
import kotlinx.serialization.json.encodeToJsonElement

private val filters =
    listOf(
        "all" to "Toutes",
        "scheduled" to "Planifiées",
        "once" to "Ponctuelles",
        "paused" to "En pause",
        "archived" to "Archivées",
    )

internal fun missionFilter(task: Task, filter: String): Boolean =
    (if (filter == "archived") task.archived else !task.archived) &&
        when (filter) {
            "scheduled" -> task.cron != null && task.enabled
            "paused" -> !task.enabled
            "once" -> task.cron == null
            else -> true
        }

/** Running first, then what needs review, then the next scheduled ones, then by name. */
internal fun missionOrder(tasks: List<Task>, latest: Map<String, Run>): List<Task> =
    tasks.sortedWith(
        compareBy<Task> {
                when (taskGroup(it, latest[it.id])) {
                    "En cours" -> 0
                    "À examiner" -> 1
                    else -> 2
                }
            }
            .thenBy { if (it.enabled && it.nextRun != null) it.nextRun else Long.MAX_VALUE }
            .thenBy { it.name.lowercase() }
    )

/** Missions: the scheduled and one-off tasks with their history, merged in one place. */
@Composable
fun MissionsScreen(vm: LeoViewModel, state: Workspace, openRun: (String) -> Unit) {
    var query by rememberSaveable { mutableStateOf("") }
    var searching by rememberSaveable { mutableStateOf(false) }
    var filter by rememberSaveable { mutableStateOf("all") }
    var selectedId by rememberSaveable { mutableStateOf<String?>(null) }
    var editing by rememberSaveable { mutableStateOf<String?>(null) }
    var deleting by rememberSaveable { mutableStateOf<String?>(null) }
    var activity by remember { mutableStateOf<List<Run>>(emptyList()) }
    var recent by remember { mutableStateOf<List<Run>>(emptyList()) }
    var loading by remember { mutableStateOf(state.session.authenticated) }
    if (state.session.authenticated)
        Poll("missions", 5000) {
            try {
                activity = vm.api.get("/tasks/activity")
                recent = vm.api.get("/runs?limit=100")
                vm.refresh()
            } catch (e: Exception) {
                vm.report(e)
            } finally {
                loading = false
            }
        }
    val latest = activity.associateBy { it.taskId }
    val history = remember(recent) { recent.filter { it.trigger != "chat" }.groupBy { it.taskId } }
    val visible =
        missionOrder(
            state.tasks.filter {
                missionFilter(it, filter) &&
                    "${it.name} ${it.tags.joinToString(" ")}".contains(query.trim(), true)
            },
            latest,
        )
    val selected = state.tasks.find { it.id == selectedId }
    val lifecycle by LocalLifecycleOwner.current.lifecycle.currentStateAsState()
    fun launch(task: Task) {
        vm.perform {
            val started = api.send<Run>("POST", "/tasks/${segment(task.id)}/run")
            activity = listOf(started) + activity.filter { it.taskId != task.id }
            selectedId = null
            openRun(started.id)
        }
    }
    fun create() {
        vm.clearMessage()
        editing = ""
    }
    val actions =
        MissionActions(
            launch = ::launch,
            edit = { editing = it.id },
            delete = {
                selectedId = null
                deleting = it.id
            },
            openRun = openRun,
            close = { selectedId = null },
        )
    BoxWithConstraints(Modifier.fillMaxSize()) {
        val wide = maxWidth >= 840.dp
        Row(Modifier.fillMaxSize()) {
            Column(if (wide) Modifier.width(420.dp).fillMaxHeight() else Modifier.fillMaxSize()) {
                Column(Modifier.padding(start = 20.dp, end = 16.dp, top = 12.dp)) {
                    val active = state.tasks.count { !it.archived && it.enabled }
                    val next =
                        state.tasks
                            .filter { it.enabled && !it.archived && it.nextRun != null }
                            .minOfOrNull { it.nextRun!! }
                    ScreenTitle(
                        "Missions",
                        listOfNotNull(
                                if (active == 1) "1 active" else "$active actives",
                                next?.let {
                                    "prochaine : ${upcomingStamp(it).replaceFirstChar { c -> c.lowercase() }}"
                                },
                            )
                            .joinToString(" · "),
                    ) {
                        Row {
                            RoundAction("Rechercher une mission", LeoIcons.Search) {
                                searching = !searching
                            }
                            RoundAction(
                                "Créer une mission",
                                LeoIcons.Plus,
                                container = signal.ink,
                                content = signal.onInk,
                                outlined = false,
                                enabled = state.agents.isNotEmpty(),
                                onClick = ::create,
                            )
                        }
                    }
                    if (searching || query.isNotBlank())
                        Box(Modifier.padding(top = 12.dp)) {
                            SearchField("Rechercher une mission", query) { query = it }
                        }
                }
                Row(
                    Modifier.horizontalScroll(rememberScrollState())
                        .padding(horizontal = 16.dp, vertical = 6.dp),
                    horizontalArrangement = Arrangement.spacedBy(8.dp),
                ) {
                    filters.forEach { (key, label) ->
                        SignalChip(
                            label,
                            filter == key,
                            state.tasks.count { missionFilter(it, key) },
                        ) {
                            filter = key
                        }
                    }
                }
                if (loading) LinearProgressIndicator(Modifier.fillMaxWidth())
                LazyColumn(
                    Modifier.weight(1f).testTag("missions"),
                    contentPadding =
                        PaddingValues(start = 16.dp, end = 16.dp, top = 8.dp, bottom = 24.dp),
                    verticalArrangement = Arrangement.spacedBy(10.dp),
                ) {
                    if (visible.isEmpty())
                        item {
                            Empty(
                                if (state.tasks.isEmpty()) "Aucune mission"
                                else "Aucune mission dans cette vue",
                                if (state.tasks.isEmpty())
                                    "Créez une mission et confiez-la à un agent."
                                else "Modifiez la recherche ou les filtres.",
                            )
                        }
                    items(visible, key = { it.id }) { task ->
                        MissionCard(
                            task,
                            state,
                            latest[task.id],
                            history[task.id].orEmpty(),
                            task.id == selected?.id && wide,
                            open = { selectedId = task.id },
                            launch = { launch(task) },
                        )
                    }
                }
            }
            if (wide) {
                VerticalDivider(color = MaterialTheme.colorScheme.outlineVariant)
                Box(Modifier.weight(1f).fillMaxHeight()) {
                    val shown = selected ?: visible.firstOrNull()
                    if (shown != null)
                        key(shown.id) {
                            Column(
                                Modifier.fillMaxSize()
                                    .verticalScroll(rememberScrollState())
                                    .padding(24.dp)
                            ) {
                                MissionDetail(
                                    vm,
                                    state,
                                    shown,
                                    latest[shown.id],
                                    actions,
                                    sheet = false,
                                )
                            }
                        }
                    else
                        Box(
                            Modifier.fillMaxSize().padding(24.dp),
                            contentAlignment = Alignment.Center,
                        ) {
                            Empty(
                                "Vos missions, au même endroit",
                                "Créez une mission pour démarrer.",
                            )
                        }
                }
            }
        }
        // The sheet is its own window, above every screen. Composed while a back gesture previews
        // Missions under a run opened from it, it took the gesture and the run stayed open, so it
        // only shows once Missions is the settled destination.
        if (!wide && selected != null && lifecycle.isAtLeast(Lifecycle.State.RESUMED))
            ModalBottomSheet(
                onDismissRequest = { selectedId = null },
                sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true),
                containerColor = MaterialTheme.colorScheme.background,
            ) {
                Column(
                    Modifier.fillMaxWidth()
                        .testTag("mission-sheet")
                        .verticalScroll(rememberScrollState())
                        .padding(horizontal = 20.dp)
                        .padding(bottom = 24.dp)
                ) {
                    MissionDetail(vm, state, selected, latest[selected.id], actions, sheet = true)
                }
            }
    }
    editing?.let { id ->
        TaskEditor(
            vm,
            state,
            state.tasks.find { it.id == id }
                ?: Task(
                    agentId =
                        state.agents.firstOrNull { it.id == MAIN_AGENT_ID }?.id
                            ?: state.agents.firstOrNull()?.id.orEmpty(),
                    timezone = java.time.ZoneId.systemDefault().id,
                ),
            onSaved = { task ->
                query = ""
                filter = if (task.archived) "archived" else "all"
                selectedId = task.id
            },
        ) {
            editing = null
        }
    }
    deleting?.let { id ->
        Confirm(
            "Supprimer cette mission ?",
            "La planification sera supprimée. Les exécutions restent dans l’historique.",
            state.busy,
            state.error,
            { deleting = null },
        ) {
            vm.perform {
                delete("/tasks/${segment(id)}")
                deleting = null
            }
        }
    }
}

private class MissionActions(
    val launch: (Task) -> Unit,
    val edit: (Task) -> Unit,
    val delete: (Task) -> Unit,
    val openRun: (String) -> Unit,
    val close: () -> Unit,
)

@Composable
private fun MissionCard(
    task: Task,
    state: Workspace,
    run: Run?,
    runs: List<Run>,
    highlighted: Boolean,
    open: () -> Unit,
    launch: () -> Unit,
) {
    val agent = state.agents.find { it.id == task.agentId }
    val project = state.projects.find { it.id == task.projectId }
    val quiet = task.archived || !task.enabled && task.cron != null
    SignalCard(
        Modifier.fillMaxWidth(),
        onClick = open,
        color =
            if (highlighted) MaterialTheme.colorScheme.primaryContainer
            else if (quiet) MaterialTheme.colorScheme.background
            else MaterialTheme.colorScheme.surface,
        outlined = quiet || MaterialTheme.colorScheme.background.luminance() < 0.2f,
        padding = PaddingValues(start = 16.dp, end = 12.dp, top = 14.dp, bottom = 14.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Column(Modifier.weight(1f)) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    AgentAvatar(agent?.name ?: "?", task.agentId, 22.dp)
                    Spacer(Modifier.width(8.dp))
                    ProjectLabel(project?.name ?: "Tous les projets", project?.id)
                }
                Text(
                    task.name,
                    Modifier.padding(top = 8.dp),
                    style = MaterialTheme.typography.titleMedium,
                    color =
                        if (quiet) MaterialTheme.colorScheme.onSurfaceVariant
                        else MaterialTheme.colorScheme.onSurface,
                    maxLines = 2,
                    overflow = TextOverflow.Ellipsis,
                )
                Row(Modifier.padding(top = 4.dp), verticalAlignment = Alignment.CenterVertically) {
                    Icon(
                        if (quiet) LeoIcons.Pause else LeoIcons.Clock,
                        null,
                        Modifier.size(14.dp),
                        tint = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                    Spacer(Modifier.width(5.dp))
                    Text(
                        describeSchedule(task),
                        Modifier.weight(1f, fill = false),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                    task.nextRun
                        ?.takeIf { task.enabled && !task.archived && task.cron != null }
                        ?.let {
                            Text(
                                "  ·  ${upcomingStamp(it).substringBefore(" · ")}",
                                style = MaterialTheme.typography.bodySmall,
                                fontWeight = FontWeight.SemiBold,
                                maxLines = 1,
                            )
                        }
                }
                val group = taskGroup(task, run)
                if (group == "À examiner" && run != null)
                    Text(
                        run.outcome?.let(::outcomeLabel) ?: statusLabel(run.status),
                        Modifier.padding(top = 6.dp),
                        style = MaterialTheme.typography.labelMedium,
                        fontWeight = FontWeight.SemiBold,
                        color = signal.attention,
                    )
                if (runs.isNotEmpty())
                    RunStrip(runs.take(12).reversed(), Modifier.padding(top = 10.dp))
            }
            Spacer(Modifier.width(10.dp))
            when {
                run?.active == true ->
                    Box(
                        Modifier.size(52.dp)
                            .clip(CircleShape)
                            .background(MaterialTheme.colorScheme.primary)
                            .semantics { contentDescription = "En cours" },
                        contentAlignment = Alignment.Center,
                    ) {
                        Text(
                            if (run.status == "queued") "…" else elapsed(run.startedAt),
                            style = MaterialTheme.typography.labelMedium,
                            fontWeight = FontWeight.Bold,
                            color = MaterialTheme.colorScheme.onPrimary,
                            maxLines = 1,
                        )
                    }
                else ->
                    RoundAction(
                        "Lancer ${task.name}",
                        LeoIcons.Play,
                        container = if (quiet) MaterialTheme.colorScheme.background else signal.ink,
                        content =
                            if (quiet) MaterialTheme.colorScheme.onSurfaceVariant else signal.onInk,
                        outlined = quiet,
                        size = 52.dp,
                        enabled = !state.busy && !task.archived,
                        onClick = launch,
                    )
            }
        }
    }
}

/** Last runs, oldest first: success, failure, running or other. */
@Composable
internal fun RunStrip(runs: List<Run>, modifier: Modifier = Modifier) {
    val label =
        "${runs.count { it.status == "succeeded" }} réussies sur ${runs.size} dernières exécutions"
    Row(
        modifier.semantics { contentDescription = label },
        horizontalArrangement = Arrangement.spacedBy(3.dp),
    ) {
        runs.forEach {
            Box(
                Modifier.size(8.dp, 13.dp)
                    .clip(RoundedCornerShape(3.dp))
                    .background(
                        when (it.status) {
                            "succeeded" -> signal.success
                            "failed",
                            "interrupted" -> signal.attention
                            "running",
                            "queued" -> MaterialTheme.colorScheme.primary
                            else -> MaterialTheme.colorScheme.outlineVariant
                        }
                    )
            )
        }
    }
}

@Composable
private fun MissionDetail(
    vm: LeoViewModel,
    state: Workspace,
    task: Task,
    run: Run?,
    actions: MissionActions,
    sheet: Boolean,
) {
    var history by remember(task.id) { mutableStateOf<List<Run>?>(null) }
    var occurrences by
        remember(task.id, task.cron, task.timezone, task.enabled) {
            mutableStateOf<List<Long>>(emptyList())
        }
    var menu by remember { mutableStateOf(false) }
    var promptExpanded by rememberSaveable(task.id) { mutableStateOf(false) }
    val connected = state.session.authenticated
    if (connected)
        Poll("mission-history:${task.id}", 10000) {
            try {
                history = vm.api.get("/runs?taskId=${segment(task.id)}&limit=10")
            } catch (e: Exception) {
                vm.report(e)
            }
        }
    LaunchedEffect(task.id, task.cron, task.timezone, task.enabled, connected) {
        val cron = task.cron
        if (connected && cron != null && task.enabled && !task.archived)
            occurrences =
                try {
                    vm.api
                        .send<Occurrences>(
                            "POST",
                            "/schedule/preview",
                            body("cron" to cron, "timezone" to task.timezone),
                        )
                        .occurrences
                        .take(2)
                } catch (e: Exception) {
                    if (e is kotlinx.coroutines.CancellationException) throw e
                    emptyList()
                }
    }
    val agent = state.agents.find { it.id == task.agentId }
    val project = state.projects.find { it.id == task.projectId }
    fun toggle(copy: Task) = vm.perform {
        save("tasks", task.id, wireJson.encodeToJsonElement(copy))
    }
    Row(verticalAlignment = Alignment.CenterVertically) {
        AgentAvatar(agent?.name ?: "?", task.agentId, 30.dp)
        Spacer(Modifier.width(10.dp))
        Column(Modifier.weight(1f)) {
            Text(
                agent?.name ?: "Agent supprimé",
                style = MaterialTheme.typography.labelLarge,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                maxLines = 1,
            )
            ProjectLabel(project?.name ?: "Tous les projets autorisés", project?.id)
        }
        Box {
            RoundAction(
                "Autres actions",
                LeoIcons.More,
                container = MaterialTheme.colorScheme.background,
                outlined = false,
            ) {
                menu = true
            }
            DropdownMenu(menu, { menu = false }) {
                DropdownMenuItem(
                    text = { Text("Dupliquer (en pause)") },
                    enabled = !state.busy,
                    onClick = {
                        menu = false
                        vm.perform {
                            save(
                                "tasks",
                                "",
                                wireJson.encodeToJsonElement(
                                    task.copy(
                                        id = "",
                                        name = task.name.take(92) + " (copie)",
                                        enabled = false,
                                        archived = false,
                                    )
                                ),
                            )
                        }
                    },
                )
                DropdownMenuItem(
                    text = { Text(if (task.archived) "Restaurer (en pause)" else "Archiver") },
                    enabled = !state.busy,
                    onClick = {
                        menu = false
                        toggle(task.copy(archived = !task.archived, enabled = false))
                    },
                )
                if (run != null)
                    DropdownMenuItem(
                        text = { Text("Ouvrir la dernière exécution") },
                        onClick = {
                            menu = false
                            actions.openRun(run.id)
                        },
                    )
                DropdownMenuItem(
                    text = { Text("Supprimer", color = MaterialTheme.colorScheme.error) },
                    onClick = {
                        menu = false
                        actions.delete(task)
                    },
                )
            }
        }
        if (sheet)
            RoundAction(
                "Fermer",
                LeoIcons.Close,
                container = MaterialTheme.colorScheme.background,
                outlined = false,
            ) {
                actions.close()
            }
    }
    Text(task.name, Modifier.padding(top = 12.dp), style = MaterialTheme.typography.headlineMedium)
    if (task.tags.isNotEmpty())
        Text(
            task.tags.joinToString(" · "),
            Modifier.padding(top = 4.dp),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    SignalCard(Modifier.fillMaxWidth().padding(top = 16.dp)) {
        val cron = task.cron
        val wording = describeSchedule(task)
        val time = Regex("\\d{2}:\\d{2}$").find(wording)?.value
        Row(verticalAlignment = Alignment.CenterVertically) {
            Column(Modifier.weight(1f)) {
                Eyebrow(
                    if (time != null)
                        wording.substringBefore(" · $time").removePrefix("En pause · ")
                    else "Planification"
                )
                Text(
                    time ?: wording,
                    style =
                        if (time != null) MaterialTheme.typography.headlineLarge
                        else MaterialTheme.typography.titleMedium,
                )
                if (cron != null)
                    Text(
                        "${cron} · ${task.timezone}",
                        style = MaterialTheme.typography.labelSmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
            }
            if (occurrences.isNotEmpty())
                Column(horizontalAlignment = Alignment.End) {
                    Text(
                        "Prochaines",
                        style = MaterialTheme.typography.labelMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                    Row(
                        Modifier.padding(top = 6.dp),
                        horizontalArrangement = Arrangement.spacedBy(6.dp),
                    ) {
                        occurrences.forEachIndexed { index, value ->
                            Surface(
                                color =
                                    if (index == 0) signal.ink
                                    else MaterialTheme.colorScheme.surfaceVariant,
                                shape = CircleShape,
                            ) {
                                Text(
                                    upcomingStamp(value).substringBefore(" · "),
                                    Modifier.padding(horizontal = 10.dp, vertical = 4.dp),
                                    style = MaterialTheme.typography.labelMedium,
                                    fontWeight = FontWeight.SemiBold,
                                    color =
                                        if (index == 0) signal.onInk
                                        else MaterialTheme.colorScheme.onSurface,
                                )
                            }
                        }
                    }
                }
        }
    }
    Surface(
        onClick = { promptExpanded = !promptExpanded },
        modifier = Modifier.fillMaxWidth().padding(top = 12.dp),
        shape = RoundedCornerShape(22.dp),
        color = MaterialTheme.colorScheme.surfaceVariant,
    ) {
        Column(Modifier.padding(16.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Eyebrow("Mission", Modifier.weight(1f))
                Icon(
                    if (promptExpanded) LeoIcons.Down else LeoIcons.Right,
                    if (promptExpanded) "Réduire la mission" else "Afficher toute la mission",
                    Modifier.size(16.dp),
                    tint = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            Spacer(Modifier.height(6.dp))
            if (promptExpanded) Markdown(task.prompt)
            else
                Text(
                    task.prompt,
                    style = MaterialTheme.typography.bodyMedium,
                    maxLines = 3,
                    overflow = TextOverflow.Ellipsis,
                )
        }
    }
    val runs = history
    Row(
        Modifier.padding(top = 20.dp, bottom = 4.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Eyebrow("Historique", Modifier.weight(1f))
        val finished = runs.orEmpty().filter { !it.active }
        if (finished.isNotEmpty()) {
            val rate = finished.count { it.status == "succeeded" } * 100 / finished.size
            Text(
                "$rate % de réussite",
                style = MaterialTheme.typography.labelLarge,
                fontWeight = FontWeight.SemiBold,
                color = if (rate >= 50) signal.success else signal.attention,
            )
        }
    }
    if (runs == null) {
        if (connected) LinearProgressIndicator(Modifier.fillMaxWidth().padding(vertical = 8.dp))
    } else if (runs.isEmpty())
        Text(
            "Aucune exécution pour le moment.",
            Modifier.padding(vertical = 8.dp),
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    else runs.forEach { item -> HistoryRow(item) { actions.openRun(item.id) } }
    Row(
        Modifier.fillMaxWidth().padding(top = 16.dp),
        horizontalArrangement = Arrangement.spacedBy(10.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        RoundAction("Modifier la mission", LeoIcons.Pencil, size = 52.dp, enabled = !state.busy) {
            actions.edit(task)
        }
        if (!task.archived && task.cron != null)
            RoundAction(
                if (task.enabled) "Mettre en pause" else "Reprendre la planification",
                if (task.enabled) LeoIcons.Pause else LeoIcons.Clock,
                size = 52.dp,
                enabled = !state.busy,
            ) {
                toggle(task.copy(enabled = !task.enabled))
            }
        SignalButton(
            "Lancer maintenant",
            Modifier.weight(1f),
            expand = true,
            icon = LeoIcons.Play,
            height = 52.dp,
            enabled = !state.busy && !task.archived,
        ) {
            actions.launch(task)
        }
    }
    if (sheet) Spacer(Modifier.height(8.dp))
}

@Composable
private fun HistoryRow(run: Run, open: () -> Unit) {
    Surface(
        onClick = open,
        color = androidx.compose.ui.graphics.Color.Transparent,
        shape = RoundedCornerShape(12.dp),
    ) {
        Row(
            Modifier.fillMaxWidth().heightIn(min = 56.dp).padding(vertical = 6.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            RunStatusTile(run.status, 30.dp)
            Spacer(Modifier.width(12.dp))
            Column(Modifier.weight(1f)) {
                Text(
                    date(run.startedAt ?: run.createdAt),
                    style = MaterialTheme.typography.titleSmall,
                )
                Text(
                    run.outcome?.reason?.lineSequence()?.firstOrNull()?.ifBlank { null }
                        ?: run.summary.lineSequence().firstOrNull()?.ifBlank { null }
                        ?: statusLabel(run.status),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
            }
            Text(
                if (run.active) elapsed(run.startedAt) else duration(run),
                style = MaterialTheme.typography.labelMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            Icon(
                LeoIcons.Right,
                null,
                Modifier.size(16.dp),
                tint = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
}

@file:OptIn(androidx.compose.material3.ExperimentalMaterial3Api::class)

package build.cairn.app.ui

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.rememberScrollState
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import build.cairn.app.data.*

@Composable
fun RunsScreen(vm: CairnViewModel, state: Workspace, open: (String) -> Unit) {
    var runs by remember { mutableStateOf<List<Run>>(emptyList()) }
    var status by rememberSaveable { mutableStateOf("") }
    var offset by rememberSaveable { mutableIntStateOf(0) }
    var taskId by rememberSaveable { mutableStateOf("") }
    var loading by remember { mutableStateOf(true) }
    var filters by rememberSaveable { mutableStateOf(false) }
    Poll("$status/$offset/$taskId", 10000) {
        try {
            runs =
                vm.api.get(
                    "/runs?limit=30&offset=$offset" +
                        (if (status.isEmpty()) "" else "&status=${segment(status)}") +
                        if (taskId.isEmpty()) "" else "&taskId=${segment(taskId)}"
                )
        } catch (e: Exception) {
            vm.report(e)
        } finally {
            loading = false
        }
    }
    LazyColumn(
        Modifier.fillMaxSize(),
        contentPadding = PaddingValues(20.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        item {
            ScreenTitle("Journal", "Toutes les exécutions, des missions comme des conversations.") {
                RoundAction("Filtrer par mission", CairnIcons.Tune) { filters = !filters }
            }
        }
        item {
            Row(
                Modifier.horizontalScroll(rememberScrollState()),
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                listOf(
                        "" to "Toutes",
                        "running" to "En cours",
                        "queued" to "En attente",
                        "succeeded" to "Terminées",
                        "failed" to "Échecs",
                        "cancelled" to "Annulées",
                        "interrupted" to "Interrompues",
                    )
                    .forEach { (key, label) ->
                        SignalChip(label, status == key) {
                            if (status != key) {
                                status = key
                                offset = 0
                                runs = emptyList()
                                loading = true
                            }
                        }
                    }
            }
        }
        if (filters || taskId.isNotBlank())
            item {
                Choice(
                    "Mission",
                    taskId,
                    listOf("" to "Toutes les missions") + state.tasks.map { it.id to it.name },
                ) {
                    taskId = it
                    offset = 0
                    runs = emptyList()
                    loading = true
                }
            }
        if (loading) item { LinearProgressIndicator(Modifier.fillMaxWidth()) }
        if (runs.isEmpty() && !loading)
            item {
                Empty(
                    "Aucune exécution",
                    "Les résultats correspondant à ces filtres apparaîtront ici.",
                )
            }
        items(runs, key = { it.id }) { RunCard(it, open) }
        item {
            Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween) {
                OutlinedButton(
                    onClick = {
                        offset = (offset - 30).coerceAtLeast(0)
                        loading = true
                    },
                    enabled = offset > 0 && !loading,
                ) {
                    Text("Précédent")
                }
                OutlinedButton(
                    onClick = {
                        offset += 30
                        loading = true
                    },
                    enabled = runs.size == 30 && !loading,
                ) {
                    Text("Suivant")
                }
            }
        }
    }
}

@Composable
fun RunScreen(
    vm: CairnViewModel,
    state: Workspace,
    id: String,
    openChat: (String) -> Unit = {},
    back: () -> Unit = {},
    openRun: (String) -> Unit,
) {
    val live = rememberLive(vm, state, "/runs/${segment(id)}/stream")
    val run = live.state?.run
    val events = live.events
    val more = live.catchingUp
    val timelineKeys = remember(id) { TimelineKeys() }
    val timeline =
        remember(events, live.state?.artifacts) {
            deliveryTimeline(
                timelineKeys.stabilize(live.history, timelineEntries(events)),
                live.state?.artifacts.orEmpty(),
            )
        }
    var menu by remember { mutableStateOf(false) }
    var details by rememberSaveable(id) { mutableStateOf(false) }
    var fullscreen by rememberSaveable(id) { mutableStateOf(false) }
    BackHandler(fullscreen) { fullscreen = false }
    val focusMode = LocalFocusMode.current
    DisposableEffect(fullscreen) {
        focusMode(fullscreen)
        onDispose { focusMode(false) }
    }
    var tab by rememberSaveable(id) { mutableIntStateOf(1) }
    var autoTab by rememberSaveable(id) { mutableStateOf(true) }
    var follow by rememberSaveable(id) { mutableStateOf(true) }
    var confirm by rememberSaveable(id) { mutableStateOf<String?>(null) }
    val logState = rememberLazyListState()
    val rendering = remember(id) { MarkdownRendering() }
    val positionReady =
        rememberHistoryPosition(
            vm,
            state,
            "/runs/${segment(id)}/stream",
            live,
            logState,
            tab == 1,
            follow,
            rendering,
        ) {
            follow = it
        }
    val loadOlder = rememberHistoryPaging(live, logState, positionReady && tab == 1)
    LaunchedEffect(run?.id, live.synced) {
        if (autoTab && run != null && live.synced) {
            autoTab = false
            if (run.active) tab = 1
        }
    }
    val followGesture = rememberHistoryFollowGesture(logState, follow) { follow = it }
    FollowHistoryTail(
        logState,
        positionReady && follow && tab == 1 && !more,
        live.cursor,
        rendering,
        followGesture,
    )
    ArtifactLinkHost(vm, live.state?.artifacts.orEmpty()) {
        Column(Modifier.fillMaxSize()) {
            live.error?.let { ErrorNotice(it) {} }
            run?.let { current ->
                val headerActions: @Composable RowScope.() -> Unit = {
                    if (current.active)
                        ActionIcon("Arrêter", CairnIcons.Stop, !state.busy) { confirm = "cancel" }
                    Box {
                        ActionIcon("Options de l’exécution", CairnIcons.More) { menu = true }
                        DropdownMenu(menu, { menu = false }) {
                            DropdownMenuItem(
                                text = { Text("Plein écran") },
                                onClick = {
                                    menu = false
                                    fullscreen = true
                                },
                            )
                            DropdownMenuItem(
                                text = { Text("Mission et détails") },
                                leadingIcon = { Icon(Icons.Default.Info, null) },
                                onClick = {
                                    menu = false
                                    autoTab = false
                                    details = true
                                },
                            )
                            if (current.resumeAvailable && current.trigger != "chat")
                                DropdownMenuItem(
                                    text = { Text("Reprendre") },
                                    enabled = !state.busy,
                                    onClick = {
                                        menu = false
                                        vm.perform { api.request("POST", "/runs/$id/resume") }
                                    },
                                )
                            if (!current.active && current.trigger != "chat")
                                DropdownMenuItem(
                                    text = { Text("Relancer") },
                                    enabled = !state.busy,
                                    onClick = {
                                        menu = false
                                        vm.perform {
                                            openRun(api.send<Run>("POST", "/runs/$id/retry").id)
                                        }
                                    },
                                )
                            if (current.trigger == "chat")
                                DropdownMenuItem(
                                    text = { Text("Ouvrir le chat") },
                                    leadingIcon = { Icon(CairnIcons.Chat, null) },
                                    onClick = {
                                        menu = false
                                        openChat(current.taskId)
                                    },
                                )
                        }
                    }
                }
                if (!fullscreen)
                    DetailHeader(
                        current.title,
                        "${statusLabel(current.status)} · ${duration(current)}",
                        back,
                        headerActions,
                    )
                else
                    Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
                        Text(
                            when (tab) {
                                0 -> "Résultat"
                                2 -> "Fichiers"
                                else -> "Conversation"
                            },
                            Modifier.weight(1f).padding(start = 16.dp),
                            style = MaterialTheme.typography.titleSmall,
                        )
                        ActionIcon("Quitter le plein écran", Icons.Default.Close) {
                            fullscreen = false
                        }
                    }
                current.error
                    ?.takeIf { it.isNotBlank() }
                    ?.let {
                        Text(
                            it,
                            Modifier.padding(horizontal = 16.dp),
                            color = MaterialTheme.colorScheme.error,
                        )
                    }
                if (current.status in listOf("failed", "interrupted", "cancelled"))
                    Text(
                        statusLabel(current.status),
                        Modifier.padding(horizontal = 16.dp),
                        color = MaterialTheme.colorScheme.error,
                        style = MaterialTheme.typography.labelMedium,
                    )
                current.accountWaitReason?.let {
                    Text(
                        "En attente : $it",
                        Modifier.padding(horizontal = 20.dp),
                        style = MaterialTheme.typography.bodySmall,
                    )
                }
                if (current.recoveryPending)
                    Text(
                        "Récupération en cours…",
                        Modifier.padding(horizontal = 20.dp),
                        style = MaterialTheme.typography.bodySmall,
                    )
                if (current.cancelRequestedAt != null && current.active)
                    Text(
                        "Arrêt demandé…",
                        Modifier.padding(horizontal = 20.dp),
                        style = MaterialTheme.typography.bodySmall,
                    )
                if (!fullscreen)
                    SecondaryTabRow(
                        selectedTabIndex = listOf(1, 0, 2).indexOf(tab),
                        containerColor = MaterialTheme.colorScheme.background,
                    ) {
                        listOf(1 to "Conversation", 0 to "Résultat", 2 to "Fichiers").forEach {
                            (index, label) ->
                            Tab(
                                selected = tab == index,
                                onClick = {
                                    autoTab = false
                                    tab = index
                                },
                                text = {
                                    Text(
                                        if (index == 2 && live.state.artifacts.isNotEmpty())
                                            "$label · ${live.state.artifacts.size}"
                                        else label,
                                        maxLines = 1,
                                    )
                                },
                            )
                        }
                    }
                when (tab) {
                    0 ->
                        Page {
                            if (current.summary.isBlank())
                                Empty(
                                    if (current.active) "Le travail est en cours"
                                    else "Aucun résumé",
                                    "Consultez l’activité pour les détails.",
                                )
                            else {
                                Markdown(current.summary)
                                ShareButton(current.summary)
                            }
                            current.outcome
                                ?.takeIf { current.status == "succeeded" }
                                ?.let { CompletionEvidence(it, current.snapshot.agent.name) }
                            if (live.state.artifacts.isNotEmpty())
                                ArtifactStrip(vm, latestArtifacts(live.state.artifacts))
                        }
                    1 -> {
                        if (more) LinearProgressIndicator(Modifier.fillMaxWidth())
                        Box(Modifier.weight(1f).fillMaxWidth()) {
                            LazyColumn(
                                Modifier.fillMaxSize().historyFollowGesture(followGesture),
                                state = logState,
                                overscrollEffect = rememberHistoryOverscroll(),
                                contentPadding = PaddingValues(16.dp),
                                verticalArrangement = Arrangement.spacedBy(12.dp, Alignment.Bottom),
                            ) {
                                items(timeline, key = { it.key }) { entry ->
                                    TimelineRow(
                                        vm,
                                        entry,
                                        current.snapshot.agent.name.ifBlank { "Cairn" },
                                        rendering,
                                        hideRunning = current.status == "running",
                                    )
                                }
                                if (!current.active)
                                    current.outcome
                                        ?.takeIf { current.status == "succeeded" }
                                        ?.let { outcome ->
                                            item(key = "outcome:${outcome.reportedAt}") {
                                                CompletionEvidence(
                                                    outcome,
                                                    current.snapshot.agent.name,
                                                )
                                            }
                                        }
                                if (current.active)
                                    item(key = "agent-working") {
                                        WorkingIndicator(
                                            remember(events, current.status, current.startedAt) {
                                                if (current.status == "queued")
                                                    WorkingStep("En attente de l’agent", "", null)
                                                else
                                                    workingStep(
                                                        events,
                                                        current.snapshot.agent.name,
                                                        current.startedAt,
                                                    )
                                            }
                                        )
                                    }
                                if (events.isEmpty()) item { Text("L’activité apparaîtra ici.") }
                            }
                            HistoryStatus(
                                live,
                                logState,
                                loadOlder,
                                live.status.takeIf { it != "En direct" },
                            )
                            HistoryBottomButton(logState, follow, "Dernière activité") {
                                follow = true
                            }
                        }
                    }
                    2 -> Page { ArtifactsPanel(vm, live.state.artifacts) }
                }
                if (details)
                    DetailSheet("Mission et détails", { details = false }) {
                        Text(
                            current.snapshot.agent.name,
                            style = MaterialTheme.typography.titleMedium,
                        )
                        Text(
                            "${current.snapshot.project?.name ?: current.snapshot.projects.joinToString { it.name }.ifBlank { "Projets autorisés" }} · ${current.snapshot.agent.model.ifBlank { "Modèle par défaut" }}",
                            style = MaterialTheme.typography.bodySmall,
                        )
                        current.accountName?.let {
                            Text("Compte : $it", style = MaterialTheme.typography.bodySmall)
                        }
                        Heading("Instructions d’origine")
                        Markdown(current.snapshot.task.prompt)
                        Panel {
                            Text("Espace de travail", style = MaterialTheme.typography.titleMedium)
                            current.workspaces.forEach { workspace ->
                                Text(
                                    current.snapshot.projects
                                        .find { it.id == workspace.projectId }
                                        ?.name ?: workspace.projectId
                                )
                                Code("${workspace.kind} · ${workspace.path}")
                                workspace.revision?.let { Code("Commit de départ : $it") }
                            }
                            Code(
                                current.workspace
                                    ?: if (current.workspaceCleanedAt != null) "Worktree nettoyé"
                                    else "Pas encore préparé"
                            )
                            if (
                                !current.active &&
                                    current.workspace != null &&
                                    !current.isolated &&
                                    current.snapshot.task.worktree
                            ) {
                                OutlinedButton(
                                    onClick = {
                                        vm.clearMessage()
                                        confirm = "cleanup"
                                    },
                                    enabled = !state.busy,
                                ) {
                                    Text("Nettoyer le worktree")
                                }
                            }
                            if (current.isolated)
                                Text(
                                    "Espace de travail et conversation conservés dans un disque privé. Reprenez cette exécution pour continuer."
                                )
                            Text("${statusLabel(current.status)} · ${duration(current)}")
                            Text("Déclenchement : ${current.trigger}")
                            Text(
                                "Créée : ${date(current.createdAt)}\nDébut : ${date(current.startedAt)}\nFin : ${date(current.finishedAt)}"
                            )
                            Text(
                                "Accès : ${current.snapshot.agent.access.sandbox} · ${if (current.snapshot.agent.timeoutMinutes == 0) "Sans limite de temps" else "${current.snapshot.agent.timeoutMinutes} minutes maximum"}"
                            )
                            Text(
                                "Skills : ${current.snapshot.skills.joinToString { it.name }.ifEmpty { "Aucun" }}"
                            )
                            current.usage?.forEach { (key, value) -> Text("$key : $value") }
                            Code("Exécution : ${current.id}\nSession : ${current.sessionId ?: "—"}")
                        }
                    }
            }
                ?: Box(
                    Modifier.fillMaxSize(),
                    contentAlignment = androidx.compose.ui.Alignment.Center,
                ) {
                    CircularProgressIndicator()
                }
        }
        confirm?.let { action ->
            Confirm(
                if (action == "cancel") "Arrêter cette exécution ?" else "Nettoyer ce worktree ?",
                if (action == "cancel")
                    "Le processus s’arrêtera. Les fichiers et les effets déjà produits resteront en place."
                else
                    "Le répertoire sera supprimé. La branche Git et l’historique restent disponibles. Le serveur refuse de supprimer un worktree contenant des changements.",
                state.busy,
                state.error,
                { confirm = null },
            ) {
                vm.perform {
                    api.request("POST", "/runs/$id/$action")
                    confirm = null
                }
            }
        }
    }
}

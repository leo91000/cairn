@file:OptIn(ExperimentalLayoutApi::class, ExperimentalMaterial3Api::class)

package build.cairn.app.ui

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.toggleable
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.Saver
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.luminance
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import build.cairn.app.data.*
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.launch
import kotlinx.serialization.json.decodeFromJsonElement

/** Purple marks rights that can affect other agents or the whole installation. */
@Composable
private fun powerColors(): Pair<Color, Color> =
    if (MaterialTheme.colorScheme.background.luminance() < 0.3f)
        Color(0xFF3A3054) to Color(0xFFCDBBFA)
    else Color(0xFFEDE7FB) to Color(0xFF5B3FB0)

private fun draftSaver() =
    Saver<MutableState<AgentDraft>, String>(
        save = { wireJson.encodeToString(AgentDraft.serializer(), it.value) },
        restore = { mutableStateOf(wireJson.decodeFromString(AgentDraft.serializer(), it)) },
    )

private fun agentSaver() =
    Saver<MutableState<Agent>, String>(
        save = { wireJson.encodeToString(Agent.serializer(), it.value) },
        restore = { mutableStateOf(wireJson.decodeFromString(Agent.serializer(), it)) },
    )

/** The review shown before saving sensitive changes. */
private class PendingReview(
    val edited: Agent,
    val lines: List<ReviewLine>,
    val delegations: List<AgentDelegationEffect>,
)

/**
 * The agent page: everything an agent can reach or do, configured in one place. An overview groups
 * the sections; each opens as a pushed screen. One draft covers the whole page and is saved from
 * the bottom bar. [agentId] is empty to create an agent.
 */
@Composable
fun AgentScreen(
    vm: CairnViewModel,
    state: Workspace,
    agentId: String,
    back: () -> Unit,
    chat: (String) -> Unit,
    navigate: (String) -> Unit,
) {
    val creating = agentId.isBlank()
    val server = state.agents.find { it.id == agentId }
    if (!creating && server == null) {
        Column(Modifier.fillMaxSize()) {
            DetailHeader("Agent", "", back) {}
            if (state.agents.isEmpty()) LinearProgressIndicator(Modifier.fillMaxWidth())
            else
                Empty(
                    "Agent introuvable",
                    "Cet agent a peut-être été supprimé. Revenez à la liste des agents.",
                )
        }
        return
    }
    AgentPage(vm, state, server ?: Agent(), creating, back, chat, navigate)
}

@Composable
private fun AgentPage(
    vm: CairnViewModel,
    state: Workspace,
    loaded: Agent,
    creating: Boolean,
    back: () -> Unit,
    chat: (String) -> Unit,
    navigate: (String) -> Unit,
) {
    var baseline by rememberSaveable(saver = agentSaver()) { mutableStateOf(loaded) }
    var draft by rememberSaveable(saver = draftSaver()) { mutableStateOf(AgentDraft(loaded)) }
    // Deliberately not saveable: never persist a credential in saved instance state.
    var newToken by remember { mutableStateOf("") }
    var section by rememberSaveable { mutableStateOf<String?>(null) }
    var nodes by remember { mutableStateOf<List<ExecutionNode>>(emptyList()) }
    var catalog by remember { mutableStateOf<PluginCatalog?>(null) }
    var tokenSaved by remember { mutableStateOf(false) }
    var saving by remember { mutableStateOf(false) }
    var saveError by remember { mutableStateOf<String?>(null) }
    var errorSection by remember { mutableStateOf<AgentSection?>(null) }
    var conflict by remember { mutableStateOf(false) }
    var review by remember { mutableStateOf<PendingReview?>(null) }
    var reapprove by remember { mutableStateOf(emptySet<String>()) }
    var discarding by remember { mutableStateOf(false) }
    var deleting by remember { mutableStateOf(false) }
    // A new agent keeps one creation identity: retrying after a lost response or a failed
    // token never creates a second agent.
    val creation = rememberResourceSave("")
    val scope = rememberCoroutineScope()
    val agentId = baseline.id
    val main = agentId == MAIN_AGENT_ID

    LaunchedEffect(agentId) {
        try {
            nodes = vm.api.get("/nodes")
        } catch (e: Exception) {
            if (e is CancellationException) throw e
        }
    }
    LaunchedEffect(agentId) {
        try {
            catalog = vm.api.get("/plugins")
        } catch (e: Exception) {
            if (e is CancellationException) throw e
            catalog = PluginCatalog()
        }
    }
    LaunchedEffect(agentId) {
        if (agentId.isNotBlank())
            try {
                tokenSaved =
                    vm.api.get<Configured>("/agents/${segment(agentId)}/github-token").configured
            } catch (e: Exception) {
                if (e is CancellationException) throw e
            }
    }

    val changes = agentChanges(baseline, draft, tokenSaved, newToken)
    val dirty = changes.isNotEmpty()
    // A newer saved version replaces an untouched page; a modified draft keeps its base so a
    // concurrent save is detected by the server.
    LaunchedEffect(loaded.revision, loaded) {
        if (!creating && !dirty && loaded != baseline) {
            baseline = loaded
            draft = AgentDraft(loaded)
        }
    }

    val resources =
        AgentResources(state.projects, state.skills, state.mcps, nodes, catalog, tokenSaved)
    val errors = agentErrors(draft, tokenSaved, newToken)
    val issues = agentIssues(draft.agent, resources)
    val current = AgentSection.entries.find { it.name == section }

    fun update(change: (Agent) -> Agent) {
        draft = draft.copy(agent = change(draft.agent))
        saveError = null
        conflict = false
    }
    fun access(change: (AccessPolicy) -> AccessPolicy) = update {
        it.copy(access = change(it.access))
    }
    fun leave() {
        when {
            current != null -> section = null
            dirty || (creating && draft.agent.name.isNotBlank()) -> discarding = true
            else -> back()
        }
    }
    BackHandler(enabled = !saving) { leave() }

    suspend fun commit(edited: Agent, approved: List<String>) {
        val saved: Agent =
            if (creating)
                wireJson.decodeFromJsonElement<Agent>(
                    creation.save(vm.api, "/agents", { agentSaveBody(edited, approved) })
                )
            else vm.api.send("PUT", "/agents/${segment(agentId)}", agentSaveBody(edited, approved))
        draft.tokenChange(tokenSaved, newToken)?.let { token ->
            try {
                vm.api.request(
                    "PUT",
                    "/agents/${segment(saved.id)}/github-token",
                    body("token" to token),
                )
            } catch (e: Exception) {
                if (e is CancellationException) throw e
                throw IllegalStateException(
                    "Agent enregistré, mais le jeton GitHub n’a pas été enregistré. Réessayez."
                )
            }
            tokenSaved = token.isNotEmpty()
        }
        newToken = ""
        baseline = saved
        draft = AgentDraft(saved)
        // The page is saved; the workspace reloads in the background.
        scope.launch { runCatching { vm.refresh() } }
        vm.notify(if (creating) "Agent créé" else "Agent enregistré")
        if (creating) back()
    }
    fun fail(error: Exception) {
        if (error is CancellationException) throw error
        conflict = error is ApiException && error.status == 409
        saveError =
            if (conflict)
                "Cet agent a été modifié ailleurs. Rechargez-le pour reprendre vos modifications sur sa dernière version."
            else if (creating && creation.pendingCreation)
                "Sauvegarde de l’agent non confirmée. Réessayez pour retrouver le même agent. ${error.message.orEmpty()}"
                    .trim()
            else error.message ?: "L’enregistrement a échoué."
        errorSection = if (conflict) null else sectionOfError(error.message.orEmpty())
    }
    fun save() {
        if (saving || errors.isNotEmpty()) return
        val edited = draft.resolved(tokenSaved)
        saving = true
        saveError = null
        scope.launch {
            try {
                val preview =
                    if (creating) null
                    else
                        vm.api.send<AgentPreview>(
                            "POST",
                            "/agents/${segment(agentId)}/preview",
                            agentSaveBody(edited),
                        )
                val lines = agentReview(baseline, edited, resources, preview)
                if (needsReview(lines, preview)) {
                    val suspended = preview?.suspendedDelegations().orEmpty()
                    reapprove = suspended.filter { it.blocked == null }.map { it.id }.toSet()
                    review = PendingReview(edited, lines, suspended)
                } else commit(edited, emptyList())
            } catch (e: Exception) {
                fail(e)
            } finally {
                saving = false
            }
        }
    }
    fun reload() {
        saving = true
        scope.launch {
            try {
                val theirs =
                    vm.api.get<List<Agent>>("/agents").find { it.id == agentId }
                        ?: error("Cet agent n’existe plus.")
                draft = draft.copy(agent = rebaseAgent(baseline, draft.agent, theirs))
                baseline = theirs
                saveError = null
                conflict = false
                runCatching { vm.refresh() }
            } catch (e: Exception) {
                fail(e)
            } finally {
                saving = false
            }
        }
    }

    Column(Modifier.fillMaxSize().testTag("agent-page")) {
        if (current == null)
            AgentHeader(
                draft.agent,
                creating,
                main,
                back = ::leave,
                chat = { chat(agentId) },
                delete = { deleting = true },
            )
        else DetailHeader(current.title, draft.agent.name.ifBlank { "Nouvel agent" }, ::leave) {}
        HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
        Box(Modifier.weight(1f)) {
            Column(
                Modifier.fillMaxSize()
                    .verticalScroll(rememberScrollState())
                    .padding(horizontal = 16.dp, vertical = 12.dp),
                verticalArrangement = Arrangement.spacedBy(12.dp),
            ) {
                errors[current]?.let { InlineError(it) }
                if (errorSection == current && current != null) saveError?.let { InlineError(it) }
                when (current) {
                    null ->
                        AgentOverview(
                            draft,
                            resources,
                            creating,
                            issues,
                            errors.keys + listOfNotNull(errorSection),
                            changes.toSet(),
                        ) {
                            section = it.name
                        }
                    AgentSection.Projects ->
                        if (main) LockedSection()
                        else
                            ProjectsSection(draft.agent.access, state.projects) { change ->
                                access(change)
                            }
                    AgentSection.Skills ->
                        if (main) LockedSection()
                        else
                            SkillsSection(draft.agent.access, state.skills, state.projects) { change
                                ->
                                access(change)
                            }
                    AgentSection.Mcp ->
                        if (main) LockedSection()
                        else
                            McpSection(
                                draft.agent.access,
                                state.mcps,
                                { navigate("mcps") },
                                ::access,
                            )
                    AgentSection.Github ->
                        if (main) LockedSection()
                        else
                            GithubSection(
                                draft,
                                tokenSaved,
                                newToken,
                                { newToken = it },
                                { mode ->
                                    draft = draft.copy(github = mode.value)
                                    saveError = null
                                },
                            )
                    AgentSection.Plugins ->
                        PluginsSection(draft.agent, catalog, { navigate(it) }, ::access)
                    AgentSection.Nodes -> NodesSection(draft.agent.access, nodes, ::access)
                    AgentSection.Sandbox ->
                        SandboxSection(draft.agent.access.sandbox) { value ->
                            access { it.copy(sandbox = value) }
                        }
                    AgentSection.Powers ->
                        PowersSection(draft.agent.access, nodes, { section = it.name }, ::access)
                    AgentSection.Identity ->
                        IdentitySection(vm, state, baseline, draft.agent, creating, ::update)
                    AgentSection.Engine -> EngineSection(vm, state, draft.agent, ::update)
                    AgentSection.Advanced ->
                        AdvancedSection(draft.timeout) {
                            draft = draft.copy(timeout = it)
                            saveError = null
                        }
                }
            }
        }
        if (dirty || creating || saveError != null)
            SaveBar(
                changes.size,
                creating,
                saving,
                errors,
                saveError.takeIf { errorSection == null || errorSection != current },
                errorSection,
                conflict,
                discard = {
                    if (creating) discarding = true
                    else {
                        draft = AgentDraft(baseline)
                        newToken = ""
                        saveError = null
                        conflict = false
                    }
                },
                save = ::save,
                reload = ::reload,
                open = { section = it.name },
            )
    }

    review?.let { pending ->
        ReviewDialog(
            pending,
            reapprove,
            saving,
            toggle = { id, on -> reapprove = if (on) reapprove + id else reapprove - id },
            dismiss = { review = null },
        ) {
            review = null
            saving = true
            scope.launch {
                try {
                    commit(pending.edited, reapprove.toList())
                } catch (e: Exception) {
                    fail(e)
                } finally {
                    saving = false
                }
            }
        }
    }
    if (discarding)
        AlertDialog(
            onDismissRequest = { discarding = false },
            title = { Text("Quitter sans enregistrer ?") },
            text = {
                Text(
                    when {
                        creating -> "Cet agent ne sera pas créé."
                        changes.size == 1 -> "La modification non enregistrée sera perdue."
                        else -> "Les ${changes.size} modifications non enregistrées seront perdues."
                    }
                )
            },
            confirmButton = {
                TextButton(
                    onClick = {
                        discarding = false
                        draft = AgentDraft(baseline)
                        newToken = ""
                        back()
                    }
                ) {
                    Text("Quitter")
                }
            },
            dismissButton = {
                TextButton(onClick = { discarding = false }) { Text("Continuer") }
            },
        )
    if (deleting)
        Confirm(
            "Supprimer « ${baseline.name} » ?",
            "Le serveur refusera la suppression si une mission utilise encore cet agent.",
            state.busy,
            state.error,
            { deleting = false },
        ) {
            vm.perform {
                delete("/agents/${segment(agentId)}")
                deleting = false
                back()
            }
        }
}

@Composable
private fun AgentHeader(
    agent: Agent,
    creating: Boolean,
    main: Boolean,
    back: () -> Unit,
    chat: () -> Unit,
    delete: () -> Unit,
) {
    var menu by remember { mutableStateOf(false) }
    Row(
        Modifier.fillMaxWidth().padding(horizontal = 4.dp, vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        ActionIcon("Retour", CairnIcons.Back, onClick = back)
        AgentAvatar(agent.name.ifBlank { "?" }, agent.id, 40.dp)
        Spacer(Modifier.width(12.dp))
        Column(Modifier.weight(1f)) {
            Text(
                agent.name.ifBlank { "Nouvel agent" },
                Modifier.semantics { heading() },
                style = MaterialTheme.typography.titleLarge,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            if (agent.description.isNotBlank())
                Text(
                    agent.description,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
        }
        if (!creating)
            Box {
                ActionIcon("Plus d’actions", CairnIcons.More) { menu = true }
                DropdownMenu(menu, { menu = false }) {
                    DropdownMenuItem(
                        text = { Text("Discuter avec cet agent") },
                        onClick = {
                            menu = false
                            chat()
                        },
                    )
                    if (!main)
                        DropdownMenuItem(
                            text = { Text("Supprimer l’agent") },
                            onClick = {
                                menu = false
                                delete()
                            },
                        )
                }
            }
    }
}

@Composable
private fun InlineError(text: String) {
    Text(
        text,
        Modifier.fillMaxWidth()
            .clip(RoundedCornerShape(12.dp))
            .background(MaterialTheme.colorScheme.errorContainer)
            .padding(12.dp),
        color = MaterialTheme.colorScheme.onErrorContainer,
        style = MaterialTheme.typography.bodyMedium,
    )
}

@Composable
private fun Note(text: String, modifier: Modifier = Modifier) {
    Text(
        text,
        modifier,
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
    )
}

@Composable
private fun InfoBox(text: String, modifier: Modifier = Modifier) {
    Text(
        text,
        modifier
            .fillMaxWidth()
            .clip(RoundedCornerShape(14.dp))
            .background(MaterialTheme.colorScheme.secondaryContainer)
            .padding(14.dp),
        style = MaterialTheme.typography.bodyMedium,
        color = MaterialTheme.colorScheme.onSecondaryContainer,
    )
}

@Composable
private fun PowerChip(text: String) {
    val (container, content) = powerColors()
    Text(
        text,
        Modifier.clip(CircleShape)
            .background(container)
            .padding(horizontal = 9.dp, vertical = 3.dp),
        style = MaterialTheme.typography.labelMedium,
        fontWeight = FontWeight.SemiBold,
        color = content,
        maxLines = 1,
    )
}

@Composable
private fun LockedChip() {
    Text(
        "🔒 Fixé",
        Modifier.clip(CircleShape)
            .background(MaterialTheme.colorScheme.surfaceVariant)
            .padding(horizontal = 10.dp, vertical = 4.dp),
        style = MaterialTheme.typography.labelMedium,
        fontWeight = FontWeight.SemiBold,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        maxLines = 1,
    )
}

@Composable
private fun LockedSection() {
    InfoBox(
        "L’agent principal a toujours accès à tous les projets, skills, connexions MCP et à GitHub, y compris futurs. Créez un autre agent pour un accès restreint."
    )
    Row(verticalAlignment = Alignment.CenterVertically) {
        Text("Tous, y compris futurs", Modifier.weight(1f))
        LockedChip()
    }
}

/** One overview row: title, effective summary with state chips, and what opens it. */
@Composable
private fun OverviewRow(
    section: AgentSection,
    summary: String,
    locked: Boolean = false,
    error: Boolean = false,
    changed: Boolean = false,
    chips: @Composable () -> Unit = {},
    open: () -> Unit,
) {
    val description = buildString {
        append(section.title)
        if (summary.isNotBlank()) append(", $summary")
        if (locked) append(", fixé")
        if (error) append(", à corriger")
        if (changed) append(", modifié")
    }
    Row(
        Modifier.fillMaxWidth()
            .heightIn(min = 64.dp)
            .then(
                if (locked) Modifier
                else Modifier.clickable(onClickLabel = "Ouvrir ${section.title}", onClick = open)
            )
            .semantics(mergeDescendants = true) { contentDescription = description }
            .testTag("agent-section-${section.name}")
            .padding(horizontal = 16.dp, vertical = 10.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            Text(section.title, style = MaterialTheme.typography.bodyLarge)
            FlowRow(
                horizontalArrangement = Arrangement.spacedBy(6.dp),
                verticalArrangement = Arrangement.spacedBy(4.dp),
                itemVerticalAlignment = Alignment.CenterVertically,
            ) {
                chips()
                if (summary.isNotBlank())
                    Text(
                        summary,
                        style = MaterialTheme.typography.bodyMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
            }
        }
        if (error)
            Box(
                Modifier.padding(horizontal = 6.dp)
                    .size(8.dp)
                    .background(MaterialTheme.colorScheme.error, CircleShape)
            )
        else if (changed)
            Box(
                Modifier.padding(horizontal = 6.dp)
                    .size(8.dp)
                    .background(MaterialTheme.colorScheme.primary, CircleShape)
            )
        if (locked) LockedChip()
        else
            androidx.compose.material3.Icon(
                CairnIcons.Right,
                null,
                Modifier.size(20.dp),
                tint = MaterialTheme.colorScheme.onSurfaceVariant,
            )
    }
    HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant.copy(alpha = 0.6f))
}

@Composable
private fun AgentOverview(
    draft: AgentDraft,
    resources: AgentResources,
    creating: Boolean,
    issues: List<AgentIssue>,
    errors: Set<AgentSection>,
    changed: Set<AgentSection>,
    open: (AgentSection) -> Unit,
) {
    val agent = draft.agent
    val access = agent.access
    val main = agent.id == MAIN_AGENT_ID
    var showIssues by rememberSaveable { mutableStateOf(false) }
    if (main)
        InfoBox(
            "L’agent principal a toujours accès à tous les projets, skills, connexions MCP et à GitHub, y compris futurs. Créez un autre agent pour un accès restreint."
        )
    if (issues.isNotEmpty())
        Column(
            Modifier.fillMaxWidth()
                .clip(RoundedCornerShape(14.dp))
                .background(signal.warningSoft)
                .clickable { showIssues = !showIssues }
                .padding(14.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Text(
                "⚠ ${issuesLabel(issues.size)}",
                color = signal.warning,
                style = MaterialTheme.typography.titleSmall,
            )
            if (showIssues)
                issues.forEach { issue ->
                    Text(
                        "${issue.section.title} : ${issue.text}",
                        Modifier.fillMaxWidth()
                            .clickable(onClickLabel = "Ouvrir ${issue.section.title}") {
                                open(issue.section)
                            }
                            .padding(vertical = 4.dp),
                        style = MaterialTheme.typography.bodyMedium,
                    )
                }
        }
    if (creating)
        Note(
            "Choisissez ce que ce nouvel agent peut voir et faire, puis créez-le avec le bouton en bas de l’écran."
        )
    val disabledMcps =
        access.mcps.orEmpty().count { id -> resources.mcps.find { it.id == id }?.enabled == false }
    val mode = draft.githubMode(resources.tokenSaved)
    val plugins = pluginSummary(access, resources.catalog)
    val exclusive = access.effectiveExclusive(resources.nodes)
    Column {
        Eyebrow("Accès", Modifier.padding(start = 16.dp, top = 8.dp, bottom = 6.dp))
        OverviewRow(
            AgentSection.Projects,
            selectionSummary(access.projects, resources.projects.size),
            locked = main,
            error = AgentSection.Projects in errors,
            changed = AgentSection.Projects in changed,
        ) {
            open(AgentSection.Projects)
        }
        OverviewRow(
            AgentSection.Skills,
            if (main) "Toutes" else selectionSummary(access.skills, resources.skills.size, true),
            locked = main,
            error = AgentSection.Skills in errors,
            changed = AgentSection.Skills in changed,
        ) {
            open(AgentSection.Skills)
        }
        OverviewRow(
            AgentSection.Mcp,
            when {
                main || access.mcps == null -> "Toutes les connexions"
                access.mcps.isEmpty() -> "Aucune connexion"
                else -> "${access.mcps.size} connexion${if (access.mcps.size > 1) "s" else ""}"
            },
            locked = main,
            error = AgentSection.Mcp in errors,
            changed = AgentSection.Mcp in changed,
            chips = {
                if (!main && disabledMcps > 0)
                    ToneChip(
                        PluginBadge(
                            "$disabledMcps désactivée${if (disabledMcps > 1) "s" else ""}",
                            PluginTone.WARNING,
                        )
                    )
            },
        ) {
            open(AgentSection.Mcp)
        }
        OverviewRow(
            AgentSection.Github,
            if (main) GithubMode.Shared.label else githubSummary(mode),
            locked = main,
            error = AgentSection.Github in errors,
            changed = AgentSection.Github in changed,
        ) {
            open(AgentSection.Github)
        }
        OverviewRow(
            AgentSection.Plugins,
            if (plugins.isBlank()) if (main) "· même pour Main" else "" else plugins,
            error = AgentSection.Plugins in errors,
            changed = AgentSection.Plugins in changed,
            chips = {
                if (plugins.isBlank()) ToneChip(PluginBadge("Aucun accès", PluginTone.MUTED))
            },
        ) {
            open(AgentSection.Plugins)
        }
        Eyebrow("Exécution", Modifier.padding(start = 16.dp, top = 16.dp, bottom = 6.dp))
        OverviewRow(
            AgentSection.Nodes,
            nodesSummary(access, resources.nodes),
            error = AgentSection.Nodes in errors,
            changed = AgentSection.Nodes in changed,
        ) {
            open(AgentSection.Nodes)
        }
        OverviewRow(
            AgentSection.Sandbox,
            sandboxLabel(access.sandbox),
            changed = AgentSection.Sandbox in changed,
        ) {
            open(AgentSection.Sandbox)
        }
        Eyebrow("Droits élevés", Modifier.padding(start = 16.dp, top = 16.dp, bottom = 6.dp))
        OverviewRow(
            AgentSection.Powers,
            when {
                exclusive.isEmpty() && !access.installation -> "Aucun"
                exclusive.isEmpty() -> ""
                access.installation -> ""
                else -> "· gestion désactivée"
            },
            changed = AgentSection.Powers in changed,
            chips = {
                if (exclusive.isNotEmpty()) PowerChip("Exclusivité")
                if (access.installation) PowerChip("Gestion de l’installation")
            },
        ) {
            open(AgentSection.Powers)
        }
        Eyebrow("Profil", Modifier.padding(start = 16.dp, top = 16.dp, bottom = 6.dp))
        OverviewRow(
            AgentSection.Identity,
            agent.description.take(60),
            error = AgentSection.Identity in errors,
            changed = AgentSection.Identity in changed,
        ) {
            open(AgentSection.Identity)
        }
        OverviewRow(
            AgentSection.Engine,
            listOf(
                    providerLabel(agent.provider),
                    agent.model.ifBlank { "modèle par défaut" },
                    effortLabel(agent.reasoning).lowercase(),
                )
                .joinToString(" · "),
            error = AgentSection.Engine in errors,
            changed = AgentSection.Engine in changed,
        ) {
            open(AgentSection.Engine)
        }
        OverviewRow(
            AgentSection.Advanced,
            draft.timeout.trim().toIntOrNull()?.let(::timeoutSummary) ?: "",
            error = AgentSection.Advanced in errors,
            changed = AgentSection.Advanced in changed,
        ) {
            open(AgentSection.Advanced)
        }
    }
}

@Composable
private fun SaveBar(
    count: Int,
    creating: Boolean,
    saving: Boolean,
    errors: Map<AgentSection, String>,
    serverError: String?,
    errorSection: AgentSection?,
    conflict: Boolean,
    discard: () -> Unit,
    save: () -> Unit,
    reload: () -> Unit,
    open: (AgentSection) -> Unit,
) {
    Surface(
        color = MaterialTheme.colorScheme.surface,
        shadowElevation = 8.dp,
        modifier = Modifier.fillMaxWidth().testTag("agent-save-bar"),
    ) {
        Column(
            Modifier.padding(horizontal = 16.dp, vertical = 10.dp),
            verticalArrangement = Arrangement.spacedBy(6.dp),
        ) {
            if (saving) LinearProgressIndicator(Modifier.fillMaxWidth())
            serverError?.let {
                Text(it, color = MaterialTheme.colorScheme.error)
                Row {
                    if (conflict)
                        TextButton(onClick = reload, enabled = !saving) { Text("Recharger") }
                    errorSection?.let { section ->
                        TextButton(onClick = { open(section) }) { Text("Voir ${section.title}") }
                    }
                }
            }
            if (errors.isNotEmpty()) {
                val first = errors.keys.first()
                Text(
                    "À corriger : ${errors.keys.joinToString(", ") { it.title }}",
                    Modifier.clickable(onClickLabel = "Ouvrir ${first.title}") { open(first) },
                    color = MaterialTheme.colorScheme.error,
                    style = MaterialTheme.typography.bodySmall,
                )
            }
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(
                    if (creating) "● Nouvel agent" else "● ${changesLabel(count)}",
                    Modifier.weight(1f),
                    style = MaterialTheme.typography.bodyMedium,
                    fontWeight = FontWeight.SemiBold,
                )
                if (count > 0 || creating)
                    TextButton(onClick = discard, enabled = !saving) { Text("Annuler") }
                Button(
                    onClick = save,
                    enabled = !saving && errors.isEmpty() && (count > 0 || creating),
                ) {
                    Text(if (creating) "Créer l’agent" else "Enregistrer")
                }
            }
        }
    }
}

@Composable
private fun ReviewDialog(
    review: PendingReview,
    reapprove: Set<String>,
    saving: Boolean,
    toggle: (String, Boolean) -> Unit,
    dismiss: () -> Unit,
    confirm: () -> Unit,
) {
    AlertDialog(
        onDismissRequest = { if (!saving) dismiss() },
        modifier = Modifier.testTag("agent-review"),
        title = { Text("Vérifier avant d’enregistrer") },
        text = {
            Column(
                Modifier.verticalScroll(rememberScrollState()),
                verticalArrangement = Arrangement.spacedBy(12.dp),
            ) {
                review.lines.forEach { line ->
                    Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
                        when (line.kind) {
                            ReviewKind.Power -> PowerChip(line.kind.label)
                            ReviewKind.Access ->
                                ToneChip(PluginBadge(line.kind.label, PluginTone.OK))
                            ReviewKind.Reduction ->
                                ToneChip(PluginBadge(line.kind.label, PluginTone.WARNING))
                        }
                        Text(line.text, style = MaterialTheme.typography.bodyMedium)
                    }
                }
                review.delegations.forEach { delegation ->
                    Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
                        ToneChip(PluginBadge("Effet externe", PluginTone.ATTENTION))
                        if (delegation.blocked != null) {
                            Text(
                                "Impossible de réapprouver « ${delegation.name} » : ${delegation.blocked}",
                                style = MaterialTheme.typography.bodyMedium,
                            )
                            Text(
                                "Ce bot sera suspendu jusqu’à sa reconfirmation sur l’instance.",
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.error,
                            )
                        } else {
                            val checked = delegation.id in reapprove
                            Row(
                                Modifier.fillMaxWidth().heightIn(min = 48.dp).toggleable(
                                    value = checked,
                                    role = Role.Checkbox,
                                    enabled = !saving,
                                ) {
                                    toggle(delegation.id, it)
                                },
                                verticalAlignment = Alignment.CenterVertically,
                            ) {
                                Checkbox(checked, null)
                                Spacer(Modifier.width(8.dp))
                                Text(
                                    "Réapprouver les nouveaux accès pour « ${delegation.name} »",
                                    style = MaterialTheme.typography.bodyMedium,
                                )
                            }
                            if (!checked)
                                Text(
                                    "Ce bot sera suspendu jusqu’à sa reconfirmation sur l’instance.",
                                    style = MaterialTheme.typography.bodySmall,
                                    color = MaterialTheme.colorScheme.error,
                                )
                        }
                    }
                }
            }
        },
        confirmButton = {
            TextButton(onClick = confirm, enabled = !saving) { Text("Enregistrer") }
        },
        dismissButton = {
            TextButton(onClick = dismiss, enabled = !saving) { Text("Revenir") }
        },
    )
}

/** « Tous, y compris futurs » / « Choisir ». */
@Composable
private fun ScopeChoice(all: Boolean, feminine: Boolean, change: (Boolean) -> Unit) {
    SingleChoiceSegmentedButtonRow(Modifier.fillMaxWidth()) {
        SegmentedButton(
            selected = all,
            onClick = { change(true) },
            shape = SegmentedButtonDefaults.itemShape(0, 2),
        ) {
            Text(
                if (feminine) "Toutes, y compris futures" else "Tous, y compris futurs",
                maxLines = 2,
            )
        }
        SegmentedButton(
            selected = !all,
            onClick = { change(false) },
            shape = SegmentedButtonDefaults.itemShape(1, 2),
        ) {
            Text("Choisir")
        }
    }
}

/** One choosable item; [unavailable] says why it cannot be used now, [missing] that it is gone. */
private data class ChoiceItem(
    val id: String,
    val label: String,
    val detail: String = "",
    val unavailable: String? = null,
    val missing: Boolean = false,
)

/**
 * The « Choisir » list: search from 8 items, « Sélectionnés » / « Indisponibles » filters, a count,
 * « Tout sélectionner (filtrés) » / « Effacer », and a « Retirer » action for missing targets.
 */
@Composable
private fun ChooseList(
    items: List<ChoiceItem>,
    selected: List<String>,
    search: String,
    change: (List<String>) -> Unit,
    extra: @Composable (ChoiceItem, Boolean) -> Unit = { _, _ -> },
) {
    var query by rememberSaveable { mutableStateOf("") }
    var onlySelected by rememberSaveable { mutableStateOf(false) }
    var onlyUnavailable by rememberSaveable { mutableStateOf(false) }
    if (items.size >= 8) SearchField(search, query) { query = it }
    if (items.isNotEmpty())
        FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            FilterChip(
                onlySelected,
                { onlySelected = !onlySelected },
                label = { Text("Sélectionnés") },
            )
            if (items.any { it.unavailable != null || it.missing })
                FilterChip(
                    onlyUnavailable,
                    { onlyUnavailable = !onlyUnavailable },
                    label = { Text("Indisponibles") },
                )
        }
    val filtered = items.filter {
        (query.isBlank() || "${it.label} ${it.detail}".contains(query.trim(), true)) &&
            (!onlySelected || it.id in selected) &&
            (!onlyUnavailable || it.unavailable != null || it.missing)
    }
    // Wraps at narrow widths instead of squeezing the count.
    FlowRow(
        Modifier.fillMaxWidth(),
        verticalArrangement = Arrangement.Center,
        itemVerticalAlignment = Alignment.CenterVertically,
    ) {
        Text(
            "${selected.count { id -> items.any { it.id == id && !it.missing } }} sur ${items.count { !it.missing }}",
            Modifier.padding(end = 8.dp),
            style = MaterialTheme.typography.labelLarge,
        )
        Spacer(Modifier.weight(1f))
        TextButton(
            onClick = {
                change(
                    (selected +
                            filtered.filter { !it.missing && it.unavailable == null }.map { it.id })
                        .distinct()
                )
            },
            enabled = filtered.isNotEmpty(),
        ) {
            Text("Tout sélectionner (filtrés)")
        }
        TextButton(
            onClick = { change(selected - filtered.map { it.id }.toSet()) },
            enabled = filtered.any { it.id in selected },
        ) {
            Text("Effacer")
        }
    }
    if (items.isEmpty()) Note("Rien à choisir pour l’instant.")
    else if (filtered.isEmpty()) Note("Aucun résultat.")
    filtered.forEach { item ->
        val checked = item.id in selected
        Column {
            Row(
                Modifier.fillMaxWidth()
                    .heightIn(min = 48.dp)
                    .then(
                        if (item.missing) Modifier
                        else
                            Modifier.toggleable(value = checked, role = Role.Checkbox) { on ->
                                change(if (on) selected + item.id else selected - item.id)
                            }
                    )
                    .semantics(mergeDescendants = true) {},
                verticalAlignment = Alignment.CenterVertically,
            ) {
                if (!item.missing) Checkbox(checked, null)
                Column(Modifier.weight(1f).padding(start = 4.dp)) {
                    Text(item.label, style = MaterialTheme.typography.bodyLarge)
                    if (item.detail.isNotBlank()) Note(item.detail)
                    item.unavailable?.let { Note(it) }
                }
                when {
                    item.missing -> {
                        ToneChip(PluginBadge("Introuvable", PluginTone.ATTENTION))
                        TextButton(onClick = { change(selected - item.id) }) { Text("Retirer") }
                    }
                    item.unavailable != null ->
                        ToneChip(PluginBadge("Indisponible", PluginTone.WARNING))
                }
            }
            extra(item, checked)
        }
    }
}

@Composable
private fun ProjectsSection(
    access: AccessPolicy,
    projects: List<Project>,
    change: ((AccessPolicy) -> AccessPolicy) -> Unit,
) {
    Note(
        "Les projets que cet agent peut ouvrir. Une réduction vaut pour les prochaines exécutions : le travail en file qui n’est plus couvert ne démarrera pas."
    )
    ScopeChoice(access.projects == null, false) { all ->
        change { it.withAllProjects(all, projects) }
    }
    if (access.projects == null) {
        Note("Les nouveaux projets seront accessibles dès leur ajout.")
        return
    }
    Note("Limiter les projets désactive la connexion GitHub partagée : utilisez un jeton dédié.")
    val selected = access.projects
    val items =
        projects.map { ChoiceItem(it.id, it.name, it.path) } +
            selected
                .filter { id -> projects.none { it.id == id } }
                .map { ChoiceItem(it, it, missing = true) }
    ChooseList(
        items,
        selected,
        "Rechercher un projet",
        { list ->
            change { it.copy(projects = list) }
        },
    )
}

@Composable
private fun SkillsSection(
    access: AccessPolicy,
    skills: List<Skill>,
    projects: List<Project>,
    change: ((AccessPolicy) -> AccessPolicy) -> Unit,
) {
    Note(
        "Seules les skills globales et celles des projets autorisés peuvent être choisies. S’applique aux prochaines exécutions."
    )
    val choosable = choosableSkills(skills, access)
    ScopeChoice(access.skills == null, true) { all ->
        change { it.copy(skills = if (all) null else it.skills ?: choosable.map(::skillKey)) }
    }
    if (access.skills == null) {
        Note("${choosable.size} disponibles · limitées aux projets autorisés.")
        return
    }
    val selected = access.skills
    val items =
        choosable.map { ChoiceItem(skillKey(it), skillLabel(it, projects), it.description) } +
            selected
                .filter { key -> choosable.none { skillKey(it) == key } }
                .map { key ->
                    val exists = skills.any { skillKey(it) == key }
                    ChoiceItem(
                        key,
                        skillKeyLabel(key, projects),
                        unavailable = if (exists) "Son projet n’est pas autorisé." else null,
                        missing = !exists,
                    )
                }
    ChooseList(
        items,
        selected,
        "Rechercher une skill",
        { list ->
            change { it.copy(skills = list) }
        },
    )
}

@Composable
private fun McpTools(
    mcp: Mcp,
    access: AccessPolicy,
    change: ((AccessPolicy) -> AccessPolicy) -> Unit,
) {
    var open by rememberSaveable(mcp.id) { mutableStateOf(false) }
    val enabled = mcp.tools.filter { mcp.enabledTools == null || it.name in mcp.enabledTools }
    val chosen = access.mcpTools[mcp.id]
    Column(Modifier.padding(start = 12.dp)) {
        TextButton(onClick = { open = !open }) {
            Text(
                "Outils : " +
                    if (chosen == null) "tous les outils activés"
                    else
                        "${enabled.size} activés sur la connexion · ${chosen.count { name -> enabled.any { it.name == name } }} choisis"
            )
        }
        if (open) {
            Toggle("Tous les outils activés de « ${mcp.name} »", chosen == null) { all ->
                change {
                    it.copy(
                        mcpTools =
                            if (all) it.mcpTools - mcp.id
                            else it.mcpTools + (mcp.id to enabled.map { tool -> tool.name })
                    )
                }
            }
            if (chosen != null)
                enabled.forEach { tool ->
                    Toggle(tool.title ?: tool.name, tool.name in chosen) { on ->
                        change {
                            val old = it.mcpTools[mcp.id].orEmpty()
                            it.copy(
                                mcpTools =
                                    it.mcpTools +
                                        (mcp.id to if (on) old + tool.name else old - tool.name)
                            )
                        }
                    }
                }
            if (enabled.isEmpty())
                Note("Testez la connexion dans Serveurs MCP pour découvrir ses outils.")
        }
    }
}

@Composable
private fun McpSection(
    access: AccessPolicy,
    mcps: List<Mcp>,
    openMcps: () -> Unit,
    change: ((AccessPolicy) -> AccessPolicy) -> Unit,
) {
    Note(
        "L’accès réel combine les connexions accordées ici et les outils activés sur chaque connexion. S’applique aux prochaines exécutions."
    )
    if (mcps.isEmpty()) {
        Empty("Aucune connexion MCP", "Ajoutez une connexion pour la proposer à vos agents.")
        TextButton(onClick = openMcps) { Text("Ouvrir les serveurs MCP ›") }
        return
    }
    ScopeChoice(access.mcps == null, true) { all ->
        change {
            it.copy(
                mcps = if (all) null else it.mcps ?: mcps.map { mcp -> mcp.id },
                mcpTools = it.mcpTools,
            )
        }
    }
    if (access.mcps == null) {
        mcps.forEach { mcp ->
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(mcp.name, Modifier.weight(1f), style = MaterialTheme.typography.bodyLarge)
                if (!mcp.enabled)
                    ToneChip(PluginBadge("Désactivée sur l’installation", PluginTone.WARNING))
            }
            McpTools(mcp, access, change)
        }
        return
    }
    val selected = access.mcps
    val items =
        mcps.map {
            ChoiceItem(
                it.id,
                it.name,
                it.url.ifBlank { it.command },
                unavailable = if (it.enabled) null else "Désactivée sur l’installation.",
            )
        } +
            selected
                .filter { id -> mcps.none { it.id == id } }
                .map { ChoiceItem(it, it, missing = true) }
    ChooseList(
        items,
        selected,
        "Rechercher une connexion",
        { list ->
            change {
                it.copy(mcps = list, mcpTools = it.mcpTools.filterKeys { id -> id in list })
            }
        },
    ) { item, checked ->
        mcps.find { it.id == item.id }?.let { if (checked) McpTools(it, access, change) }
    }
}

@Composable
private fun RadioRow(
    label: String,
    detail: String,
    selected: Boolean,
    enabled: Boolean = true,
    select: () -> Unit,
) {
    Row(
        Modifier.fillMaxWidth()
            .heightIn(min = 56.dp)
            .selectable(selected, enabled = enabled, role = Role.RadioButton, onClick = select)
            .padding(vertical = 4.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        RadioButton(selected, null, enabled = enabled)
        Column(Modifier.weight(1f).padding(start = 8.dp)) {
            Text(
                label,
                style = MaterialTheme.typography.bodyLarge,
                color =
                    if (enabled) MaterialTheme.colorScheme.onSurface
                    else MaterialTheme.colorScheme.onSurfaceVariant,
            )
            if (detail.isNotBlank()) Note(detail)
        }
    }
}

@Composable
private fun GithubSection(
    draft: AgentDraft,
    tokenSaved: Boolean,
    token: String,
    changeToken: (String) -> Unit,
    choose: (GithubMode) -> Unit,
) {
    val mode = draft.githubMode(tokenSaved)
    val allProjects = draft.agent.access.projects == null
    RadioRow(
        GithubMode.Shared.label,
        if (allProjects) "Utilise le compte GitHub connecté à l’installation."
        else "Impossible : projets limités. Choisissez « Tous » dans Projets pour l’utiliser.",
        mode == GithubMode.Shared && allProjects,
        enabled = allProjects,
    ) {
        choose(GithubMode.Shared)
    }
    RadioRow(
        GithubMode.Dedicated.label,
        "Un jeton personnel propre à cet agent.",
        mode == GithubMode.Dedicated,
    ) {
        choose(GithubMode.Dedicated)
    }
    if (mode == GithubMode.Dedicated)
        Column(
            Modifier.padding(start = 40.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            if (tokenSaved) Note("Un jeton est enregistré.")
            SecretField(
                if (tokenSaved) "Nouveau jeton (facultatif)" else "Jeton GitHub dédié",
                token,
                changeToken,
            )
            Note("Le jeton est enregistré avec la fiche.")
            if (tokenSaved)
                TextButton(onClick = { choose(GithubMode.None) }) {
                    Text("Retirer le jeton enregistré")
                }
        }
    RadioRow(
        GithubMode.None.label,
        if (tokenSaved && mode == GithubMode.None)
            "Le jeton enregistré sera supprimé à l’enregistrement."
        else "L’agent n’utilise pas GitHub.",
        mode == GithubMode.None,
    ) {
        choose(GithubMode.None)
    }
}

@Composable
private fun SwitchRow(
    label: String,
    detail: String,
    checked: Boolean,
    enabled: Boolean = true,
    power: Boolean = false,
    description: String = label,
    change: (Boolean) -> Unit,
) {
    val (_, powerContent) = powerColors()
    Row(
        Modifier.fillMaxWidth()
            .heightIn(min = 56.dp)
            .toggleable(
                value = checked,
                enabled = enabled,
                role = Role.Switch,
                onValueChange = change,
            )
            .semantics(mergeDescendants = true) { contentDescription = description }
            .padding(vertical = 4.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Column(Modifier.weight(1f)) {
            Text(
                label,
                style = MaterialTheme.typography.bodyLarge,
                color =
                    if (enabled) MaterialTheme.colorScheme.onSurface
                    else MaterialTheme.colorScheme.onSurfaceVariant,
            )
            if (detail.isNotBlank()) Note(detail)
        }
        Spacer(Modifier.width(12.dp))
        Switch(
            checked,
            null,
            enabled = enabled,
            colors =
                if (power)
                    SwitchDefaults.colors(
                        checkedTrackColor = powerContent,
                        checkedBorderColor = powerContent,
                    )
                else SwitchDefaults.colors(),
        )
    }
}

@Composable
private fun PluginsSection(
    agent: Agent,
    catalog: PluginCatalog?,
    navigate: (String) -> Unit,
    change: ((AccessPolicy) -> AccessPolicy) -> Unit,
) {
    if (catalog == null) {
        LinearProgressIndicator(Modifier.fillMaxWidth())
        return
    }
    val granted = agent.access.plugins
    val groups = catalog.agentGrantable()
    if (groups.isEmpty())
        Empty(
            "Aucun plugin ne donne d’accès aux agents",
            "Ajoutez un compte de service 1Password ou une instance de plugin pour l’accorder ici.",
        )
    groups.forEach { (entry, connections) ->
        val onePassword = entry.id == "onepassword"
        Panel {
            Text(
                if (onePassword) "${entry.name} · comptes de service" else entry.name,
                Modifier.semantics { heading() },
                style = MaterialTheme.typography.titleMedium,
            )
            Note(
                if (onePassword) "Lecture seule · aucun accès par défaut, Main compris"
                else
                    entry.manifest?.agentAccess?.description?.ifBlank { null }
                        ?: "Outils, commandes et instructions apportés par chaque instance"
            )
            connections.forEach { connection ->
                val on = connection.id in granted
                val failed = connection.status.state in setOf("error", "failed")
                Column {
                    SwitchRow(
                        connection.name,
                        when {
                            on && !connection.enabled ->
                                "L’accès est conservé, mais l’agent ne peut rien utiliser tant que ${if (onePassword) "le compte" else "l’instance"} reste désactivé${if (onePassword) "" else "e"}."
                            !connection.enabled ->
                                "Non accordé · désactivé${if (onePassword) "" else "e"}"
                            on -> "Accordé"
                            else -> "Non accordé"
                        },
                        on,
                        description = "Accès à ${connection.name}",
                    ) { allowed ->
                        change {
                            it.copy(
                                plugins =
                                    if (allowed) (it.plugins + connection.id).distinct()
                                    else it.plugins - connection.id
                            )
                        }
                    }
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        ToneChip(
                            when {
                                !connection.enabled ->
                                    PluginBadge(
                                        if (onePassword) "Compte désactivé" else "Désactivée",
                                        PluginTone.WARNING,
                                    )
                                failed -> PluginBadge("Test échoué", PluginTone.ATTENTION)
                                else -> PluginBadge("Actif", PluginTone.OK)
                            }
                        )
                        Spacer(Modifier.weight(1f))
                        TextButton(onClick = { navigate("plugins/${segment(entry.id)}") }) {
                            Text(if (onePassword) "Configurer le compte ↗" else "Configurer ↗")
                        }
                    }
                }
            }
            if (onePassword)
                Note(
                    "Avec un compte accordé, l’agent peut lister les coffres, les éléments et les noms de champs, puis lire une valeur avec cairn plugin onepassword read. Il ne peut ni créer, ni modifier, ni supprimer. Ce que le compte voit se règle dans 1Password : créez un autre compte de service pour d’autres droits. Retirer l’accès bloque immédiatement les lectures suivantes ; un secret déjà lu ne peut pas être rappelé."
                )
            else Note("Effectif immédiatement, y compris dans les conversations en cours.")
        }
    }
    val missing = granted.filter { id -> catalog.connections.none { it.id == id } }
    missing.forEach { id ->
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text("Instance supprimée", Modifier.weight(1f))
            ToneChip(PluginBadge("Introuvable", PluginTone.ATTENTION))
            TextButton(onClick = { change { it.copy(plugins = it.plugins - id) } }) {
                Text("Retirer")
            }
        }
    }
    val delegations = catalog.connections.filter { agent.id.isNotBlank() && it.agentId == agent.id }
    if (delegations.isNotEmpty())
        Panel {
            Text("Délégations", style = MaterialTheme.typography.titleMedium)
            delegations.forEach { connection ->
                val plugin = catalog.packages.find { it.id == connection.pluginId }?.name
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text(
                        "Répond en tant que cet agent sur « ${connection.name} »" +
                            (plugin?.let { " ($it)" } ?: ""),
                        Modifier.weight(1f),
                    )
                    ToneChip(PluginBadge("Réglé sur l’instance", PluginTone.MUTED))
                }
                TextButton(onClick = { navigate("plugins/${segment(connection.pluginId)}") }) {
                    Text("Ouvrir ↗")
                }
            }
            Note(
                "Modifier les accès de cet agent suspend ces bots jusqu’à leur reconfirmation (vous pourrez les réapprouver en enregistrant)."
            )
        }
    if (groups.isEmpty() && delegations.isEmpty())
        TextButton(onClick = { navigate("plugins") }) { Text("Ouvrir les plugins ›") }
}

private fun nodeStatus(node: ExecutionNode): PluginBadge =
    when {
        node.local -> PluginBadge("En ligne", PluginTone.OK)
        node.status == "offline" -> PluginBadge("Hors ligne", PluginTone.WARNING)
        node.maintenance != null || (node.status == "online" && !node.executionReady) ->
            PluginBadge("Runtime à mettre à jour", PluginTone.WARNING)
        !node.accepting -> PluginBadge("N’accepte pas de travaux", PluginTone.MUTED)
        else -> PluginBadge("En ligne", PluginTone.OK)
    }

@Composable
private fun NodesSection(
    access: AccessPolicy,
    nodes: List<ExecutionNode>,
    change: ((AccessPolicy) -> AccessPolicy) -> Unit,
) {
    var explain by remember { mutableStateOf(false) }
    var query by rememberSaveable { mutableStateOf("") }
    val choices = agentNodeChoices(nodes)
    SwitchRow(
        "Toutes les nodes, y compris futures",
        "Les nouvelles machines seront utilisables par cet agent dès leur inscription.",
        access.nodes == null,
    ) { all ->
        change { it.withAllNodes(all, nodes) }
    }
    if (choices.size >= 8) SearchField("Rechercher une node ou un tag", query) { query = it }
    choices
        .filter {
            query.isBlank() ||
                "${it.name} ${it.tags.joinToString(" ")} ${it.capabilities.os}"
                    .contains(query.trim(), true)
        }
        .forEach { node ->
            val runs = access.runsOn(node.id)
            val exclusive = node.id in access.exclusiveNodes
            Column(
                Modifier.fillMaxWidth().testTag("agent-node-${node.id}"),
                verticalArrangement = Arrangement.spacedBy(2.dp),
            ) {
                Row(
                    Modifier.padding(top = 10.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Text(
                        node.name,
                        style = MaterialTheme.typography.titleSmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                    Spacer(Modifier.width(8.dp))
                    ToneChip(nodeStatus(node))
                }
                val facts =
                    (node.tags +
                            listOfNotNull(
                                node.capabilities.os.ifBlank { null },
                                if (node.local) null else "${node.capabilities.cpu} CPU",
                            ))
                        .joinToString(" · ")
                if (facts.isNotBlank()) Note(facts)
                SwitchRow(
                    "Exécuter ses conversations",
                    if (access.nodes == null) "Inclus dans « Toutes les nodes »" else "",
                    runs,
                    enabled = access.nodes != null,
                    description = "Exécuter ses conversations sur ${node.name}",
                ) { on ->
                    change { it.withNodeRun(node.id, on) }
                }
                HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
                val supported = node.exclusiveReservations
                SwitchRow(
                    "Réserver en exclusivité",
                    when {
                        !supported -> "Indisponible sur ce runtime"
                        !runs -> "Nécessite le droit d’exécuter"
                        else -> "Suspend les autres conversations"
                    },
                    exclusive,
                    enabled = runs && supported,
                    power = true,
                    description = "Réserver ${node.name} en exclusivité",
                ) { on ->
                    change { it.withExclusive(node.id, on) }
                }
                if (exclusive && (!runs || !supported))
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Note(
                            if (!runs) "Inactif : ne peut pas s’exécuter ici."
                            else "Inactif tant que le runtime n’est pas mis à jour.",
                            Modifier.weight(1f),
                        )
                        TextButton(onClick = { change { it.withExclusive(node.id, false) } }) {
                            Text("Retirer")
                        }
                    }
                HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
            }
        }
    if (nodes.isNotEmpty())
        access.exclusiveNodes
            .filter { id -> choices.none { it.id == id } }
            .forEach { id ->
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text("Node supprimée ou révoquée", Modifier.weight(1f))
                    ToneChip(PluginBadge("Introuvable", PluginTone.ATTENTION))
                    TextButton(onClick = { change { it.withExclusive(id, false) } }) {
                        Text("Retirer")
                    }
                }
            }
    Text(
        "ⓘ Exécuter ou suspendre les autres : quelle différence ?",
        Modifier.fillMaxWidth()
            .clip(RoundedCornerShape(14.dp))
            .background(MaterialTheme.colorScheme.secondaryContainer)
            .clickable { explain = true }
            .padding(14.dp),
        color = MaterialTheme.colorScheme.onSecondaryContainer,
    )
    if (explain)
        DetailSheet("Exécuter ou suspendre les autres", { explain = false }) {
            Text(
                "Exécuter : ses conversations peuvent démarrer ou être déplacées sur cette machine et partagent ses slots et budgets avec les autres agents."
            )
            Text(
                "Réserver en exclusivité : pendant un travail intensif, l’agent peut suspendre toutes les autres conversations de la machine, pour 15 minutes par demande et 1 heure au total avec les renouvellements. Elles reprennent automatiquement. Ce droit exige le droit d’exécuter."
            )
            Text(
                "Ces droits s’appliquent immédiatement, y compris dans les conversations en cours, et ne bloquent jamais le travail en file."
            )
        }
}

@Composable
private fun SandboxSection(value: String, change: (String) -> Unit) {
    Note("Un mode plus strict bloque les exécutions en file qui demandaient plus.")
    SANDBOX_CHOICES.forEach { (key, label, effect) ->
        RadioRow(label, effect, value == key) { change(key) }
    }
}

@Composable
private fun PowersSection(
    access: AccessPolicy,
    nodes: List<ExecutionNode>,
    open: (AgentSection) -> Unit,
    change: ((AccessPolicy) -> AccessPolicy) -> Unit,
) {
    val (container, content) = powerColors()
    Column(
        Modifier.fillMaxWidth()
            .clip(RoundedCornerShape(16.dp))
            .background(container.copy(alpha = 0.5f))
            .padding(14.dp),
        verticalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        Text("Droits élevés", color = content, style = MaterialTheme.typography.titleSmall)
        SwitchRow(
            "Gérer l’installation Cairn",
            "Créer et modifier les agents (y compris leurs permissions), projets, skills, connexions MCP, missions et exécutions de toute l’installation, au nom du propriétaire. S’applique même si les accès ci-dessus sont restreints.",
            access.installation,
            power = true,
        ) { on ->
            change { it.copy(installation = on) }
        }
    }
    val exclusive = access.effectiveExclusive(nodes)
    Panel {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text(
                "Réservation exclusive",
                Modifier.weight(1f),
                style = MaterialTheme.typography.titleMedium,
            )
            if (exclusive.isNotEmpty())
                PowerChip(if (exclusive.size == 1) "1 node" else "${exclusive.size} nodes")
        }
        Text(
            if (exclusive.isEmpty()) "Aucune node"
            else "Accordée sur ${exclusive.joinToString(", ") { it.name }}"
        )
        TextButton(onClick = { open(AgentSection.Nodes) }) { Text("Régler par node ›") }
    }
    Note(
        "La réservation exclusive et les accès aux plugins restent réservés au propriétaire : un agent qui gère l’installation ne peut pas les accorder."
    )
    Panel {
        Text("Toujours disponible", style = MaterialTheme.typography.titleMedium)
        Text(
            "Publier des livrables (y compris en lien public), attendre un délai ou une condition, voir ses nodes et y déplacer la conversation, rendre compte du résultat."
        )
    }
}

@Composable
private fun IdentitySection(
    vm: CairnViewModel,
    state: Workspace,
    saved: Agent,
    agent: Agent,
    creating: Boolean,
    update: ((Agent) -> Agent) -> Unit,
) {
    Field("Nom", agent.name, { value -> update { it.copy(name = value) } })
    Field("Description", agent.description, { value -> update { it.copy(description = value) } }, 3)
    Field(
        "Instructions",
        agent.instructions,
        { value -> update { it.copy(instructions = value) } },
        6,
    )
    if (!creating) AgentPortraitEditor(vm, state, saved)
    else Note("Le portrait pourra être ajouté après la création.")
}

@Composable
private fun EngineSection(
    vm: CairnViewModel,
    state: Workspace,
    agent: Agent,
    update: ((Agent) -> Agent) -> Unit,
) {
    ModelPicker(
        state.modelCatalog(agent.provider),
        agent.model,
        agent.reasoning,
        enabled = !state.busy,
        refresh = { vm.refreshModels(agent.provider) },
        provider = agent.provider,
        changeProvider = { provider ->
            update { it.copy(provider = provider, model = "", reasoning = "") }
        },
        field = true,
    ) { model, reasoning ->
        update { it.copy(model = model, reasoning = reasoning) }
    }
    InfoBox(
        "Utilise le pool partagé de comptes ${providerLabel(agent.provider)} de l’installation. Ce n’est pas une permission."
    )
}

@Composable
private fun AdvancedSection(timeout: String, change: (String) -> Unit) {
    val minutes = timeout.trim().toIntOrNull()
    Toggle("Sans limite de temps", minutes == 0) { change(if (it) "0" else "120") }
    if (minutes != 0)
        Field(
            "Limite en minutes (1–720)",
            timeout,
            change,
            keyboardOptions = InputKeyboards.Number,
        )
    Note("Une exécution qui dépasse cette limite est arrêtée.")
}

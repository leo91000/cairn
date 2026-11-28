@file:OptIn(ExperimentalLayoutApi::class, ExperimentalMaterial3Api::class)

package build.cairn.app.ui

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.Saver
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.luminance
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.unit.dp
import build.cairn.app.data.*
import build.cairn.contracts.AgentInput
import build.cairn.contracts.CreateAgentInput
import build.cairn.contracts.ListAgentsInput
import build.cairn.contracts.PluginGrantAccess
import build.cairn.contracts.ReplaceAgentInput
import build.cairn.contracts.ReplaceAgentPluginGrantsInput
import build.cairn.contracts.createAgent
import build.cairn.contracts.deleteAgent
import build.cairn.contracts.getAgent
import build.cairn.contracts.getAgentPluginGrants
import build.cairn.contracts.listAgents
import build.cairn.contracts.previewAgent
import build.cairn.contracts.replaceAgent
import build.cairn.contracts.replaceAgentPluginGrants
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.launch
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.decodeFromJsonElement
import kotlinx.serialization.json.encodeToJsonElement

/** Purple marks rights that can affect other agents or the whole installation. */
@Composable
internal fun powerColors(): Pair<Color, Color> =
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
internal class PendingReview(
    val edited: Agent,
    val lines: List<ReviewLine>,
    val delegations: List<AgentDelegationEffect>,
    val nativeNodes: List<String>,
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
    AgentPage(vm, state, server ?: newAgent(), creating, back, chat, navigate)
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
    var savedGrantsJson by rememberSaveable {
        mutableStateOf(wireJson.encodeToString(PluginGrantDraft.serializer(), PluginGrantDraft()))
    }
    var grantsJson by rememberSaveable { mutableStateOf(savedGrantsJson) }
    val savedGrants = wireJson.decodeFromString(PluginGrantDraft.serializer(), savedGrantsJson)
    val grants = wireJson.decodeFromString(PluginGrantDraft.serializer(), grantsJson)
    var grantsLoaded by rememberSaveable { mutableStateOf(creating) }
    var grantsError by remember { mutableStateOf<String?>(null) }
    var grantsAttempt by remember { mutableIntStateOf(0) }
    fun editGrants(value: PluginGrantDraft) {
        grantsJson = wireJson.encodeToString(PluginGrantDraft.serializer(), value)
    }
    var section by rememberSaveable { mutableStateOf<String?>(null) }
    var nodes by remember { mutableStateOf<List<ExecutionNode>>(emptyList()) }
    var nodesLoaded by remember { mutableStateOf(false) }
    // Without the machine list, node access cannot be changed safely.
    var nodesFailed by remember { mutableStateOf(false) }
    var nodesAttempt by remember { mutableIntStateOf(0) }
    var catalog by remember { mutableStateOf<PluginCatalog?>(null) }
    // Without the catalog, held grants cannot be checked: the section stays read-only.
    var catalogFailed by remember { mutableStateOf(false) }
    var catalogAttempt by remember { mutableIntStateOf(0) }
    var saving by remember { mutableStateOf(false) }
    var saveError by remember { mutableStateOf<String?>(null) }
    var errorSection by remember { mutableStateOf<AgentSection?>(null) }
    var conflict by remember { mutableStateOf(false) }
    var review by remember { mutableStateOf<PendingReview?>(null) }
    var reapprove by remember { mutableStateOf(emptySet<String>()) }
    var discarding by remember { mutableStateOf(false) }
    var reloadFailed by remember { mutableStateOf(false) }
    var deleting by remember { mutableStateOf(false) }
    // A new agent keeps one creation identity: retrying after a lost response never creates a
    // second agent.
    val creation = rememberResourceSave("")
    val scope = rememberCoroutineScope()
    val agentId = baseline.id
    val main = agentId == MAIN_AGENT_ID
    // Account access applies at once and never joins the profile draft.
    val people = remember(agentId) { AgentPeople() }
    var peopleAttempt by remember { mutableIntStateOf(0) }
    LaunchedEffect(agentId, peopleAttempt, creating) {
        if (!creating) people.load(vm, agentId)
    }

    LaunchedEffect(agentId, nodesAttempt) {
        try {
            nodes = vm.api.get("/nodes")
            nodesLoaded = true
            nodesFailed = false
        } catch (e: Exception) {
            if (e is CancellationException) throw e
            nodesLoaded = false
            nodesFailed = true
        }
    }
    LaunchedEffect(agentId, catalogAttempt) {
        try {
            catalog = vm.api.get("/plugins")
            catalogFailed = false
        } catch (e: Exception) {
            if (e is CancellationException) throw e
            // A failed re-read leaves the catalog unknown, not as it was.
            catalog = null
            catalogFailed = true
        }
    }

    LaunchedEffect(agentId, grantsAttempt) {
        if (creating || grantsLoaded) return@LaunchedEffect
        try {
            val currentGrants = vm.api.getAgentPluginGrants(AgentInput(agentId))
            if (currentGrants.revision != baseline.revision)
                throw ApiException(
                    409,
                    "L’agent a changé. Rechargez-le avant de modifier ses droits.",
                )
            savedGrantsJson =
                wireJson.encodeToString(PluginGrantDraft.serializer(), currentGrants.draft())
            grantsJson = savedGrantsJson
            grantsLoaded = true
            grantsError = null
        } catch (e: Exception) {
            if (e is CancellationException) throw e
            grantsError = e.message
            if (e is ApiException && e.status == 409) {
                conflict = true
                saveError = "L’agent a changé. Rechargez-le avant de modifier ses droits."
            }
        }
    }
    val grantChanges = grants.key() != savedGrants.key()
    val changes =
        (agentChanges(baseline, draft) +
                if (grantChanges) listOf(AgentSection.Plugins) else emptyList())
            .distinct()
    val dirty = changes.isNotEmpty()
    // A newer saved version replaces an untouched page; a modified draft keeps its base so a
    // concurrent save is detected by the server.
    LaunchedEffect(loaded.revision, loaded) {
        if (!creating && !dirty && loaded != baseline) {
            val agentRevisionChanged = loaded.revision != baseline.revision
            baseline = loaded
            draft = AgentDraft(loaded)
            if (agentRevisionChanged) {
                grantsLoaded = false
                grantsError = null
                grantsAttempt++
            }
        }
    }

    // Existing MCP connections (owners load them): tool choices of deleted ones are never sent.
    val mcpIds = state.mcps.map { it.id }.toSet().takeIf { state.isOwner }
    val resources =
        AgentResources(
            state.projects,
            state.skills,
            state.mcps,
            nodes,
            catalog,
            nodesLoaded = nodesLoaded,
            // Skills arrive with the agents, in the same workspace load.
            skillsLoaded = state.agents.isNotEmpty(),
        )
    val errors = agentErrors(draft)
    val displayDraft =
        if (grantsLoaded)
            draft.copy(agent = draft.agent.copy(plugins = grants.grants.map { it.instanceId }))
        else draft
    val issues = agentIssues(displayDraft.agent, resources)
    val current = AgentSection.entries.find { it.name == section }

    fun update(change: (Agent) -> Agent) {
        draft = draft.copy(agent = change(draft.agent))
        saveError = null
        conflict = false
    }
    // Tool choices of connections no longer granted stay in the draft, so granting the connection
    // again restores them; only granted ones are saved.
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

    suspend fun requireCodingAgentConnection(provider: String) {
        val catalog =
            checkNotNull(vm.refreshModels(provider, availabilityOnly = true)) {
                "Impossible de vérifier la connexion de cet assistant de code. Réessayez."
            }
        check(catalog.connected != false) {
            "Connectez ${providerLabel(provider)} ou choisissez un autre assistant de code."
        }
    }

    suspend fun retryAgentCreation(input: CreateAgentInput, latest: () -> CreateAgentInput): Agent {
        val agentId = input.creationId ?: return vm.api.createAgent(input)
        try {
            requireCodingAgentConnection(input.settings.provider)
        } catch (connectionError: IllegalStateException) {
            if (connectionError is CancellationException) throw connectionError
            // Recover an existing agent without writing the original disconnected provider.
            try {
                return vm.api.getAgent(AgentInput(agentId))
            } catch (readError: ApiException) {
                if (readError.status != 404) throw readError
            }

            val current = latest().copy(creationId = agentId)
            if (current.settings.provider == input.settings.provider) throw connectionError
            requireCodingAgentConnection(current.settings.provider)
            return vm.api.createAgent(current)
        }
        return vm.api.createAgent(input)
    }

    suspend fun commit(
        edited: Agent,
        approved: List<String>,
        nativeNodes: List<String> = emptyList(),
    ) {
        val needsConnection =
            if (creating) creation.id.isBlank() && !creation.pendingCreation
            else edited.provider != baseline.provider
        if (needsConnection) requireCodingAgentConnection(edited.provider)

        // Explicit reapproval uses the revision-checked profile API, including grant-only edits.
        val profilePending =
            (creating && creation.id.isBlank()) ||
                profileChanged(baseline, edited) ||
                approved.isNotEmpty()
        var saved: Agent =
            when {
                !profilePending -> baseline
                creating -> {
                    // Once created, the agent's revision guards the recovery update, so a change
                    // made elsewhere meanwhile is reported as a conflict instead of overwritten.
                    val replaying = creation.pendingCreation
                    val createBody = {
                        agentSaveBody(
                                edited.copy(revision = ""),
                                approved,
                                mcpIds,
                                nativeNodeReviews = nativeNodes,
                            )
                            .let { JsonObject(it - "agentId" - "revision") }
                    }
                    val response =
                        try {
                            creation.save(
                                vm.api,
                                "/agents",
                                value = {
                                    agentSaveBody(
                                        edited.copy(id = baseline.id, revision = baseline.revision),
                                        approved,
                                        mcpIds,
                                        nativeNodeReviews = nativeNodes,
                                    )
                                },
                                createValue = createBody,
                                onCreated = {
                                    baseline = wireJson.decodeFromJsonElement<Agent>(it)
                                },
                                request = { method, _, payload ->
                                    val saved =
                                        if (method == "POST") {
                                            val input =
                                                wireJson.decodeFromJsonElement<CreateAgentInput>(
                                                    payload
                                                )
                                            if (replaying)
                                                retryAgentCreation(input) {
                                                    wireJson.decodeFromJsonElement<
                                                        CreateAgentInput
                                                    >(
                                                        createBody()
                                                    )
                                                }
                                            else vm.api.createAgent(input)
                                        } else {
                                            if (edited.provider != baseline.provider)
                                                requireCodingAgentConnection(edited.provider)
                                            vm.api.replaceAgent(
                                                wireJson.decodeFromJsonElement<ReplaceAgentInput>(
                                                    payload
                                                )
                                            )
                                        }
                                    wireJson.encodeToJsonElement(saved)
                                },
                            )
                        } catch (e: Exception) {
                            if (e is CancellationException) throw e
                            // The agent exists; only the edits made since its creation failed.
                            val conflicting = e is ApiException && e.status == 409
                            if (creation.id.isBlank() || conflicting) throw e
                            throw IllegalStateException(
                                "Agent enregistré, mais les dernières modifications n’ont pas été enregistrées. Réessayez. ${e.message.orEmpty()}"
                                    .trim()
                            )
                        }
                    wireJson.decodeFromJsonElement<Agent>(response)
                }
                else ->
                    vm.api.replaceAgent(
                        wireJson.decodeFromJsonElement<ReplaceAgentInput>(
                            agentSaveBody(
                                edited,
                                approved,
                                mcpIds,
                                nativeNodeReviews = nativeNodes,
                            )
                        )
                    )
            }

        baseline = saved
        draft = AgentDraft(saved)
        if (grantChanges) {
            val updated =
                vm.api.replaceAgentPluginGrants(
                    ReplaceAgentPluginGrantsInput(
                        saved.id,
                        grants.grants,
                        saved.revision,
                        grants.runCredentialInstances,
                    )
                )
            savedGrantsJson =
                wireJson.encodeToString(PluginGrantDraft.serializer(), updated.draft())
            grantsJson = savedGrantsJson
            saved =
                saved.copy(
                    revision = updated.revision,
                    plugins =
                        updated.grants
                            .filter { it.access is PluginGrantAccess.A }
                            .map { it.instanceId },
                )
            baseline = saved
            draft = AgentDraft(saved)
        }
        try {
            vm.refresh()
        } catch (e: Exception) {
            if (e is CancellationException) throw e
            // The agent is saved; saving again only retries the reload.
            reloadFailed = true
            throw IllegalStateException(
                "Agent enregistré, mais le rechargement a échoué. Réessayez."
            )
        }
        reloadFailed = false
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
        if (saving || conflict || errors.isNotEmpty()) return
        val resolvedDraft = draft.resolved()
        val edited = resolvedDraft
        saving = true
        saveError = null
        scope.launch {
            try {
                // The agent is saved; only its reload failed.
                if (reloadFailed && !dirty && !creating) {
                    commit(edited, emptyList())
                    return@launch
                }
                val preview =
                    if (creating) null
                    else
                        vm.api.previewAgent(
                            wireJson.decodeFromJsonElement<ReplaceAgentInput>(
                                agentSaveBody(edited, mcpIds = mcpIds)
                            )
                        )
                var lines =
                    agentReview(
                        baseline,
                        edited,
                        resources,
                        preview,
                    ) +
                        pluginGrantReview(
                            savedGrants,
                            grants,
                            catalog,
                            if (savedGrants.key() == grants.key()) emptyMap()
                            else resolveGrantReviewPaths(vm.api, catalog, grants),
                        )
                if (lines.any { it.kind == ReviewKind.Power || it.kind == ReviewKind.Access }) {
                    if (!creating) {
                        people.load(vm, agentId)
                        if (people.error != null || (!people.loaded && !people.outdated))
                            error(
                                people.error
                                    ?: "Impossible de charger les personnes ayant accès à cet agent."
                            )
                    }
                    val audience =
                        agentReviewAudience(
                            state.session.account?.email.orEmpty(),
                            people.rows,
                            everyMemberHasAccess = people.outdated,
                        )
                    // Plugin grant changes are access too, so the audience line follows them.
                    lines = lines + audienceReviewLine(audience)
                }
                if (needsReview(lines, preview)) {
                    val suspended = preview?.suspendedDelegations().orEmpty()
                    reapprove = preview?.preselectedReapprovals().orEmpty()
                    review =
                        PendingReview(
                            edited,
                            lines,
                            suspended,
                            edited.nativeReviewNodes(resources),
                        )
                } else commit(edited, emptyList())
            } catch (e: Exception) {
                fail(e)
            } finally {
                saving = false
            }
        }
    }
    fun reload() {
        val keepGrantEdits = grantChanges
        saving = true
        scope.launch {
            var attemptedFreshGrantRead = false
            try {
                val base = baseline
                val theirs =
                    vm.api.listAgents(build.cairn.contracts.ListAgentsInput()).agents.find {
                        it.id == agentId
                    } ?: error("Cet agent n’existe plus.")
                // Tool choices of connections deleted meanwhile cannot be saved: read the
                // connections again first.
                val refreshed = runCatching { vm.refresh() }.isSuccess
                val fresh = vm.state.value
                val freshMcps =
                    fresh.mcps.map { it.id }.toSet().takeIf { refreshed && fresh.isOwner }
                // Supplier merging depends on the current instance-to-package mapping. Do not
                // guess when a concurrently added instance is missing from the stale catalog.
                val freshCatalog =
                    if (keepGrantEdits) vm.api.get<PluginCatalog>("/plugins") else null
                attemptedFreshGrantRead = true
                val freshGrants = vm.api.getAgentPluginGrants(AgentInput(agentId))
                if (freshGrants.revision != theirs.revision)
                    throw ApiException(409, "L’agent a changé pendant le rechargement. Réessayez.")

                // Do not rebase onto a profile revision until its matching grants are known too.
                val rebasedDraft = draft.rebased(base, theirs, freshMcps)
                val keepGrantEdits = grantChanges
                val baseGrants = savedGrants
                val localGrants = grants
                val refreshedGrantsJson =
                    wireJson.encodeToString(PluginGrantDraft.serializer(), freshGrants.draft())
                val rebasedGrantsJson =
                    if (keepGrantEdits) {
                        val packageByInstance =
                            freshCatalog!!.connections.associate { it.id to it.pluginId }
                        wireJson.encodeToString(
                            PluginGrantDraft.serializer(),
                            mergePluginGrantDrafts(
                                baseGrants,
                                localGrants,
                                freshGrants.draft(),
                                packageByInstance,
                            ),
                        )
                    } else {
                        refreshedGrantsJson
                    }
                baseline = theirs
                draft = rebasedDraft
                savedGrantsJson = refreshedGrantsJson
                grantsJson = rebasedGrantsJson
                grantsLoaded = true
                grantsError = null
                if (freshCatalog != null) catalog = freshCatalog
                // Machines and plugin instances may have changed elsewhere too; until read again
                // they are unknown.
                nodesAttempt++
                if (!keepGrantEdits) catalogAttempt++
                saveError = null
                conflict = false
            } catch (e: Exception) {
                if (e is CancellationException) throw e
                // A failed refresh leaves both the draft's base and the current grant state
                // unknown. A catalog failure leaves the local grant draft intact and retryable.
                conflict = true
                if (attemptedFreshGrantRead || keepGrantEdits) grantsLoaded = false
                grantsError = e.message
                errorSection = null
                saveError = e.message ?: "Impossible de confirmer les droits actuels. Réessayez."
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
                if (!creating && current == null)
                    TextButton(onClick = { navigate("rules?agent=${segment(agentId)}") }) {
                        Text("Règles utilisant cet agent ›")
                    }
                when (current) {
                    null ->
                        AgentOverview(
                            displayDraft,
                            resources,
                            creating,
                            issues,
                            errors.keys + listOfNotNull(errorSection),
                            changes.toSet(),
                            state.modelCatalog("codex").models,
                            people,
                            grantsLoaded,
                            grantsError,
                        ) {
                            section = it.name
                        }
                    AgentSection.Projects ->
                        if (main) LockedSection()
                        else
                            ProjectsSection(
                                draft.agent.access,
                                state.projects,
                                chooseAll = { all ->
                                    draft = draft.withAllProjects(all)
                                    saveError = null
                                    conflict = false
                                },
                            ) { change ->
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
                    AgentSection.Plugins ->
                        if (catalog == null || !grantsLoaded) {
                            Text(
                                grantsError
                                    ?: if (catalogFailed) "Impossible de charger les plugins."
                                    else "Chargement des droits…"
                            )
                            val held =
                                if (grantsLoaded) grants.grants.map { it.instanceId }
                                else draft.agent.plugins
                            if (held.isNotEmpty())
                                Text(
                                    if (held.size == 1) "1 instance accordée :"
                                    else "${held.size} instances accordées :"
                                )
                            held.forEach { Text(it) }
                            TextButton({
                                catalogAttempt++
                                grantsAttempt++
                            }) {
                                Text("Réessayer")
                            }
                        } else {
                            PluginGrantEditor(
                                vm.api,
                                catalog!!,
                                grants,
                                ::editGrants,
                                agentId,
                                navigate,
                                enabled = !saving,
                            )
                        }
                    AgentSection.Nodes ->
                        NodesSection(
                            draft.agent,
                            nodes,
                            nodesLoaded,
                            nodesFailed,
                            retry = { nodesAttempt++ },
                            change = ::update,
                        )
                    AgentSection.Sandbox ->
                        SandboxSection(draft.agent.access.sandbox, draft.agent.access.backend) {
                            value ->
                            access { it.copy(sandbox = value) }
                        }
                    AgentSection.Powers ->
                        PowersSection(
                            draft.agent,
                            nodes,
                            nodesLoaded,
                            { section = it.name },
                            ::access,
                        )
                    AgentSection.Identity ->
                        IdentitySection(vm, state, baseline, draft.agent, creating, ::update)
                    AgentSection.Engine ->
                        EngineSection(
                            vm,
                            state,
                            draft.agent,
                            ::update,
                            !creating && !dirty,
                            saved = baseline.takeUnless { creating && creation.id.isBlank() },
                            openConnections = { navigate("connections") },
                        ) {
                            navigate(
                                "agents/${draft.agent.id}/configuration/${draft.agent.provider}"
                            )
                        }
                    AgentSection.Advanced ->
                        AdvancedSection(draft.timeout) {
                            draft = draft.copy(timeout = it)
                            saveError = null
                        }
                    AgentSection.People ->
                        PeopleSection(
                            vm,
                            state,
                            people,
                            agentId,
                            baseline.name,
                            creating,
                            // Access applies at once to the saved agent: warn for either state.
                            baseline.access.installation || draft.agent.access.installation,
                            powerColors(),
                            scope,
                            retry = { peopleAttempt++ },
                        )
                }
            }
        }
        // Kept while saving: the save ends once the workspace has reloaded.
        if (dirty || creating || saving || saveError != null)
            SaveBar(
                changes.size,
                creating,
                saving,
                errors,
                saveError.takeIf { errorSection == null || errorSection != current },
                errorSection,
                conflict,
                retryable = reloadFailed,
                discard = {
                    if (creating) discarding = true
                    else {
                        draft = AgentDraft(baseline)
                        grantsJson = savedGrantsJson
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
                    commit(pending.edited, reapprove.toList(), pending.nativeNodes)
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
                        creating && baseline.id.isBlank() -> "Cet agent ne sera pas créé."
                        changes.isEmpty() -> "Les modifications non enregistrées seront perdues."
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
            "Les règles qui utilisent cet agent perdront leurs droits.",
            state.busy,
            state.error,
            { deleting = false },
        ) {
            vm.perform {
                api.deleteAgent(AgentInput(agentId))
                refresh()
                notify("Suppression effectuée")
                deleting = false
                back()
            }
        }
}

package build.cairn.app.ui

import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import build.cairn.app.data.*
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Job
import kotlinx.coroutines.launch

/** Saved granular rights remain independent of the agent's instance-wide draft. */
@Composable
internal fun AgentResourceAccess(
    vm: CairnViewModel,
    state: Workspace,
    agentId: String,
    catalog: PluginCatalog,
    changed: () -> Unit,
) =
    key(agentId) {
        var rules by remember { mutableStateOf(emptyList<AgentResourceGrant>()) }
        var next by remember { mutableStateOf<String?>(null) }
        var ready by remember { mutableStateOf(false) }
        var busy by remember { mutableStateOf(false) }
        var writing by remember { mutableStateOf(false) }
        var error by remember { mutableStateOf<String?>(null) }
        var removing by remember { mutableStateOf<AgentResourceGrant?>(null) }
        var request by remember { mutableStateOf<Job?>(null) }
        var generation by remember { mutableIntStateOf(0) }
        var instance by rememberForm("", agentId)
        var collection by rememberForm("", agentId)
        var selected by rememberForm("", agentId)
        var resources by remember { mutableStateOf(emptyList<PluginResource>()) }
        var resourcesNext by remember { mutableStateOf<String?>(null) }
        var resourcesBusy by remember { mutableStateOf(false) }
        var resourceError by remember { mutableStateOf<String?>(null) }
        var resourceRequest by remember { mutableStateOf<Job?>(null) }
        var resourceGeneration by remember { mutableIntStateOf(0) }
        var editorActive by remember { mutableStateOf(false) }
        val scope = rememberCoroutineScope()
        val path = "/agents/${segment(agentId)}/plugin-resource-grants"
        val candidates =
            catalog.connections.filter { connection ->
                catalog.packages.any {
                    it.id == connection.pluginId &&
                        it.active != null &&
                        it.manifest?.operations?.isNotEmpty() == true
                }
            }
        val instanceNameCounts = candidates.groupingBy { it.name }.eachCount()
        val instanceGroupCounts = candidates.groupingBy { it.pluginId to it.name }.eachCount()
        val packagesById = catalog.packages.associateBy { it.id }
        val connection = candidates.find { it.id == instance }
        val manifest = catalog.packages.find { it.id == connection?.pluginId }?.manifest
        val collections =
            manifest?.resources.orEmpty().filter { item ->
                manifest?.operations.orEmpty().any { it.collection == item.name }
            }
        val resourcePath =
            connection
                ?.takeIf { collection.isNotBlank() }
                ?.let {
                    "/plugins/${segment(it.pluginId)}/connections/${segment(it.id)}/resources/${segment(collection)}"
                }
        val resource = resources.find { it.id == selected }
        fun load(more: Boolean = false) {
            if (writing || (more && next == null)) return
            request?.cancel()
            val current = ++generation
            val cursor = if (more) "?cursor=${segment(next!!)}" else ""
            busy = true
            ready = false
            error = null
            request = scope.launch {
                try {
                    val page = vm.api.get<PluginPage<AgentResourceGrant>>(path + cursor)
                    if (generation == current) {
                        rules =
                            if (more)
                                rules.filter { row -> page.items.none { it.id == row.id } } +
                                    page.items
                            else page.items
                        next = page.nextCursor
                        ready = true
                        changed()
                    }
                } catch (e: Exception) {
                    if (e is CancellationException) throw e
                    if (generation == current) error = e.message
                } finally {
                    if (generation == current) busy = false
                }
            }
        }
        fun clearResources() {
            resourceGeneration++
            resourceRequest?.cancel()
            resources = emptyList()
            resourcesNext = null
            resourcesBusy = false
            resourceError = null
        }
        fun pageResources(more: Boolean = false) {
            val target = resourcePath ?: return
            if (writing || (more && resourcesNext == null)) return
            resourceRequest?.cancel()
            val current = ++resourceGeneration
            val cursor = if (more) "?cursor=${segment(resourcesNext!!)}" else ""
            resourcesBusy = true
            resourceError = null
            resourceRequest = scope.launch {
                try {
                    val page = vm.api.get<PluginPage<PluginResource>>(target + cursor)
                    if (resourceGeneration == current) {
                        resources =
                            if (more)
                                resources.filter { row -> page.items.none { it.id == row.id } } +
                                    page.items
                            else page.items
                        resourcesNext = page.nextCursor
                    }
                } catch (e: Exception) {
                    if (e is CancellationException) throw e
                    if (resourceGeneration == current) resourceError = e.message
                } finally {
                    if (resourceGeneration == current) resourcesBusy = false
                }
            }
        }
        fun revoke(rule: AgentResourceGrant) {
            if (!ready || busy || writing) return
            writing = true
            ready = false
            error = null
            scope.launch {
                try {
                    vm.api.request("DELETE", "$path/${segment(rule.id)}")
                    removing = null
                    writing = false
                    load()
                } catch (e: Exception) {
                    if (e is CancellationException) throw e
                    error = "${e.message} Actualisez les règles avant toute nouvelle modification."
                    removing = null
                } finally {
                    writing = false
                }
            }
        }
        LaunchedEffect(agentId) { load() }
        LaunchedEffect(resourcePath) {
            clearResources()
            if (resourcePath != null) pageResources()
        }
        Column(verticalArrangement = Arrangement.spacedBy(10.dp)) {
            Text("Droits sur les ressources", style = MaterialTheme.typography.titleMedium)
            Text(
                "Ces règles enregistrées sont distinctes des accès globaux. Les profils, exceptions et limites restent propres à chaque règle. Leur usage dépend aussi du plugin, de la ressource et du demandeur. Les changements ici s’enregistrent immédiatement.",
                style = MaterialTheme.typography.bodySmall,
            )
            error?.let { Text(it, color = MaterialTheme.colorScheme.error) }
            SignalButton(
                if (busy) "Chargement des droits…" else "Actualiser les droits sur les ressources",
                enabled = !busy && !writing && !editorActive,
            ) {
                load()
            }
            if (ready && rules.isEmpty()) Text("Aucune règle enregistrée sur les ressources.")
            rules.forEach { rule ->
                Card {
                    Column(
                        Modifier.padding(12.dp),
                        verticalArrangement = Arrangement.spacedBy(8.dp),
                    ) {
                        Text(
                            "${rule.instanceName ?: "Instance supprimée"} · ${rule.resourceName ?: "Ressource supprimée"}"
                        )
                        Text(rule.profile?.label ?: "Opérations choisies")
                        Text(
                            "Autorisées explicitement : ${rule.operations.joinToString().ifBlank { "aucune" }}"
                        )
                        Text(
                            "Refusées dans cette règle : ${rule.deniedOperations.joinToString().ifBlank { "aucune" }}"
                        )
                        rule.scopes.forEach { (name, values) ->
                            Text("$name : ${values.joinToString()}")
                        }
                        if (rule.scopes.isEmpty())
                            Text("Aucune limite de périmètre supplémentaire.")
                        SignalButton(
                            "Révoquer le droit sur la ressource",
                            enabled = ready && !busy && !writing && !editorActive,
                        ) {
                            removing = rule
                        }
                        if (removing?.id == rule.id) {
                            Text(
                                "Révoquer cette règle ? Les autres règles conservent leurs droits."
                            )
                            SignalButton("Annuler la révocation", enabled = !writing) {
                                removing = null
                            }
                            SignalButton("Confirmer la révocation", enabled = !writing) {
                                revoke(rule)
                            }
                        }
                    }
                }
            }
            if (next != null) {
                Text("D’autres règles enregistrées sont disponibles.")
                SignalButton(
                    "Plus de droits sur les ressources",
                    enabled = !busy && !writing && !editorActive,
                ) {
                    load(true)
                }
            }
            Choice(
                "Instance pour les droits sur les ressources",
                instance,
                candidates.map { candidate ->
                    val label =
                        if (instanceNameCounts.getValue(candidate.name) == 1) candidate.name
                        else {
                            val plugin = packagesById[candidate.pluginId]
                            val repeatedInPackage =
                                instanceGroupCounts.getValue(candidate.pluginId to candidate.name) >
                                    1
                            val identity =
                                if (repeatedInPackage) "${candidate.name} · ${candidate.id}"
                                else candidate.name
                            "$identity · ${candidate.pluginId} (${plugin?.name ?: candidate.pluginId})"
                        }
                    candidate.id to label
                },
                enabled = !busy && !writing && !editorActive,
            ) {
                if (!busy && !writing && !editorActive) {
                    clearResources()
                    instance = it
                    collection = ""
                    selected = ""
                }
            }
            if (instance.isNotBlank())
                Choice(
                    "Collection de ressources",
                    collection,
                    collections.map { it.name to it.title },
                    enabled = !busy && !writing && !editorActive,
                ) {
                    if (!busy && !writing && !editorActive) {
                        clearResources()
                        collection = it
                        selected = ""
                    }
                }
            resourceError?.let { Text(it, color = MaterialTheme.colorScheme.error) }
            if (resourcePath != null) {
                SignalButton(
                    "Actualiser les ressources disponibles",
                    enabled = !resourcesBusy && !writing && !editorActive,
                ) {
                    pageResources()
                }
                Choice(
                    "Ressource pour les droits de l’agent",
                    selected,
                    resources.map { it.id to it.name },
                    enabled = !resourcesBusy && !writing && !editorActive,
                ) {
                    if (!resourcesBusy && !writing && !editorActive) selected = it
                }
            }
            if (selected.isNotBlank() && resource == null)
                Text(
                    "Ressource sélectionnée : $selected. Chargez sa page avant de modifier ses droits."
                )
            if (resourcesNext != null) {
                Text("La liste des ressources contient d’autres pages.")
                SignalButton(
                    "Plus de ressources disponibles",
                    enabled = !resourcesBusy && !writing && !editorActive,
                ) {
                    pageResources(true)
                }
            }
            if (resource != null && resourcePath != null)
                key(resourcePath, resource.id) {
                    PluginResourceGrants(
                        vm,
                        state,
                        "$resourcePath/${segment(resource.id)}/grants",
                        resource.name,
                        manifest?.operations.orEmpty().filter { it.collection == collection },
                        enabled = !writing,
                        fixedAgent = agentId,
                        activity = { active ->
                            editorActive = active
                            if (!active) load()
                        },
                    )
                }
        }
    }

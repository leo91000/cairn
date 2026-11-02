package build.cairn.app.ui

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import build.cairn.app.data.*
import build.cairn.contracts.ListPluginResourcesInput
import build.cairn.contracts.PluginGrantAccess
import build.cairn.contracts.PluginInstanceGrant
import build.cairn.contracts.PluginResource
import build.cairn.contracts.PluginResourceRef
import build.cairn.contracts.PluginResourceType
import build.cairn.contracts.ResolvePluginResourceInput
import build.cairn.contracts.listPluginResources
import build.cairn.contracts.resolvePluginResource
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.async
import kotlinx.coroutines.awaitAll
import kotlinx.coroutines.coroutineScope

internal const val MAX_PLUGIN_RESOURCE_ROOTS_PER_CONTRIBUTION = 100

internal fun canSelectPluginResourceRoot(selectedCount: Int, alreadySelected: Boolean): Boolean =
    alreadySelected || selectedCount < MAX_PLUGIN_RESOURCE_ROOTS_PER_CONTRIBUTION

@Composable
fun PluginGrantEditor(
    api: CairnApi,
    catalog: PluginCatalog,
    draft: PluginGrantDraft,
    change: (PluginGrantDraft) -> Unit,
    agentId: String,
    navigate: (String) -> Unit,
) {
    var query by rememberSaveable { mutableStateOf("") }
    OutlinedTextField(
        query,
        { query = it },
        label = { Text("Rechercher une instance ou un plugin") },
        modifier = Modifier.fillMaxWidth(),
    )
    val groups = catalog.agentGrantable()
    val delegations = catalog.connections.filter { agentId.isNotBlank() && it.agentId == agentId }
    val unavailable = catalog.unavailableGrants(draft.grants.map { it.instanceId })
    val missing =
        draft.grants.map { it.instanceId }.filter { id -> catalog.connections.none { it.id == id } }

    groups.forEach { (entry, connections) ->
        val manifest = entry.manifest ?: return@forEach
        connections
            .filter { (it.name + " " + entry.name).contains(query, ignoreCase = true) }
            .forEach { connection ->
                key(connection.id) {
                    val grant = draft.grants.find { it.instanceId == connection.id }
                    var selectedMode by rememberSaveable {
                        mutableStateOf(grant?.access is PluginGrantAccess.B)
                    }
                    val mode =
                        when (grant?.access) {
                            is PluginGrantAccess.A -> "full"
                            is PluginGrantAccess.B -> "selected"
                            null -> if (selectedMode) "selected" else "none"
                        }
                    OutlinedCard(
                        Modifier.fillMaxWidth().testTag("agent-plugin-instance-${connection.id}")
                    ) {
                        Column(
                            Modifier.padding(16.dp),
                            verticalArrangement = Arrangement.spacedBy(12.dp),
                        ) {
                            Text(
                                if (entry.id == "onepassword") "1Password · comptes de service"
                                else entry.name,
                                style = MaterialTheme.typography.titleMedium,
                            )
                            Row(verticalAlignment = Alignment.CenterVertically) {
                                Switch(
                                    grant != null,
                                    { on ->
                                        selectedMode = false
                                        val next = draft.without(connection.id)
                                        change(
                                            if (on)
                                                next.copy(
                                                    grants =
                                                        next.grants +
                                                            PluginInstanceGrant(
                                                                PluginGrantAccess.A(),
                                                                connection.id,
                                                            )
                                                )
                                            else next
                                        )
                                    },
                                    modifier =
                                        Modifier.semantics {
                                            contentDescription = "Accès à ${connection.name}"
                                        },
                                )
                                Text(connection.name, Modifier.weight(1f))
                            }
                            if (entry.id == "onepassword")
                                Text(
                                    "Lecture seule · aucun accès par défaut, Main compris",
                                    style = MaterialTheme.typography.bodySmall,
                                )
                            manifest.agentAccess
                                ?.description
                                ?.takeIf { it.isNotBlank() }
                                ?.let { Text(it, style = MaterialTheme.typography.bodySmall) }
                            Row(verticalAlignment = Alignment.CenterVertically) {
                                ToneChip(connection.state())
                                Spacer(Modifier.weight(1f))
                                TextButton(onClick = { navigate("plugins/${segment(entry.id)}") }) {
                                    Text(
                                        if (entry.id == "onepassword") "Configurer le compte ↗"
                                        else "Configurer ↗"
                                    )
                                }
                            }
                            if (!connection.enabled)
                                Text(
                                    if (entry.id == "onepassword") "Compte désactivé"
                                    else
                                        "Instance désactivée : les droits sont conservés mais inutilisables."
                                )
                            listOf(
                                    "none" to "Aucun accès",
                                    "full" to "Accès complet",
                                    "selected" to "Choisir les contributions",
                                )
                                .forEach { (value, label) ->
                                    Row(verticalAlignment = Alignment.CenterVertically) {
                                        RadioButton(
                                            mode == value,
                                            onClick = {
                                                selectedMode = value == "selected"
                                                val next = draft.without(connection.id)
                                                change(
                                                    if (value == "full")
                                                        next.copy(
                                                            grants =
                                                                next.grants +
                                                                    PluginInstanceGrant(
                                                                        PluginGrantAccess.A(),
                                                                        connection.id,
                                                                    )
                                                        )
                                                    else next
                                                )
                                            },
                                            modifier =
                                                Modifier.semantics {
                                                    contentDescription =
                                                        "Accès à ${connection.name} : $value"
                                                },
                                        )
                                        Text(label)
                                    }
                                }
                            if (mode == "full")
                                Text(
                                    "Toutes les contributions actuelles et futures. Les identifiants exigent le choix explicite ci-dessous. Les étapes partagées de configuration exigent l’accès complet."
                                )
                            if (mode == "selected") {
                                Text(
                                    "Seules les contributions cochées sont accordées. Les nouveautés restent désactivées. Rien ne change avant l’enregistrement."
                                )
                                manifest.grantContributions().forEach { contribution ->
                                    val item = contribution.grant
                                    val held =
                                        (grant?.access as? PluginGrantAccess.B)
                                            ?.contributions
                                            ?.find { it.kind == item.kind && it.name == item.name }
                                    fun update(
                                        on: Boolean,
                                        resources: List<PluginResourceRef> = item.resources,
                                    ) {
                                        val items =
                                            (grant?.access as? PluginGrantAccess.B)
                                                ?.contributions
                                                .orEmpty()
                                                .filterNot {
                                                    it.kind == item.kind && it.name == item.name
                                                } +
                                                if (on) listOf(item.copy(resources = resources))
                                                else emptyList()
                                        var next = draft.without(connection.id)
                                        if (items.isNotEmpty())
                                            next =
                                                next.copy(
                                                    grants =
                                                        next.grants +
                                                            PluginInstanceGrant(
                                                                PluginGrantAccess.B(items),
                                                                connection.id,
                                                            )
                                                )
                                        if (
                                            (connection.id in draft.runCredentialInstances ||
                                                on && item.kind == "runCredentials") &&
                                                items.any { it.kind == "runCredentials" }
                                        )
                                            next =
                                                next.copy(
                                                    runCredentialInstances =
                                                        next.runCredentialInstances + connection.id
                                                )
                                        if (on && item.kind == "runCredentials")
                                            next =
                                                next.chooseSupplier(
                                                    connections.map { it.id },
                                                    connection.id,
                                                )
                                        change(next)
                                    }
                                    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                                        Row(verticalAlignment = Alignment.CenterVertically) {
                                            Checkbox(
                                                held != null,
                                                { update(it) },
                                                modifier =
                                                    Modifier.semantics {
                                                        contentDescription =
                                                            "${item.kind} ${item.name}"
                                                    },
                                            )
                                            Column(Modifier.weight(1f)) {
                                                Text(
                                                    "${item.kind} ${item.name}",
                                                    style = MaterialTheme.typography.labelLarge,
                                                )
                                                Text(
                                                    contribution.description,
                                                    style = MaterialTheme.typography.bodySmall,
                                                )
                                            }
                                        }
                                        if (
                                            held != null &&
                                                item.kind in listOf("tool", "command", "hook") &&
                                                contribution.resourceTypes.isNotEmpty()
                                        ) {
                                            ResourceGrantPicker(
                                                api,
                                                entry.id,
                                                connection.id,
                                                contribution.resourceTypes,
                                                held.resources,
                                            ) {
                                                update(true, it)
                                            }
                                        } else if (held != null)
                                            Text(
                                                if (item.kind == "condition")
                                                    "Sans restriction de ressource. L’attente reste limitée par l’accès aux projets."
                                                else "Sans restriction de ressource.",
                                                style = MaterialTheme.typography.bodySmall,
                                            )
                                    }
                                }
                            }
                            connection.status.message
                                ?.takeIf { it.isNotBlank() }
                                ?.let {
                                    Text(it, style = MaterialTheme.typography.bodySmall)
                                }
                            if (manifest.runCredentials != null) {
                                val supplier =
                                    catalog.connections.find {
                                        it.pluginId == entry.id &&
                                            it.id in draft.runCredentialInstances
                                    }
                                Text(
                                    "Les identifiants de run donnent tout le pouvoir du compte de service et rendent vaines les restrictions de ressource des outils. Une seule instance par plugin les fournit. Fournisseur actuel : ${supplier?.name ?: "Aucun"}.",
                                    color = MaterialTheme.colorScheme.error,
                                )
                                if (mode == "full")
                                    Row(verticalAlignment = Alignment.CenterVertically) {
                                        Checkbox(
                                            connection.id in draft.runCredentialInstances,
                                            { on ->
                                                change(
                                                    draft.chooseSupplier(
                                                        connections.map { it.id },
                                                        if (on) connection.id else null,
                                                    )
                                                )
                                            },
                                            modifier =
                                                Modifier.semantics {
                                                    contentDescription =
                                                        "Fournir les identifiants depuis ${connection.name}"
                                                },
                                        )
                                        Text(
                                            "Fournir les identifiants depuis ${connection.name}",
                                            Modifier.weight(1f),
                                        )
                                    }
                                Text(
                                    "Retirer ce fournisseur laisse l’agent sans identifiants jusqu’à un nouveau choix. L’accès complet aux autres instances n’en fournit pas.",
                                    style = MaterialTheme.typography.bodySmall,
                                )
                            }
                            if (entry.id == "onepassword")
                                Note(
                                    "Avec un compte accordé, l’agent peut lister les coffres, les éléments et les noms de champs, puis lire une valeur avec cairn plugin onepassword read. Il ne peut ni créer, ni modifier, ni supprimer. Ce que le compte voit se règle dans 1Password : créez un autre compte de service pour d’autres droits. Retirer l’accès bloque immédiatement les lectures suivantes ; un secret déjà lu ne peut pas être rappelé."
                                )
                        }
                    }
                }
            }
    }
    unavailable.forEach { connection ->
        val packageName = catalog.packages.find { it.id == connection.pluginId }?.name
        Column(Modifier.fillMaxWidth().testTag("agent-plugin-unavailable-${connection.id}")) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(
                    connection.name + (packageName?.let { " · $it" } ?: ""),
                    Modifier.weight(1f),
                    style = MaterialTheme.typography.titleSmall,
                )
                ToneChip(PluginBadge("Indisponible", PluginTone.ATTENTION))
                TextButton(
                    { change(draft.without(connection.id)) },
                    modifier =
                        Modifier.semantics { contentDescription = "Retirer ${connection.name}" },
                ) {
                    Text("Retirer")
                }
            }
            Note(
                "Ce plugin ne donne plus d’accès aux agents (version non approuvée, invalide ou sans accès agent). L’accès est conservé, mais ne donne rien tant qu’il ne redevient pas disponible."
            )
        }
    }
    missing.forEach { id ->
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text("Instance supprimée", Modifier.weight(1f))
            ToneChip(PluginBadge("Introuvable", PluginTone.ATTENTION))
            TextButton(
                { change(draft.without(id)) },
                modifier = Modifier.semantics { contentDescription = "Retirer l’instance $id" },
            ) {
                Text("Retirer")
            }
        }
    }
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
    if (groups.isEmpty() && delegations.isEmpty() && unavailable.isEmpty()) {
        Empty(
            "Aucun plugin ne donne d’accès aux agents",
            "Ajoutez un compte de service 1Password ou une instance de plugin pour l’accorder ici.",
        )
        TextButton(onClick = { navigate("plugins") }) { Text("Ouvrir les plugins ›") }
    }
}

@Composable
private fun ResourceGrantPicker(
    api: CairnApi,
    plugin: String,
    instance: String,
    types: List<PluginResourceType>,
    selected: List<PluginResourceRef>,
    change: (List<PluginResourceRef>) -> Unit,
) {
    var open by rememberSaveable { mutableStateOf(false) }
    var query by rememberSaveable { mutableStateOf("") }
    var type by rememberSaveable { mutableStateOf(types.first().name) }
    var page by remember { mutableStateOf(1) }
    var nextPage by remember { mutableStateOf<Int?>(null) }
    var attempt by remember { mutableIntStateOf(0) }
    var items by remember { mutableStateOf(emptyList<Pair<PluginResource, String>>()) }
    var loading by remember { mutableStateOf(false) }
    var error by remember { mutableStateOf<String?>(null) }
    Text(
        if (selected.isEmpty())
            "Toutes les ressources. Choisissez une ressource pour restreindre cette contribution."
        else
            "Racines et descendants sélectionnés uniquement. Chaque type restreint doit correspondre ; les autres types restent libres.",
        style = MaterialTheme.typography.bodySmall,
    )
    Text(
        "${selected.size}/$MAX_PLUGIN_RESOURCE_ROOTS_PER_CONTRIBUTION radicaux sélectionnés pour cette contribution. Maximum 100 ; retirez-en un pour en choisir un autre.",
        Modifier.testTag("plugin-resource-root-count"),
        style = MaterialTheme.typography.bodySmall,
    )
    selected.forEach { item ->
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text(
                "${item.resourceType} · ${item.id} et descendants",
                Modifier.weight(1f),
                style = MaterialTheme.typography.bodySmall,
            )
            TextButton({ change(selected - item) }) { Text("Retirer") }
        }
    }
    OutlinedButton({ open = true }) { Text("Choisir les ressources") }
    if (!open) return
    LaunchedEffect(type, query) {
        page = 1
        nextPage = null
        items = emptyList()
    }
    LaunchedEffect(plugin, instance, type, query, page, attempt) {
        loading = true
        error = null
        try {
            val result =
                api.listPluginResources(
                    ListPluginResourcesInput(instance, page, plugin, query, type)
                )
            val resolved = coroutineScope {
                result.items
                    .map { item ->
                        async {
                            val ancestry =
                                api.resolvePluginResource(
                                    ResolvePluginResourceInput(
                                        item.id,
                                        instance,
                                        plugin,
                                        item.resourceType,
                                    )
                                )
                            item to
                                (ancestry.ancestors.map { it.name } + item.name).joinToString(" › ")
                        }
                    }
                    .awaitAll()
            }
            items =
                if (page == 1) resolved
                else (items + resolved).distinctBy { it.first.resourceType to it.first.id }
            nextPage = result.nextPage
        } catch (e: Exception) {
            if (e is CancellationException) throw e
            error = e.message
        } finally {
            loading = false
        }
    }
    AlertDialog(
        onDismissRequest = { open = false },
        title = { Text("Ressources autorisées") },
        text = {
            Column(
                Modifier.verticalScroll(rememberScrollState()),
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                types.forEach { resourceType ->
                    val names = mutableListOf(resourceType.name)
                    var parent = resourceType.parentType
                    while (parent != null && parent !in names) {
                        names.add(0, parent)
                        parent = types.find { it.name == parent }?.parentType
                    }
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        RadioButton(
                            type == resourceType.name,
                            { type = resourceType.name },
                            modifier =
                                Modifier.semantics {
                                    contentDescription = "Type de ressource ${resourceType.name}"
                                },
                        )
                        Text(names.joinToString(" › "))
                    }
                }
                OutlinedTextField(
                    query,
                    { query = it },
                    label = { Text("Rechercher des ressources") },
                    modifier =
                        Modifier.semantics {
                            contentDescription = "Rechercher des ressources"
                        },
                )
                error?.let {
                    Text(it, color = MaterialTheme.colorScheme.error)
                    TextButton({ attempt++ }) { Text("Réessayer") }
                }
                items.forEach { (item, path) ->
                    val ref = PluginResourceRef(item.id, item.resourceType)
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Checkbox(
                            ref in selected,
                            { checked ->
                                if (canSelectPluginResourceRoot(selected.size, ref in selected))
                                    change(if (checked) selected + ref else selected - ref)
                            },
                            modifier = Modifier.semantics { contentDescription = path },
                            enabled = canSelectPluginResourceRoot(selected.size, ref in selected),
                        )
                        Text(path, Modifier.weight(1f))
                    }
                }
                if (loading) LinearProgressIndicator(Modifier.fillMaxWidth())
                else if (items.isEmpty() && error == null) Text("Aucune ressource trouvée")
                nextPage
                    ?.takeIf { error == null }
                    ?.let { availablePage ->
                        TextButton({ page = availablePage }, enabled = !loading) {
                            Text("Charger plus de ressources")
                        }
                    }
            }
        },
        confirmButton = { TextButton({ open = false }) { Text("Terminé") } },
    )
}

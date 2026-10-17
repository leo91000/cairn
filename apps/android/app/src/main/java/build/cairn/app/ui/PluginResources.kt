package build.cairn.app.ui

import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import build.cairn.app.data.*
import kotlinx.serialization.json.*

/** Selects a management namespace explicitly; a setup never resolves a live instance. */
internal data class PluginResourceLocation(
    val plugin: String,
    val instance: String,
    val setupRevision: String? = null,
) {
    val path: String
        get() =
            "/plugins/${segment(plugin)}/${if (setupRevision == null) "connections" else "setup-drafts"}/${segment(instance)}/resources"
}

@Composable
internal fun PluginResources(
    vm: CairnViewModel,
    state: Workspace,
    manifest: PluginManifest,
    location: PluginResourceLocation,
    enabled: Boolean = true,
) {
    var pages by
        remember(location.plugin, location.instance) {
            mutableStateOf(mapOf<String, PluginPage<PluginResource>>())
        }
    var editing by
        remember(location.plugin, location.instance) {
            mutableStateOf<Pair<PluginResourceCollection, PluginResource?>?>(null)
        }
    var removing by
        remember(location.plugin, location.instance) { mutableStateOf<PluginResource?>(null) }
    suspend fun load() {
        pages =
            manifest.resources.associate { collection ->
                collection.name to
                    vm.api.get<PluginPage<PluginResource>>(
                        "${location.path}/${segment(collection.name)}"
                    )
            }
    }
    suspend fun more(collection: String) {
        val page = pages[collection] ?: return
        val cursor = page.nextCursor ?: return
        val next =
            vm.api.get<PluginPage<PluginResource>>(
                "${location.path}/${segment(collection)}?cursor=${segment(cursor)}"
            )
        pages = pages + (collection to PluginPage(page.items + next.items, next.nextCursor))
    }
    LaunchedEffect(location) {
        try {
            load()
        } catch (e: Exception) {
            vm.report(e)
        }
    }
    Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
        TextButton(onClick = { vm.perform { load() } }, enabled = !state.busy) {
            Text("Actualiser les ressources")
        }
        manifest.resources.forEach { collection ->
            Text(collection.title, style = MaterialTheme.typography.titleSmall)
            pages[collection.name]?.items?.forEach { resource ->
                SignalCard(Modifier.fillMaxWidth()) {
                    Text(resource.name, style = MaterialTheme.typography.titleSmall)
                    Text(
                        if (location.setupRevision != null) "Préparé"
                        else if (resource.enabled) "Activé" else "En pause",
                        style = MaterialTheme.typography.bodySmall,
                    )
                    resource.parentId?.let { parent ->
                        Text(
                            pages[collection.parent]?.items?.find { it.id == parent }?.name
                                ?: "Ressource parente",
                            style = MaterialTheme.typography.bodySmall,
                        )
                    }
                    Row {
                        TextButton(
                            onClick = {
                                vm.perform {
                                    collection.parent?.let { parent ->
                                        while (pages[parent]?.nextCursor != null) more(parent)
                                    }
                                    editing = collection to resource
                                }
                            },
                            enabled = enabled && !state.busy,
                        ) {
                            Text("Modifier ${resource.name}")
                        }
                        TextButton(
                            onClick = { removing = resource },
                            enabled = enabled && !state.busy,
                        ) {
                            Text("Supprimer ${resource.name}")
                        }
                    }
                }
            }
            if (pages[collection.name]?.items?.isEmpty() == true)
                Text("Aucune ressource.", style = MaterialTheme.typography.bodySmall)
            if (pages[collection.name]?.nextCursor != null)
                TextButton(
                    onClick = { vm.perform { more(collection.name) } },
                    enabled = !state.busy,
                ) {
                    Text("Voir plus · ${collection.title}")
                }
            SignalButton(
                "Ajouter · ${collection.singular}",
                height = 40.dp,
                enabled = enabled && !state.busy,
            ) {
                vm.perform {
                    collection.parent?.let { parent ->
                        while (pages[parent]?.nextCursor != null) more(parent)
                    }
                    editing = collection to null
                }
            }
        }
    }
    editing?.let { (collection, initial) ->
        ResourceEditor(
            vm,
            state,
            location,
            collection,
            initial,
            pages[collection.parent]?.items.orEmpty(),
            close = { editing = null },
        ) {
            editing = null
            load()
        }
    }
    removing?.let { resource ->
        Confirm(
            "Supprimer ${resource.name} ?",
            "Ses sous-ressources, identifiants enregistrés et règles d’accès seront supprimés.",
            state.busy,
            state.error,
            dismiss = { removing = null },
        ) {
            vm.perform {
                api.request(
                    "DELETE",
                    "${location.path}/${segment(resource.collection)}/${segment(resource.id)}",
                    buildJsonObject {
                        put("revision", resource.revision)
                        location.setupRevision?.let { put("setupRevision", it) }
                    },
                )
                removing = null
                load()
            }
        }
    }
}

@Composable
private fun ResourceEditor(
    vm: CairnViewModel,
    state: Workspace,
    location: PluginResourceLocation,
    collection: PluginResourceCollection,
    initial: PluginResource?,
    parents: List<PluginResource>,
    close: () -> Unit,
    saved: suspend () -> Unit,
) {
    var name by remember { mutableStateOf(initial?.name.orEmpty()) }
    var parent by remember { mutableStateOf(initial?.parentId.orEmpty()) }
    var enabled by remember { mutableStateOf(initial?.enabled ?: true) }
    var values by remember {
        mutableStateOf(
            collection.settings
                .filter { it.type != "secret" }
                .associate { it.key to formValue(it, initial?.settings?.get(it.key)) }
        )
    }
    var secrets by remember { mutableStateOf(mapOf<String, String>()) }
    Editor(
        if (initial == null) "Nouveau · ${collection.singular}" else "Modifier ${initial.name}",
        state.busy,
        state.error,
        close,
        valid = name.isNotBlank() && (collection.parent == null || parent.isNotBlank()),
        save = {
            vm.perform {
                api.request(
                    if (initial == null) "POST" else "PUT",
                    "${location.path}/${segment(collection.name)}${initial?.let { "/${segment(it.id)}" }.orEmpty()}",
                    buildJsonObject {
                        put("name", name.trim())
                        put("settings", pluginSettings(collection.settings, values))
                        put(
                            "secrets",
                            JsonObject(
                                secrets
                                    .filterValues { it.isNotBlank() }
                                    .mapValues { JsonPrimitive(it.value.trim()) }
                            ),
                        )
                        put("enabled", enabled)
                        if (collection.parent != null) put("parentId", parent)
                        initial?.let { put("revision", it.revision) }
                        location.setupRevision?.let { put("setupRevision", it) }
                    },
                )
                secrets = emptyMap()
                saved()
            }
        },
    ) {
        Field("Nom · ${collection.singular}", name, { name = it }, enabled = !state.busy)
        if (collection.parent != null && !state.busy)
            Choice("Parent", parent, parents.map { it.id to it.name }) { parent = it }
        PluginFields(
            collection.settings,
            values,
            { key, value -> values = values + (key to value) },
            secrets,
            { key, value -> secrets = secrets + (key to value) },
            initial?.secretKeys.orEmpty(),
            enabled = !state.busy,
        )
        Toggle("Activé", enabled) { if (!state.busy) enabled = it }
    }
    DisposableEffect(Unit) { onDispose { secrets = emptyMap() } }
}

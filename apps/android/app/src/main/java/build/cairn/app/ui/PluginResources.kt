package build.cairn.app.ui

import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import build.cairn.app.data.*
import java.util.concurrent.atomic.AtomicInteger
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

internal data class PluginResourceActivity(
    val busy: Boolean = false,
    val editing: Boolean = false,
    val pending: Boolean = false,
)

@Composable
internal fun PluginResources(
    vm: CairnViewModel,
    state: Workspace,
    manifest: PluginManifest,
    location: PluginResourceLocation,
    enabled: Boolean = true,
    stateChanged: (PluginResourceActivity) -> Unit = {},
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
    var grantEditors by
        remember(location.plugin, location.instance) { mutableStateOf(mapOf<String, Boolean>()) }
    val grantLocked = grantEditors.values.any { it }
    var authorizations by
        remember(location.plugin, location.instance) {
            mutableStateOf(mapOf<String, PluginAuthorizationActivity>())
        }
    var loading by remember(location.plugin, location.instance) { mutableStateOf(false) }
    val loads = remember(location.plugin, location.instance) { AtomicInteger() }
    val oauthBusy = authorizations.values.any { it.busy }
    val oauthPending = authorizations.values.any { it.pending }
    val controlsEnabled =
        enabled && !state.busy && !loading && !oauthBusy && !oauthPending && !grantLocked
    LaunchedEffect(loading, oauthBusy, oauthPending, editing, removing, grantLocked) {
        stateChanged(
            PluginResourceActivity(
                loading || oauthBusy,
                editing != null || removing != null || grantLocked,
                oauthPending,
            )
        )
    }
    DisposableEffect(location.plugin, location.instance) {
        onDispose {
            loads.incrementAndGet()
            stateChanged(PluginResourceActivity())
        }
    }
    suspend fun load() {
        val request = loads.incrementAndGet()
        loading = true
        try {
            val refreshed =
                manifest.resources.associate { collection ->
                    // Keep the previously visible IDs so later-page consent stays composed.
                    val remaining =
                        pages[collection.name]?.items.orEmpty().map { it.id }.toMutableSet()
                    val items = mutableListOf<PluginResource>()
                    var page =
                        vm.api.get<PluginPage<PluginResource>>(
                            "${location.path}/${segment(collection.name)}"
                        )
                    while (true) {
                        items += page.items
                        remaining.removeAll(page.items.map { it.id }.toSet())
                        val cursor = page.nextCursor
                        if (remaining.isEmpty() || cursor == null) break
                        page =
                            vm.api.get(
                                "${location.path}/${segment(collection.name)}?cursor=${segment(cursor)}"
                            )
                    }
                    collection.name to PluginPage(items.toList(), page.nextCursor)
                }
            if (request == loads.get()) pages = refreshed
        } finally {
            if (request == loads.get()) loading = false
        }
    }
    suspend fun more(collection: String) {
        val request = loads.get()
        val page = pages[collection] ?: return
        val cursor = page.nextCursor ?: return
        val next =
            vm.api.get<PluginPage<PluginResource>>(
                "${location.path}/${segment(collection)}?cursor=${segment(cursor)}"
            )
        if (request == loads.get())
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
        TextButton(
            onClick = { vm.perform { load() } },
            enabled = !state.busy && !loading && !grantLocked,
        ) {
            Text("Actualiser les ressources")
        }
        manifest.resources.forEach { collection ->
            Text(collection.title, style = MaterialTheme.typography.titleSmall)
            pages[collection.name]?.items?.forEach { resource ->
                key(resource.id) {
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
                                enabled = controlsEnabled,
                            ) {
                                Text("Modifier ${resource.name}")
                            }
                            TextButton(
                                onClick = { removing = resource },
                                enabled = controlsEnabled,
                            ) {
                                Text("Supprimer ${resource.name}")
                            }
                        }
                        val resourceOperations =
                            manifest.operations.filter { it.collection == collection.name }
                        if (
                            location.setupRevision == null &&
                                manifest.agentAccess != null &&
                                resourceOperations.isNotEmpty()
                        ) {
                            key("${location.path}/${resource.id}/grants@${resource.revision}") {
                                PluginResourceGrants(
                                    vm,
                                    state,
                                    "${location.path}/${segment(collection.name)}/${segment(resource.id)}/grants",
                                    resource.name,
                                    resourceOperations,
                                    enabled &&
                                        !state.busy &&
                                        !loading &&
                                        editing == null &&
                                        !oauthBusy &&
                                        !oauthPending,
                                    activity = { active ->
                                        grantEditors =
                                            if (active) grantEditors + (resource.id to true)
                                            else grantEditors - resource.id
                                    },
                                )
                            }
                        }
                        val authorizationPath =
                            "${location.path}/${segment(collection.name)}/${segment(resource.id)}/oauth/"
                        manifest.oauth
                            .filter { it.collection == collection.name && it.personal == null }
                            .forEach { provider ->
                                val path = "$authorizationPath${segment(provider.name)}"
                                val identity =
                                    "$path@${resource.revision}@${location.setupRevision.orEmpty()}"
                                key(identity) {
                                    PluginResourceAuthorization(
                                        vm,
                                        path,
                                        provider,
                                        location.setupRevision,
                                        enabled &&
                                            !state.busy &&
                                            !loading &&
                                            editing == null &&
                                            !grantLocked,
                                        blocked =
                                            authorizations.any { (other, activity) ->
                                                other != identity &&
                                                    other.startsWith(authorizationPath) &&
                                                    (activity.busy || activity.pending)
                                            },
                                        stateChanged = { activity ->
                                            authorizations =
                                                if (activity.busy || activity.pending)
                                                    authorizations + (identity to activity)
                                                else authorizations - identity
                                        },
                                        changed = { load() },
                                    )
                                }
                            }
                    }
                }
            }
            if (pages[collection.name]?.items?.isEmpty() == true)
                Text("Aucune ressource.", style = MaterialTheme.typography.bodySmall)
            if (pages[collection.name]?.nextCursor != null)
                TextButton(
                    onClick = { vm.perform { more(collection.name) } },
                    enabled = !state.busy && !loading && !grantLocked,
                ) {
                    Text("Voir plus · ${collection.title}")
                }
            SignalButton(
                "Ajouter · ${collection.singular}",
                height = 40.dp,
                enabled = controlsEnabled,
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
        Field(
            "Nom · ${collection.singular}",
            name,
            { name = it },
            enabled = !state.busy,
        )
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

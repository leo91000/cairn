package build.cairn.app.ui

import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import build.cairn.app.data.*
import java.util.concurrent.atomic.AtomicInteger
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.delay
import kotlinx.coroutines.ensureActive
import kotlinx.serialization.Serializable
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

/** Only an open editor's identity is saved, rather than its resource list. */
@Serializable
private data class ResourceEditorTarget(
    val id: String,
    val collection: String,
    val revision: String,
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
        rememberForm<Pair<PluginResourceCollection, PluginResource?>?>(null, location.path)
    var removing by
        remember(location.plugin, location.instance) { mutableStateOf<PluginResource?>(null) }
    var accessEditors by rememberForm(emptyMap<String, ResourceEditorTarget>(), location.path)
    val accessLocked = accessEditors.isNotEmpty()
    var authorizations by
        remember(location.plugin, location.instance) {
            mutableStateOf(mapOf<String, PluginAuthorizationActivity>())
        }
    var loading by remember(location.plugin, location.instance) { mutableStateOf(false) }
    var ready by remember(location.plugin, location.instance) { mutableStateOf(false) }
    val loads = remember(location.plugin, location.instance) { AtomicInteger() }
    fun accessBlocked(own: String) = accessEditors.keys.any { it != own }
    fun accessActivity(key: String, resource: PluginResource, active: Boolean) {
        val target = ResourceEditorTarget(resource.id, resource.collection, resource.revision)
        if (active) accessEditors = accessEditors + (key to target)
        else if (accessEditors[key] == target) accessEditors = accessEditors - key
    }
    val oauthBusy = authorizations.values.any { it.busy }
    val oauthPending = authorizations.values.any { it.pending }
    val controlsEnabled =
        enabled && ready && !state.busy && !loading && !oauthBusy && !oauthPending && !accessLocked
    LaunchedEffect(loading, oauthBusy, oauthPending, editing, removing, accessLocked) {
        stateChanged(
            PluginResourceActivity(
                loading || oauthBusy,
                editing != null || removing != null || accessLocked,
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
        ready = false
        try {
            val refreshed =
                manifest.resources.associate { collection ->
                    // Keep the previously visible IDs so later-page consent stays composed.
                    val remaining =
                        pages[collection.name]?.items.orEmpty().map { it.id }.toMutableSet()
                    remaining +=
                        accessEditors.values
                            .filter { it.collection == collection.name }
                            .map { it.id }
                    editing
                        ?.takeIf { it.first.name == collection.name }
                        ?.second
                        ?.let { remaining += it.id }
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
            if (request == loads.get()) {
                pages = refreshed
                accessEditors = accessEditors.filterValues { target ->
                    refreshed[target.collection]?.items.orEmpty().any {
                        it.id == target.id && it.revision == target.revision
                    }
                }
                ready = true
            }
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
            if (e is CancellationException) throw e
            vm.report(e)
        }
    }
    Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
        TextButton(
            onClick = { vm.perform { load() } },
            enabled = !state.busy && !loading && (!accessLocked || !ready),
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
                                        ready &&
                                        !state.busy &&
                                        !loading &&
                                        editing == null &&
                                        removing == null &&
                                        !oauthBusy &&
                                        !oauthPending &&
                                        !accessBlocked("grants/${resource.id}"),
                                    activity = { active ->
                                        accessActivity("grants/${resource.id}", resource, active)
                                    },
                                )
                            }
                        }
                        if (location.setupRevision == null && collection.delegation) {
                            key("${location.path}/${resource.id}/routes@${resource.revision}") {
                                PluginResourceRoutes(
                                    vm,
                                    state,
                                    "${location.path}/${segment(collection.name)}/${segment(resource.id)}",
                                    resource.name,
                                    enabled &&
                                        ready &&
                                        !state.busy &&
                                        !loading &&
                                        editing == null &&
                                        removing == null &&
                                        !oauthBusy &&
                                        !oauthPending &&
                                        !accessBlocked("routes/${resource.id}"),
                                    activity = { active ->
                                        accessActivity("routes/${resource.id}", resource, active)
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
                                            ready &&
                                            !state.busy &&
                                            !loading &&
                                            editing == null &&
                                            !accessLocked,
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
                    enabled = ready && !state.busy && !loading && !accessLocked,
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
        key(location, manifest.version, collection.name, initial?.id) {
            val unchanged =
                manifest.resources.any { it == collection } &&
                    (initial == null ||
                        pages[collection.name]?.items.orEmpty().any {
                            it.id == initial.id && it.revision == initial.revision
                        })
            ResourceEditor(
                vm,
                state,
                location,
                collection,
                initial,
                pages[collection.parent]?.items.orEmpty(),
                ready = controlsEnabled && unchanged,
                stale = ready && !unchanged,
                reload = if (!ready && !loading) ({ vm.perform { load() } }) else null,
                close = { editing = null },
            ) {
                editing = null
                load()
            }
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
    ready: Boolean,
    stale: Boolean,
    reload: (() -> Unit)?,
    close: () -> Unit,
    saved: suspend () -> Unit,
) {
    var name by rememberForm(initial?.name.orEmpty())
    var parent by rememberForm(initial?.parentId.orEmpty())
    var enabled by rememberForm(initial?.enabled ?: true)
    val ordinaryDraft =
        rememberPluginSettingsDraft(
            collection.settings,
            collection.settings
                .filter { it.type != "secret" }
                .associate { it.key to formValue(it, initial?.settings?.get(it.key)) },
            "${location.path}/${collection.name}/${initial?.id.orEmpty()}@${initial?.revision.orEmpty()}@${location.setupRevision.orEmpty()}",
        )
    var values by ordinaryDraft.state
    var secrets by remember { mutableStateOf(mapOf<String, String>()) }
    var submitted by rememberForm(false)
    val controls = ready && !state.busy && !submitted
    var completion by remember { mutableStateOf<PluginForm?>(null) }
    var completing by remember { mutableStateOf(collection.form) }
    var formReady by remember { mutableStateOf(!collection.form) }
    var formFailure by remember { mutableStateOf<String?>(null) }
    var asking by remember { mutableIntStateOf(0) }
    val formGeneration = remember { AtomicInteger() }
    var changed by remember { mutableStateOf<String?>(null) }
    fun refresh(key: String? = null) {
        if (!collection.form) return
        changed = key
        asking++
        completing = true
        formReady = false
    }
    if (collection.form)
        LaunchedEffect(location, initial?.revision, parent, asking, ready, submitted) {
            val generation = formGeneration.incrementAndGet()
            formReady = false
            if (!ready || submitted) return@LaunchedEffect
            val request = asking
            completing = true
            formFailure = null
            if (request > 0) delay(350)
            try {
                val result =
                    decodePluginForm(
                        vm.api.pluginOperation(
                            "${location.path}/${segment(collection.name)}/form",
                            buildJsonObject {
                                put("settings", pluginSettings(collection.settings, values))
                                put("secrets", pluginSecrets(secrets))
                                if (parent.isNotBlank()) put("parentId", parent)
                                initial?.let {
                                    put("resource", it.id)
                                    put("revision", it.revision)
                                }
                                location.setupRevision?.let { put("setupRevision", it) }
                                changed?.let { put("changed", it) }
                            },
                        )
                    )
                currentCoroutineContext().ensureActive()
                if (request != asking || generation != formGeneration.get()) return@LaunchedEffect
                values =
                    values +
                        result.values.mapValues { (key, value) ->
                            formValue(
                                collection.settings.find { it.key == key }
                                    ?: PluginSetting(key = key),
                                value,
                            )
                        }
                completion = result
                formReady = true
            } catch (e: Exception) {
                if (e is CancellationException) throw e
                if (request == asking && generation == formGeneration.get())
                    formFailure = e.message ?: "Le plugin n’a pas répondu."
            } finally {
                if (request == asking && generation == formGeneration.get()) completing = false
            }
        }
    val canSave = controls && !completing && formReady && completion?.errors.orEmpty().isEmpty()
    fun reconcile() {
        vm.perform { saved() }
    }
    Editor(
        if (initial == null) "Nouveau · ${collection.singular}" else "Modifier ${initial.name}",
        state.busy,
        state.error,
        {
            if (submitted) reconcile()
            else {
                ordinaryDraft.discard()
                close()
            }
        },
        valid = canSave && name.isNotBlank() && (collection.parent == null || parent.isNotBlank()),
        save = save@{
                if (!canSave || submitted) return@save
                vm.perform {
                    submitted = true
                    try {
                        api.pluginOperation(
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
                            method = if (initial == null) "POST" else "PUT",
                        )
                        secrets = emptyMap()
                        ordinaryDraft.discard()
                        saved()
                    } catch (e: Exception) {
                        if (e is ApiException && e.status == 400) submitted = false
                        throw e
                    }
                }
            },
    ) {
        ordinaryDraft.notice?.let { FormError(it) }
        if (stale)
            FormError("La ressource a changé. Fermez cet éditeur puis actualisez les ressources.")
        if (!submitted)
            reload?.let { retry ->
                TextButton(onClick = retry, enabled = !state.busy) {
                    Text("Réessayer le chargement")
                }
            }
        if (submitted && !state.busy) {
            FormError(
                "Actualisez les ressources pour vérifier la sauvegarde avant une autre modification."
            )
            TextButton(onClick = { reconcile() }, enabled = !state.busy) {
                Text("Vérifier la sauvegarde")
            }
        }
        Field(
            "Nom · ${collection.singular}",
            name,
            { name = it },
            enabled = controls,
        )
        if (collection.parent != null && controls)
            Choice("Parent", parent, parents.map { it.id to it.name }) {
                parent = it
                refresh("parentId")
            }
        PluginFields(
            collection.settings,
            values,
            { key, value ->
                values = values + (key to value)
                refresh(key)
            },
            secrets,
            { key, value ->
                secrets = secrets + (key to value)
                refresh(key)
            },
            initial?.secretKeys.orEmpty(),
            enabled = controls,
            options = completion?.options.orEmpty(),
            errors = completion?.errors.orEmpty(),
        )
        completion?.let { result ->
            result.errors
                .filterKeys { key ->
                    key == "form" ||
                        key == "parentId" ||
                        collection.settings.any { it.key == key && it.hidden }
                }
                .values
                .forEach { FormError(it) }
            PluginViewNodes(result.view, true, {}, { _, _, _ -> })
        }
        if (completing)
            Text("Vérification des réglages…", style = MaterialTheme.typography.bodySmall)
        formFailure?.let {
            FormError(it)
            TextButton(onClick = { refresh() }, enabled = controls && !completing) {
                Text("Réessayer la vérification")
            }
        }
        Toggle("Activé", enabled) { if (controls) enabled = it }
    }
    DisposableEffect(Unit) {
        onDispose {
            formGeneration.incrementAndGet()
            secrets = emptyMap()
        }
    }
}

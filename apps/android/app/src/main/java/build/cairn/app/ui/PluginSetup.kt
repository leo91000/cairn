@file:OptIn(androidx.compose.material3.ExperimentalMaterial3Api::class)

package build.cairn.app.ui

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import build.cairn.app.data.*
import kotlinx.coroutines.delay
import kotlinx.serialization.json.*

/** Owner configuration is kept on the installation; typed secrets stay in memory. */
@Composable
internal fun PluginSetup(
    vm: CairnViewModel,
    state: Workspace,
    manifest: PluginManifest,
    initial: PluginSetupDraft,
    close: () -> Unit,
    changed: suspend () -> Unit,
    activated: suspend () -> Unit,
) {
    var draft by remember(initial.id) { mutableStateOf(initial) }
    var name by remember(initial.id) { mutableStateOf(initial.name) }
    var values by
        remember(initial.id) {
            mutableStateOf(
                manifest.settings
                    .filter { it.type != "secret" }
                    .associate { it.key to formValue(it, initial.settings[it.key]) }
            )
        }
    var token by remember(initial.id) { mutableStateOf("") }
    var secrets by remember(initial.id) { mutableStateOf(mapOf<String, String>()) }
    var granted by remember(initial.id) { mutableStateOf(listOf<String>()) }
    var agentId by remember(initial.id) { mutableStateOf("") }
    var enabled by remember(initial.id) { mutableStateOf(true) }
    var completion by remember(initial.id) { mutableStateOf<PluginForm?>(null) }
    var completing by remember(initial.id) { mutableStateOf(manifest.instanceForm) }
    var asking by remember(initial.id) { mutableStateOf(0) }
    var changedField by remember(initial.id) { mutableStateOf<String?>(null) }
    var expired by remember(initial.id) { mutableStateOf(false) }
    val path = "/plugins/${segment(initial.pluginId)}/setup-drafts/${segment(initial.id)}"
    val dirty =
        name.trim() != draft.name ||
            pluginSettings(manifest.settings, values) != draft.settings ||
            token.isNotBlank() ||
            secrets.values.any { it.isNotBlank() }
    val available = draft.status == "setup" && !expired
    fun refresh(changed: String? = null) {
        if (manifest.instanceForm) {
            changedField = changed
            completing = true
            asking++
        }
    }
    LaunchedEffect(draft.expiresAt) {
        expired = draft.expiresAt <= System.currentTimeMillis()
        if (!expired) {
            delay((draft.expiresAt - System.currentTimeMillis()).coerceAtLeast(0))
            expired = true
        }
    }
    if (manifest.instanceForm)
        LaunchedEffect(draft.revision, asking) {
            completing = true
            if (asking > 0) delay(350)
            try {
                val result =
                    decodePluginForm(
                        vm.api.send<JsonElement>(
                            "POST",
                            "$path/form",
                            buildJsonObject {
                                put("revision", draft.revision)
                                put("settings", pluginSettings(manifest.settings, values))
                                put(
                                    "secrets",
                                    JsonObject(secrets.mapValues { JsonPrimitive(it.value) }),
                                )
                                if (token.isNotBlank()) put("token", token.trim())
                                changedField?.let { put("changed", it) }
                            },
                        )
                    )
                values =
                    values +
                        result.values.mapValues { (key, value) ->
                            formValue(
                                manifest.settings.find { it.key == key }
                                    ?: PluginSetting(key = key),
                                value,
                            )
                        }
                completion = result
            } catch (e: Exception) {
                if (e is kotlinx.coroutines.CancellationException) throw e
                completion =
                    PluginForm(
                        errors = mapOf("form" to (e.message ?: "Le plugin n’a pas répondu."))
                    )
            }
            completing = false
        }
    Dialog(
        onDismissRequest = { if (!state.busy) close() },
        properties =
            DialogProperties(usePlatformDefaultWidth = false, decorFitsSystemWindows = false),
    ) {
        Scaffold(
            modifier = Modifier.fillMaxSize().imePadding(),
            topBar = {
                TopAppBar(
                    title = { Text("Configurer ${manifest.name}") },
                    navigationIcon = {
                        IconButton(onClick = close, enabled = !state.busy) {
                            Icon(Icons.AutoMirrored.Filled.ArrowBack, "Fermer")
                        }
                    },
                )
            },
        ) { padding ->
            Column(
                Modifier.padding(padding)
                    .fillMaxSize()
                    .verticalScroll(rememberScrollState())
                    .padding(20.dp),
                verticalArrangement = Arrangement.spacedBy(16.dp),
            ) {
                Text(
                    "La configuration est conservée 24 heures et reste inactive jusqu’à sa validation. Seules les modifications enregistrées sont conservées à la fermeture.",
                    style = MaterialTheme.typography.bodySmall,
                )
                if (state.busy || completing) LinearProgressIndicator(Modifier.fillMaxWidth())
                state.error?.let { FormError(it) }
                if (!available)
                    FormError(
                        "Cette configuration a expiré ou le package doit être approuvé. Fermez-la puis actualisez le plugin."
                    )
                Field("Nom", name, { name = it }, enabled = !state.busy && available)
                manifest.credential?.let { credential ->
                    OutlinedTextField(
                        token,
                        {
                            token = it
                            refresh("credential")
                        },
                        Modifier.fillMaxWidth(),
                        label = { Text(credential.label) },
                        isError = completion?.errors?.containsKey("credential") == true,
                        supportingText = {
                            completion?.errors?.get("credential")?.let { Text(it) }
                        },
                        visualTransformation = PasswordVisualTransformation(),
                        keyboardOptions = InputKeyboards.Password,
                        singleLine = true,
                        enabled = !state.busy && available,
                        placeholder = {
                            if (draft.credential) Text("Enregistré · laissez vide pour le garder")
                        },
                    )
                }
                PluginFields(
                    manifest.settings.map { it.copy(required = false) },
                    values,
                    { key, value ->
                        values = values + (key to value)
                        refresh()
                    },
                    secrets,
                    { key, value ->
                        secrets = secrets + (key to value)
                        refresh()
                    },
                    draft.secretKeys,
                    enabled = !state.busy && available,
                    options = completion?.options.orEmpty(),
                    errors = completion?.errors.orEmpty(),
                    refresh = { refresh(it) },
                )
                completion?.let { form ->
                    form.errors["form"]?.let { FormError(it) }
                    manifest.unshownFormErrors(form.errors).forEach { FormError(it) }
                    PluginViewNodes(form.view, state.busy, {}, { _, _, _ -> })
                }
                SignalButton(
                    "Enregistrer la configuration",
                    enabled = !state.busy && !completing && available && name.isNotBlank(),
                ) {
                    vm.perform {
                        draft =
                            api.send(
                                "PATCH",
                                path,
                                buildJsonObject {
                                    put("revision", draft.revision)
                                    put("name", name.trim())
                                    put("settings", pluginSettings(manifest.settings, values))
                                    put(
                                        "secrets",
                                        JsonObject(
                                            secrets
                                                .filterValues { it.isNotBlank() }
                                                .mapValues { JsonPrimitive(it.value.trim()) }
                                        ),
                                    )
                                    if (token.isNotBlank()) put("token", token.trim())
                                },
                            )
                        changedField = null
                        name = draft.name
                        values =
                            manifest.settings
                                .filter { it.type != "secret" }
                                .associate { it.key to formValue(it, draft.settings[it.key]) }
                        secrets = emptyMap()
                        token = ""
                        changed()
                    }
                }
                Text(
                    if (completing) "Vérification…"
                    else if (dirty) "Modifications non enregistrées" else "Tout est enregistré",
                    style = MaterialTheme.typography.bodySmall,
                )
                if (dirty)
                    Text(
                        "Enregistrez les modifications avant de configurer les ressources ou d’activer l’instance.",
                        style = MaterialTheme.typography.bodySmall,
                    )
                if (manifest.resources.isNotEmpty())
                    PluginResources(
                        vm,
                        state,
                        manifest,
                        PluginResourceLocation(initial.pluginId, draft.id, draft.revision),
                        enabled = available && !dirty && !completing && !state.busy,
                    )
                if (manifest.agentAccess != null) {
                    Text(
                        "Commandes et outils de l’instance",
                        style = MaterialTheme.typography.titleSmall,
                    )
                    Text(
                        "Aucun agent n’a d’accès par défaut. Les droits sur les ressources se configurent séparément après l’activation.",
                        style = MaterialTheme.typography.bodySmall,
                    )
                    state.agents.forEach { agent ->
                        Toggle(agent.name, agent.id in granted) { selected ->
                            if (!state.busy && available)
                                granted = if (selected) granted + agent.id else granted - agent.id
                        }
                    }
                }
                if (manifest.delegates && !state.busy)
                    Choice(
                        "Agent des conversations entrantes",
                        agentId,
                        state.agents.filter { it.eligibleForPlugins() }.map { it.id to it.name },
                    ) {
                        agentId = it
                    }
                if (manifest.delegates)
                    Text(
                        "Les personnes doivent être approuvées. Vous pourrez régler leurs accès et les limites après activation.",
                        style = MaterialTheme.typography.bodySmall,
                    )
                Toggle("Démarrer immédiatement", enabled) {
                    if (!state.busy && available) enabled = it
                }
                SignalButton(
                    "Activer l’instance",
                    enabled =
                        available &&
                            !state.busy &&
                            !dirty &&
                            !completing &&
                            (!manifest.instanceForm ||
                                (completion != null && completion?.errors?.isEmpty() == true)) &&
                            (!manifest.delegates || agentId.isNotBlank()),
                ) {
                    vm.perform {
                        api.request(
                            "POST",
                            "$path/activate",
                            buildJsonObject {
                                put("revision", draft.revision)
                                put("enabled", enabled)
                                put("agentIds", JsonArray(granted.map(::JsonPrimitive)))
                                if (manifest.delegates) {
                                    put("agentId", agentId)
                                    put(
                                        "projectId",
                                        state.agents
                                            .find { it.id == agentId }
                                            ?.access
                                            ?.projects
                                            ?.firstOrNull()
                                            ?.let(::JsonPrimitive) ?: JsonNull,
                                    )
                                    put("accessMode", "requests")
                                }
                            },
                        )
                        activated()
                    }
                }
            }
        }
    }
    DisposableEffect(initial.id) {
        onDispose {
            secrets = emptyMap()
            token = ""
        }
    }
}

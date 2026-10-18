package build.cairn.app.ui

import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import build.cairn.app.data.*
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Job
import kotlinx.coroutines.launch
import kotlinx.serialization.json.*

/** Owner-managed resource rules. Opening the editor never grants an agent access. */
@Composable
internal fun PluginResourceGrants(
    vm: CairnViewModel,
    state: Workspace,
    path: String,
    name: String,
    operations: List<PluginOperation>,
    enabled: Boolean,
    activity: (Boolean) -> Unit,
) {
    var opened by remember { mutableStateOf(false) }
    var ready by remember { mutableStateOf(false) }
    var busy by remember { mutableStateOf(false) }
    var writing by remember { mutableStateOf(false) }
    var generation by remember { mutableIntStateOf(0) }
    var pending by remember { mutableStateOf<Job?>(null) }
    var error by remember { mutableStateOf<String?>(null) }
    var rules by remember { mutableStateOf(emptyList<PluginResourceGrant>()) }
    var adding by remember { mutableStateOf(false) }
    var removing by remember { mutableStateOf<PluginResourceGrant?>(null) }
    var agentId by remember { mutableStateOf("") }
    var profile by remember { mutableStateOf<PluginAccessProfile?>(null) }
    var overrides by remember { mutableStateOf(emptyMap<String, String>()) }
    var limits by remember { mutableStateOf(emptyMap<String, String>()) }
    val scope = rememberCoroutineScope()
    val scopeNames = operations.flatMap { it.scopes }.distinct()
    fun currentRule() =
        PluginResourceGrant(
            agentId = agentId,
            profile = profile,
            operations = operations.filter { overrides[it.name] == "allow" }.map { it.name },
            deniedOperations = operations.filter { overrides[it.name] == "deny" }.map { it.name },
            scopes =
                scopeNames
                    .mapNotNull { key ->
                        val values =
                            limits[key]
                                .orEmpty()
                                .lines()
                                .map { it.trim() }
                                .filter { it.isNotEmpty() }
                                .distinct()
                        if (values.isEmpty()) null else key to values
                    }
                    .toMap(),
        )
    val draft = currentRule()
    val allowed = operations.filter { draft.permits(it) }
    val controls = ready && enabled && !busy
    val canGrant = controls && state.agents.any { it.id == agentId } && allowed.isNotEmpty()
    suspend fun load(current: Int) {
        ready = false
        val result = vm.api.get<PluginPage<PluginResourceGrant>>(path)
        if (current == generation) {
            rules = result.items
            ready = true
        }
    }
    fun refresh() {
        if (busy) return
        busy = true
        error = null
        val current = ++generation
        pending = scope.launch {
            try {
                load(current)
            } catch (e: Exception) {
                if (e is CancellationException) throw e
                if (current == generation) error = e.message
            } finally {
                if (current == generation) {
                    busy = false
                    pending = null
                }
            }
        }
    }
    fun close() {
        if (writing) return
        generation++
        pending?.cancel()
        pending = null
        busy = false
        ready = false
        opened = false
        adding = false
        removing = null
        activity(false)
    }
    fun mutate(revoking: PluginResourceGrant? = null) {
        if (busy || !ready || !enabled) return
        val input = currentRule()
        if (
            revoking == null &&
                (state.agents.none { it.id == input.agentId } ||
                    operations.none { input.permits(it) })
        )
            return
        val target = if (revoking == null) path else "$path/${segment(revoking.id)}"
        val body =
            if (revoking == null)
                buildJsonObject {
                    put("agentId", input.agentId)
                    put(
                        "profile",
                        input.profile?.let { wireJson.encodeToJsonElement(it) } ?: JsonNull,
                    )
                    put("operations", JsonArray(input.operations.map(::JsonPrimitive)))
                    put("deniedOperations", JsonArray(input.deniedOperations.map(::JsonPrimitive)))
                    put("scopes", wireJson.encodeToJsonElement(input.scopes))
                }
            else buildJsonObject {}
        busy = true
        writing = true
        ready = false
        error = null
        val current = ++generation
        pending = scope.launch {
            try {
                vm.api.request(if (revoking == null) "POST" else "DELETE", target, body)
                if (current != generation) return@launch
                writing = false
                adding = false
                removing = null
                load(current)
            } catch (e: Exception) {
                if (e is CancellationException) throw e
                // The server may already have changed permissions; never repost automatically.
                if (current == generation) {
                    error = "${e.message} Actualisez les règles avant une autre modification."
                    adding = false
                    removing = null
                }
            } finally {
                if (current == generation) {
                    writing = false
                    busy = false
                    pending = null
                }
            }
        }
    }
    DisposableEffect(Unit) {
        onDispose {
            generation++
            pending?.cancel()
            activity(false)
        }
    }
    TextButton(
        onClick = {
            opened = true
            activity(true)
            refresh()
        },
        enabled = enabled,
    ) {
        Text("Accès des agents")
    }
    if (opened)
        Editor(
            title = "Accès des agents · $name",
            busy = busy,
            error = error,
            close = { close() },
            save = { mutate() },
            valid = adding && canGrant,
            dismissible = !writing,
        ) {
            Text(
                "Aucun agent n’a accès par défaut. Les règles s’additionnent : un refus ne s’applique qu’à sa règle, une autre peut encore autoriser l’opération."
            )
            Text(
                "Les permissions du service et les autorisations personnelles restent nécessaires.",
                style = MaterialTheme.typography.bodySmall,
            )
            TextButton(onClick = { refresh() }, enabled = !busy && !adding && removing == null) {
                Text(if (busy) "Chargement des règles…" else "Actualiser les règles")
            }
            if (ready && !adding) {
                if (rules.isEmpty()) Text("Aucune règle d’accès.")
                rules.forEach { rule ->
                    SignalCard {
                        Text(
                            "${state.agents.find { it.id == rule.agentId }?.name ?: "Agent supprimé"} · ${rule.profile?.label ?: "Opérations choisies"}",
                            style = MaterialTheme.typography.titleSmall,
                        )
                        Text(
                            "Autorisées par cette règle : ${operations.filter { rule.permits(it) }.joinToString { it.name }.ifEmpty { "Aucune" }}",
                            style = MaterialTheme.typography.bodySmall,
                        )
                        if (rule.operations.isNotEmpty())
                            Text(
                                "Autorisées explicitement : ${rule.operations.joinToString()}",
                                style = MaterialTheme.typography.bodySmall,
                            )
                        if (rule.deniedOperations.isNotEmpty())
                            Text(
                                "Refusées dans cette règle : ${rule.deniedOperations.joinToString()}",
                                style = MaterialTheme.typography.bodySmall,
                            )
                        rule.scopes.forEach { (key, values) ->
                            Text(
                                "$key : ${values.joinToString()}",
                                style = MaterialTheme.typography.bodySmall,
                            )
                        }
                        if (rule.scopes.isEmpty())
                            Text(
                                "Aucune limite de périmètre supplémentaire.",
                                style = MaterialTheme.typography.bodySmall,
                            )
                        TextButton(onClick = { removing = rule }, enabled = controls) {
                            Text("Révoquer la règle")
                        }
                    }
                }
                SignalButton("Ajouter une règle", enabled = controls && rules.size < 64) {
                    agentId = ""
                    profile = null
                    overrides = emptyMap()
                    limits = emptyMap()
                    adding = true
                }
            }
            if (ready && adding) {
                ChoiceList(
                    "Agent",
                    agentId,
                    state.agents.map { PluginOption(it.id, it.name) },
                    enabled = controls,
                ) {
                    agentId = it
                }
                ChoiceList(
                    "Profil",
                    profile?.name.orEmpty(),
                    listOf(PluginOption("", "Opérations choisies")) +
                        PluginAccessProfile.entries.map { PluginOption(it.name, it.label) },
                    enabled = controls,
                ) { value ->
                    profile = PluginAccessProfile.entries.find { it.name == value }
                }
                Text(
                    "Contribution inclut Lecture. Gestion inclut les deux. Choisissez les exceptions ci-dessous.",
                    style = MaterialTheme.typography.bodySmall,
                )
                operations.forEach { operation ->
                    ChoiceList(
                        operation.name,
                        overrides[operation.name].orEmpty(),
                        listOf(
                            PluginOption("", "Selon le profil"),
                            PluginOption("allow", "Autoriser ${operation.name}"),
                            PluginOption("deny", "Refuser ${operation.name}"),
                        ),
                        enabled = controls,
                    ) {
                        overrides = overrides + (operation.name to it)
                    }
                }
                Text(
                    "Limites facultatives : une valeur autorisée par ligne. Vide signifie aucune limite supplémentaire. Les opérations sans chacun des périmètres choisis sont exclues.",
                    style = MaterialTheme.typography.bodySmall,
                )
                scopeNames.forEach { key ->
                    Field(
                        "Limiter $key",
                        limits[key].orEmpty(),
                        { limits = limits + (key to it) },
                        lines = 2,
                        enabled = controls,
                    )
                }
                Text(
                    "Autorisées par cette règle : ${allowed.joinToString { it.name }.ifEmpty { "Aucune" }}",
                    style = MaterialTheme.typography.bodySmall,
                )
                TextButton(onClick = { adding = false }, enabled = !busy) {
                    Text("Annuler la règle")
                }
            }
        }
    removing?.let { rule ->
        Confirm(
            "Révoquer cette règle ?",
            "Les autres règles conservent leurs propres permissions.",
            busy,
            error,
            dismiss = { removing = null },
        ) {
            mutate(rule)
        }
    }
}

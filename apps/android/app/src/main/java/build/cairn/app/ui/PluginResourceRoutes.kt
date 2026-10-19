package build.cairn.app.ui

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.text.input.KeyboardType
import build.cairn.app.data.*
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Job
import kotlinx.coroutines.launch
import kotlinx.serialization.json.*

/** Configuration has no authority until the owner saves a route through the host. */
@Composable
internal fun PluginResourceRoutes(
    vm: CairnViewModel,
    state: Workspace,
    path: String,
    resourceName: String,
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
    var routes by remember { mutableStateOf(emptyList<PluginRoute>()) }
    var editing by remember { mutableStateOf(false) }
    var original by remember { mutableStateOf<PluginRoute?>(null) }
    var removing by remember { mutableStateOf<PluginRoute?>(null) }
    var people by remember { mutableStateOf<PluginRoute?>(null) }
    var requests by remember { mutableStateOf(emptyList<PluginRouteRequest>()) }
    var name by remember { mutableStateOf("") }
    var routeEnabled by remember { mutableStateOf(true) }
    var conditions by remember { mutableStateOf(emptyList<Pair<String, String>>()) }
    var catchAll by remember { mutableStateOf(false) }
    var agentId by remember { mutableStateOf("") }
    var projectId by remember { mutableStateOf("") }
    var mode by remember { mutableStateOf("requests") }
    var personIds by remember { mutableStateOf("") }
    var hourly by remember { mutableStateOf("20") }
    var personalHourly by remember { mutableStateOf("5") }
    var eligibility by remember { mutableStateOf(emptyList<RouteEligibility>()) }
    var previewFor by remember { mutableStateOf<String?>(null) }
    var previewError by remember { mutableStateOf<String?>(null) }
    var previewGeneration by remember { mutableIntStateOf(0) }
    val scope = rememberCoroutineScope()
    val previewKey = "$projectId/$mode"
    val trimmedName = name.trim()
    val userIds = personIds.lines().map { it.trim() }.filter { it.isNotEmpty() }.distinct()
    val controls = enabled && ready && !busy
    val conditionsValid =
        if (conditions.isEmpty()) catchAll
        else
            conditions.size <= 8 &&
                conditions.map { it.first }.distinct().size == conditions.size &&
                conditions.all { (key, value) ->
                    key.matches(Regex("[a-zA-Z0-9_]{1,40}")) &&
                        value.isNotEmpty() &&
                        value.toByteArray().size <= 200 &&
                        value.none { it.isISOControl() }
                }
    val canSave =
        controls &&
            editing &&
            previewFor == previewKey &&
            eligibility.any { it.agentId == agentId && it.eligible } &&
            trimmedName.isNotEmpty() &&
            Character.codePointCount(trimmedName, 0, trimmedName.length) <= 100 &&
            trimmedName.none { it.isISOControl() } &&
            conditionsValid &&
            (mode != "allowlist" || userIds.isNotEmpty()) &&
            userIds.size <= 100 &&
            userIds.all { it.matches(Regex("[a-zA-Z0-9_.:-]{1,64}")) } &&
            hourly.toIntOrNull() in 1..100 &&
            personalHourly.toIntOrNull() in 1..50

    suspend fun load(current: Int) {
        ready = false
        val result = vm.api.pluginManagementItems<PluginRoute>("$path/routes")
        if (current == generation) {
            routes = result
            ready = true
        }
    }
    fun refresh() {
        if (busy || editing || removing != null) return
        busy = true
        people = null
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
        previewGeneration++
        pending?.cancel()
        pending = null
        busy = false
        ready = false
        opened = false
        editing = false
        removing = null
        people = null
        activity(false)
    }
    fun edit(route: PluginRoute? = null) {
        if (!enabled || !ready || busy) return
        original = route
        name = route?.name.orEmpty()
        routeEnabled = route?.enabled ?: true
        conditions = route?.match?.toList().orEmpty()
        catchAll = route != null && conditions.isEmpty()
        agentId = route?.agentId.orEmpty()
        projectId = route?.projectId.orEmpty()
        mode = route?.accessMode ?: "requests"
        personIds = route?.userIds.orEmpty().joinToString("\n")
        hourly = (route?.maxMessagesPerHour ?: 20).toString()
        personalHourly = (route?.maxPerPersonPerHour ?: 5).toString()
        previewFor = null
        previewError = null
        people = null
        editing = true
    }
    fun mutate(target: String, method: String, body: JsonObject) {
        if (!enabled || !ready || busy) return
        busy = true
        writing = true
        ready = false
        error = null
        val current = ++generation
        pending = scope.launch {
            try {
                vm.api.request(method, target, body)
                if (current != generation) return@launch
                editing = false
                removing = null
                people = null
                writing = false
                load(current)
            } catch (e: Exception) {
                if (e is CancellationException) throw e
                if (current == generation) {
                    editing = false
                    removing = null
                    people = null
                    error = "${e.message} Actualisez les routes avant une autre modification."
                }
            } finally {
                if (current == generation) {
                    busy = false
                    writing = false
                    pending = null
                }
            }
        }
    }
    fun save() {
        if (!canSave || previewFor != "$projectId/$mode") return
        val route = original
        mutate(
            "$path/routes${route?.let { "/${segment(it.id)}" }.orEmpty()}",
            if (route == null) "POST" else "PUT",
            buildJsonObject {
                put("name", name.trim())
                put("enabled", routeEnabled)
                put("match", wireJson.encodeToJsonElement(conditions.toMap()))
                put("agentId", agentId)
                put(
                    "projectId",
                    projectId.takeIf { it.isNotEmpty() }?.let(::JsonPrimitive) ?: JsonNull,
                )
                put("accessMode", mode)
                put("userIds", wireJson.encodeToJsonElement(userIds))
                put(
                    "people",
                    wireJson.encodeToJsonElement(
                        route?.people.orEmpty().filter { it.id in userIds }
                    ),
                )
                put("maxMessagesPerHour", hourly.toInt())
                put("maxPerPersonPerHour", personalHourly.toInt())
                route?.let { put("revision", it.revision) }
            },
        )
    }
    fun showPeople(route: PluginRoute) {
        if (!enabled || !ready || busy) return
        busy = true
        error = null
        val current = ++generation
        pending = scope.launch {
            try {
                val result =
                    vm.api.get<PluginPage<PluginRouteRequest>>(
                        "$path/routes/${segment(route.id)}/requests"
                    )
                if (current == generation) {
                    people = route
                    requests = result.items.filter { it.status == "pending" }
                }
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
    fun personAction(route: PluginRoute, person: String, action: String) =
        mutate(
            "$path/routes/${segment(route.id)}/people/${segment(person)}",
            "POST",
            buildJsonObject {
                put("action", action)
                put("revision", route.revision)
            },
        )
    LaunchedEffect(opened, editing, projectId, mode) {
        val current = ++previewGeneration
        previewFor = null
        eligibility = emptyList()
        previewError = null
        if (!opened || !editing) return@LaunchedEffect
        val expected = previewKey
        try {
            val query =
                "accessMode=${segment(mode)}" +
                    if (projectId.isEmpty()) "" else "&projectId=${segment(projectId)}"
            val result = vm.api.get<PluginPage<RouteEligibility>>("$path/route-eligibility?$query")
            if (
                current == previewGeneration && opened && editing && expected == "$projectId/$mode"
            ) {
                eligibility = result.items
                previewFor = expected
            }
        } catch (e: Exception) {
            if (e is CancellationException) throw e
            if (current == previewGeneration && opened && editing && expected == "$projectId/$mode")
                previewError = e.message
        }
    }
    DisposableEffect(Unit) {
        onDispose {
            generation++
            previewGeneration++
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
        Text("Routes de conversation")
    }
    if (opened)
        Editor(
            title = "Routes de conversation · $resourceName",
            busy = busy,
            error = error,
            close = { close() },
            save = { save() },
            valid = canSave,
            dismissible = !writing,
        ) {
            Text(
                "Choisissez l’agent et les personnes autorisées pour chaque source. La route la plus précise est prioritaire ; une route en pause bloque les sources correspondantes. Les correspondances de même précision demandent une révision."
            )
            TextButton(onClick = { refresh() }, enabled = !busy && !editing && removing == null) {
                Text(if (busy) "Chargement des routes…" else "Actualiser les routes")
            }
            if (ready && !editing && people == null) {
                if (routes.isEmpty())
                    Text(
                        "Aucune route. Personne ne peut démarrer de conversation depuis cette ressource."
                    )
                routes.forEach { route ->
                    SignalCard {
                        Text(
                            "${route.name} · ${if (route.enabled) "Activée" else "En pause"}",
                            style = MaterialTheme.typography.titleSmall,
                        )
                        Text(
                            "${state.agents.find { it.id == route.agentId }?.name ?: "Agent supprimé"} · ${route.projectId?.let { id -> state.projects.find { it.id == id }?.name ?: "Projet supprimé" } ?: "Aucun projet"}"
                        )
                        if (route.match.isEmpty()) Text("Toutes les sources de cette ressource")
                        route.match.forEach { (key, value) -> Text("$key = $value") }
                        Text(
                            "${route.maxMessagesPerHour} messages par heure · " +
                                when (route.accessMode) {
                                    "everyone" ->
                                        "${route.maxPerPersonPerHour} par personne · tout le monde"
                                    "requests" ->
                                        "${route.userIds.size} personnes · approbation sur demande"
                                    else -> "${route.userIds.size} personnes · liste uniquement"
                                }
                        )
                        TextButton(
                            onClick = { edit(route) },
                            enabled = controls && removing == null,
                        ) {
                            Text("Modifier la route ${route.name}")
                        }
                        TextButton(
                            onClick = { showPeople(route) },
                            enabled = controls && removing == null,
                        ) {
                            Text("Personnes et demandes · ${route.name}")
                        }
                        TextButton(
                            onClick = { removing = route },
                            enabled = controls && removing == null,
                        ) {
                            Text("Supprimer la route ${route.name}")
                        }
                        if (removing?.id == route.id) {
                            Text(
                                "Supprimer ${route.name} ? Une route plus générale pourra alors correspondre à ses sources."
                            )
                            TextButton(onClick = { removing = null }, enabled = !busy) {
                                Text("Annuler la suppression")
                            }
                            TextButton(
                                onClick = {
                                    mutate(
                                        "$path/routes/${segment(route.id)}",
                                        "DELETE",
                                        buildJsonObject { put("revision", route.revision) },
                                    )
                                },
                                enabled = controls,
                            ) {
                                Text("Confirmer la suppression")
                            }
                        }
                    }
                }
                SignalButton(
                    "Ajouter une route",
                    enabled = controls && removing == null && routes.size < 256,
                ) {
                    edit()
                }
            }
            if (ready && people != null) {
                val route = people!!
                Text("Personnes · ${route.name}", style = MaterialTheme.typography.titleSmall)
                if (route.accessMode == "everyone")
                    Text(
                        "Tout le monde n’utilise pas de liste de personnes. Modifiez la route et choisissez un mode d’accès restreint pour retirer des participants individuellement."
                    )
                if (route.userIds.isEmpty()) Text("Aucune personne dans la liste.")
                route.userIds.forEach { person ->
                    Text(route.people.find { it.id == person }?.name?.ifBlank { person } ?: person)
                    TextButton(
                        onClick = { personAction(route, person, "remove") },
                        enabled = controls && route.accessMode != "everyone",
                    ) {
                        Text("Retirer $person")
                    }
                }
                Text("Demandes en attente", style = MaterialTheme.typography.titleSmall)
                if (requests.isEmpty()) Text("Aucune demande en attente.")
                requests.forEach { request ->
                    SignalCard {
                        Text(request.name.ifBlank { request.userId })
                        TextButton(
                            onClick = { personAction(route, request.userId, "approve") },
                            enabled = controls && route.accessMode != "everyone",
                        ) {
                            Text("Approuver ${request.userId}")
                        }
                        TextButton(
                            onClick = { personAction(route, request.userId, "deny") },
                            enabled = controls && route.accessMode != "everyone",
                        ) {
                            Text("Refuser ${request.userId}")
                        }
                    }
                }
                TextButton(onClick = { people = null }, enabled = !busy) {
                    Text("Retour aux routes")
                }
            }
            if (ready && editing) {
                Field("Nom de la route", name, { name = it }, enabled = controls)
                Text("Jusqu’à 100 caractères.", style = MaterialTheme.typography.bodySmall)
                Toggle("Route activée", routeEnabled) { if (controls) routeEnabled = it }
                ChoiceList(
                    "Projet",
                    projectId,
                    listOf(PluginOption("", "Aucun projet")) +
                        state.projects.map { PluginOption(it.id, it.name) },
                    controls,
                ) {
                    projectId = it
                }
                ChoiceList(
                    "Qui peut participer",
                    mode,
                    listOf(
                        PluginOption("allowlist", "Personnes de la liste"),
                        PluginOption("requests", "Approuver sur demande"),
                        PluginOption("everyone", "Tout le monde"),
                    ),
                    controls,
                ) {
                    mode = it
                }
                if (previewError != null)
                    Text(previewError!!, color = MaterialTheme.colorScheme.error)
                if (previewFor != previewKey && previewError == null)
                    Text("Vérification des agents éligibles…")
                ChoiceList(
                    "Agent",
                    agentId,
                    state.agents.map { item ->
                        val result = eligibility.find { it.agentId == item.id }
                        PluginOption(
                            item.id,
                            item.name,
                            result?.reason.orEmpty(),
                            result?.eligible != true,
                        )
                    },
                    controls && previewFor == previewKey,
                ) {
                    agentId = it
                }
                Text(
                    "L’agent doit être dédié au projet choisi. Tout le monde exige un agent en lecture seule sans accès installation, GitHub, MCP ou plugin.",
                    style = MaterialTheme.typography.bodySmall,
                )
                Text("Conditions de source", style = MaterialTheme.typography.titleSmall)
                Text(
                    "Mode avancé : clés et valeurs exactes du contexte fourni par l’intégration. Toutes les conditions doivent correspondre.",
                    style = MaterialTheme.typography.bodySmall,
                )
                conditions.forEachIndexed { index, condition ->
                    SignalCard {
                        Field(
                            "Clé de la condition ${index + 1}",
                            condition.first,
                            { value ->
                                conditions = conditions.mapIndexed { i, item ->
                                    if (i == index) value to item.second else item
                                }
                            },
                            enabled = controls,
                        )
                        Field(
                            "Valeur de la condition ${index + 1}",
                            condition.second,
                            { value ->
                                conditions = conditions.mapIndexed { i, item ->
                                    if (i == index) item.first to value else item
                                }
                            },
                            enabled = controls,
                        )
                        TextButton(
                            onClick = {
                                conditions = conditions.filterIndexed { i, _ -> i != index }
                                catchAll = false
                            },
                            enabled = controls,
                        ) {
                            Text("Retirer la condition ${index + 1}")
                        }
                    }
                }
                TextButton(
                    onClick = {
                        conditions = conditions + ("" to "")
                        catchAll = false
                    },
                    enabled = controls && conditions.size < 8,
                ) {
                    Text("Ajouter une condition")
                }
                if (conditions.isEmpty())
                    Toggle("Toutes les sources de cette ressource", catchAll) {
                        if (controls) catchAll = it
                    }
                Field(
                    "Identifiants des personnes autorisées",
                    personIds,
                    { personIds = it },
                    lines = 3,
                    enabled = controls,
                )
                Text(
                    "Un identifiant par ligne, jusqu’à 100. Ces personnes sont autorisées immédiatement. Les demandes exigent une approbation ; Tout le monde n’utilise pas cette liste.",
                    style = MaterialTheme.typography.bodySmall,
                )
                Field(
                    "Messages par heure",
                    hourly,
                    { hourly = it },
                    enabled = controls,
                    keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number),
                )
                Field(
                    "Messages par personne et par heure",
                    personalHourly,
                    { personalHourly = it },
                    enabled = controls,
                    keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number),
                )
                Text(
                    "Le plafond par personne s’applique à Tout le monde.",
                    style = MaterialTheme.typography.bodySmall,
                )
                TextButton(onClick = { editing = false }, enabled = !busy) {
                    Text("Annuler la modification")
                }
            }
        }
}

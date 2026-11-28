@file:OptIn(
    androidx.compose.material3.ExperimentalMaterial3Api::class,
    androidx.compose.foundation.layout.ExperimentalLayoutApi::class,
)

package build.cairn.app.ui

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import build.cairn.app.data.*
import build.cairn.contracts.HookRuleCatalog
import build.cairn.contracts.HookRuleInput
import build.cairn.contracts.runHookRule
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.launch
import kotlinx.serialization.json.*

internal fun ruleObject(vararg fields: Pair<String, JsonElement?>) =
    JsonObject(fields.mapNotNull { (k, v) -> v?.let { k to it } }.toMap())

internal fun JsonObject.patch(vararg fields: Pair<String, JsonElement?>) =
    JsonObject(
        toMutableMap().apply {
            fields.forEach { (key, value) -> if (value == null) remove(key) else put(key, value) }
        }
    )

internal fun JsonElement?.allowed(action: String) = obj("allowed")?.get(action).flag() == true

internal fun ruleAgentName(state: Workspace, id: String) =
    state.agents.find { it.id == id }?.name ?: "Agent"

@Composable
fun RulesScreen(
    vm: CairnViewModel,
    state: Workspace,
    initialRule: String = "",
    agentFilter: String = "",
    instanceFilter: String = "",
    openConversation: (String) -> Unit = {},
    edit: (String) -> Unit,
) {
    var catalog by remember(vm.api.cacheScope) { mutableStateOf<HookRuleCatalog?>(null) }
    var catalogError by remember(vm.api.cacheScope) { mutableStateOf("") }
    var catalogAttempt by remember { mutableIntStateOf(0) }
    LaunchedEffect(vm.api.cacheScope, catalogAttempt) {
        try {
            catalog = vm.api.get("/hook-rules/catalog")
            catalogError = ""
        } catch (e: Exception) {
            if (e is CancellationException) throw e
            catalogError = e.message.orEmpty()
        }
    }
    var selected by rememberSaveable(vm.api.cacheScope, initialRule) { mutableStateOf(initialRule) }
    var detail by remember(vm.api.cacheScope) { mutableStateOf<JsonObject?>(null) }
    var query by rememberSaveable { mutableStateOf("") }
    var filter by rememberSaveable { mutableStateOf("all") }
    var refresh by remember { mutableIntStateOf(0) }
    var scopeCleared by
        rememberSaveable(vm.api.cacheScope, agentFilter, instanceFilter) { mutableStateOf(false) }
    val listPath = buildString {
        append("/hook-rules")
        if (!scopeCleared) {
            val parameters = buildList {
                if (agentFilter.isNotBlank()) add("agentId=${segment(agentFilter)}")
                if (instanceFilter.isNotBlank()) add("instanceId=${segment(instanceFilter)}")
            }
            if (parameters.isNotEmpty()) append("?${parameters.joinToString("&")}")
        }
    }
    var rules by
        remember(vm.api.cacheScope, listPath) {
            mutableStateOf<List<JsonObject>>(emptyList())
        }
    var loading by remember(vm.api.cacheScope, listPath) { mutableStateOf(true) }
    var error by remember(vm.api.cacheScope, listPath) { mutableStateOf("") }
    var creatorNames by
        remember(vm.api.cacheScope) { mutableStateOf<Map<String, String>>(emptyMap()) }
    LaunchedEffect(vm.api.cacheScope, state.isOwner) {
        if (state.isOwner)
            try {
                creatorNames = vm.installationSharing().members.associate { it.id to it.email }
            } catch (e: Exception) {
                if (e is CancellationException) throw e
            }
    }
    val creatorName: (String) -> String = {
        if (it == state.session.account?.id) "vous" else creatorNames[it] ?: "une autre personne"
    }
    Poll("rules:${vm.api.cacheScope}:$listPath:$refresh", 10000) {
        val requestedRefresh = refresh
        try {
            val output: JsonObject = vm.api.get(listPath)
            if (refresh != requestedRefresh) return@Poll
            rules = output.list("rules").map { it.jsonObject }
            val requested = selected
            if (requested.isNotBlank()) {
                val loaded: JsonObject = vm.api.get("/hook-rules/${segment(requested)}")
                if (selected == requested && refresh == requestedRefresh) detail = loaded
            }
            if (refresh == requestedRefresh) error = ""
        } catch (e: Exception) {
            if (e is CancellationException) throw e
            if (refresh == requestedRefresh) error = e.message.orEmpty()
        } finally {
            if (refresh == requestedRefresh) loading = false
        }
    }
    LaunchedEffect(selected, refresh, vm.api.cacheScope) {
        if (detail.text("id") != selected) detail = null
        if (selected.isNotBlank())
            try {
                detail = vm.api.get("/hook-rules/${segment(selected)}")
            } catch (e: Exception) {
                if (e is CancellationException) throw e
                error = e.message.orEmpty()
            }
    }
    fun changed(saved: JsonObject?) {
        refresh++
        rules =
            if (saved == null) rules.filter { it.text("id") != selected }
            else
                rules.map { entry ->
                    if (entry.text("id") != saved.text("id")) entry
                    else
                        entry.patch(
                            *listOf(
                                    "revision",
                                    "version",
                                    "blocks",
                                    "allowed",
                                    "updatedAt",
                                    "nextRun",
                                )
                                .map { it to saved[it] }
                                .toTypedArray()
                        )
                }
        detail = saved
        selected = saved.text("id")
    }
    var scheduling by rememberSaveable { mutableStateOf(false) }
    val scheduleCatalog = catalog
    if (scheduling && scheduleCatalog != null && scheduleCatalog.agents.isNotEmpty())
        ScheduleAgentDialog(vm, state, scheduleCatalog, agentFilter, { scheduling = false }) {
            scheduling = false
            changed(it)
        }
    val visible = rules.filter { rule ->
        (filter != "mine" || rule.text("creatorAccountId") == state.session.account?.id) &&
            (filter != "blocked" || rule.list("blocks").isNotEmpty()) &&
            "${rule.text("name")} ${rule.text("description")}".contains(query.trim(), true)
    }
    BoxWithConstraints(Modifier.fillMaxSize().testTag("rules-screen")) {
        val wide = maxWidth >= 840.dp
        Row(Modifier.fillMaxSize()) {
            Column(if (wide) Modifier.width(390.dp).fillMaxHeight() else Modifier.fillMaxSize()) {
                Column(
                    Modifier.padding(horizontal = 20.dp, vertical = 12.dp),
                    verticalArrangement = Arrangement.spacedBy(10.dp),
                ) {
                    ScreenTitle("Règles", "Quand ceci arrive, faire cela.") {
                        FilledTonalButton(
                            onClick = { edit("") },
                            enabled = catalog?.agents?.isNotEmpty() == true,
                        ) {
                            Text("+ Créer")
                        }
                    }
                    if (instanceFilter.isBlank() || scopeCleared)
                        OutlinedButton(
                            onClick = { scheduling = true },
                            enabled = catalog?.agents?.isNotEmpty() == true,
                        ) {
                            Text("Programmer un agent")
                        }
                    if (
                        !scopeCleared && (agentFilter.isNotBlank() || instanceFilter.isNotBlank())
                    ) {
                        Text(
                            if (agentFilter.isNotBlank())
                                "Agent : ${ruleAgentName(state, agentFilter)}"
                            else
                                "Instance : ${catalog?.hooks?.firstOrNull { wireJson.encodeToJsonElement(it.trigger).text("instanceId") == instanceFilter }?.instanceName ?: "sélectionnée"}",
                            style = MaterialTheme.typography.bodySmall,
                        )
                        TextButton(onClick = { scopeCleared = true }) {
                            Text("Afficher toutes les règles")
                        }
                    }
                    OutlinedTextField(
                        query,
                        { query = it },
                        Modifier.fillMaxWidth(),
                        label = { Text("Rechercher une règle") },
                        singleLine = true,
                        shape = RoundedCornerShape(24.dp),
                    )
                    Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                        listOf("all" to "Toutes", "mine" to "Les miennes", "blocked" to "Arrêtées")
                            .forEach { (id, title) ->
                                FilterChip(
                                    selected = filter == id,
                                    onClick = { filter = id },
                                    label = { Text(title) },
                                )
                            }
                    }
                    if (catalogError.isNotEmpty()) {
                        Text(catalogError, color = MaterialTheme.colorScheme.error)
                        TextButton(onClick = { catalogAttempt++ }) {
                            Text("Recharger le catalogue")
                        }
                    }
                    if (error.isNotEmpty()) Text(error, color = MaterialTheme.colorScheme.error)
                    if (loading) LinearProgressIndicator(Modifier.fillMaxWidth())
                }
                LazyColumn(
                    Modifier.weight(1f),
                    contentPadding = PaddingValues(start = 16.dp, end = 16.dp, bottom = 24.dp),
                    verticalArrangement = Arrangement.spacedBy(10.dp),
                ) {
                    items(visible, key = { it.text("id") }) { rule ->
                        val blocks = rule.list("blocks")
                        Surface(
                            onClick = { selected = rule.text("id") },
                            shape = RoundedCornerShape(22.dp),
                            color =
                                if (selected == rule.text("id") && wide)
                                    MaterialTheme.colorScheme.secondaryContainer
                                else MaterialTheme.colorScheme.surfaceContainerLow,
                            modifier = Modifier.fillMaxWidth(),
                        ) {
                            Column(
                                Modifier.padding(16.dp),
                                verticalArrangement = Arrangement.spacedBy(7.dp),
                            ) {
                                val trigger = describeTrigger(rule.obj("trigger"), catalog)
                                Text(
                                    "${trigger.first} · ${trigger.second}",
                                    style = MaterialTheme.typography.labelMedium,
                                    color = MaterialTheme.colorScheme.primary,
                                )
                                Text(
                                    rule.text("name"),
                                    style = MaterialTheme.typography.titleMedium,
                                    fontWeight = FontWeight.SemiBold,
                                )
                                Text(
                                    if (blocks.isEmpty()) "● Active"
                                    else
                                        "● ${ruleBlockTitles[blocks.first().text("kind")] ?: "Arrêtée"}",
                                    style = MaterialTheme.typography.labelMedium,
                                    color =
                                        if (blocks.isEmpty()) MaterialTheme.colorScheme.primary
                                        else MaterialTheme.colorScheme.error,
                                )
                                Text(
                                    "${ruleAgentName(state, rule.text("executionAgentId"))} · v${rule.text("version")} · ${creatorName(rule.text("creatorAccountId"))}",
                                    style = MaterialTheme.typography.bodySmall,
                                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                                )
                                rule["nextRun"]?.jsonPrimitive?.longOrNull?.let { next ->
                                    Text(
                                        "Prochaine exécution ${upcomingStamp(next)}",
                                        style = MaterialTheme.typography.bodySmall,
                                    )
                                }
                            }
                        }
                    }
                    if (!loading && visible.isEmpty())
                        item {
                            Column(
                                Modifier.fillMaxWidth().padding(24.dp),
                                horizontalAlignment = Alignment.CenterHorizontally,
                                verticalArrangement = Arrangement.spacedBy(12.dp),
                            ) {
                                Text(
                                    if (rules.isEmpty()) "Votre première règle"
                                    else "Aucune règle correspondante",
                                    style = MaterialTheme.typography.titleMedium,
                                )
                                Text(
                                    "Réagissez à un événement avec des étapes visuelles.",
                                    style = MaterialTheme.typography.bodyMedium,
                                )
                                if (rules.isEmpty())
                                    Button(
                                        onClick = { edit("") },
                                        enabled = catalog?.agents?.isNotEmpty() == true,
                                    ) {
                                        Text("Créer une règle")
                                    }
                            }
                        }
                }
            }
            if (wide) {
                VerticalDivider()
                Box(
                    Modifier.weight(1f)
                        .fillMaxHeight()
                        .verticalScroll(rememberScrollState())
                        .padding(24.dp)
                ) {
                    detail?.let {
                        RuleDetailPanel(
                            vm,
                            state,
                            it,
                            catalog,
                            ::changed,
                            edit,
                            openConversation,
                            creatorName(it.text("creatorAccountId")),
                        )
                    } ?: Text("Sélectionnez une règle pour voir son fonctionnement.")
                }
            }
        }
        if (!wide && selected.isNotBlank() && detail != null) {
            ModalBottomSheet(
                onDismissRequest = { selected = "" },
                sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true),
            ) {
                Column(
                    Modifier.fillMaxWidth()
                        .fillMaxHeight(0.94f)
                        .verticalScroll(rememberScrollState())
                        .padding(horizontal = 20.dp)
                        .padding(bottom = 32.dp)
                ) {
                    RuleDetailPanel(
                        vm,
                        state,
                        detail!!,
                        catalog,
                        ::changed,
                        edit,
                        openConversation,
                        creatorName(detail!!.text("creatorAccountId")),
                    )
                }
            }
        }
    }
}

@Composable
internal fun RuleDetailPanel(
    vm: CairnViewModel,
    state: Workspace,
    rule: JsonObject,
    catalog: HookRuleCatalog?,
    changed: (JsonObject?) -> Unit,
    edit: (String) -> Unit,
    openConversation: (String) -> Unit,
    creator: String,
) {
    val scope = rememberCoroutineScope()
    var tab by rememberSaveable(rule.text("id")) { mutableStateOf("flow") }
    var latestTriggering by rememberSaveable(rule.text("id")) { mutableStateOf("") }
    var busy by remember { mutableStateOf(false) }
    var error by remember { mutableStateOf("") }
    var deleting by remember { mutableStateOf(false) }
    var duplicateId by
        rememberSaveable(rule.text("id")) { mutableStateOf(java.util.UUID.randomUUID().toString()) }
    val definition = rule.obj("definition").orEmpty()
    val id = rule.text("id")
    val blocks = rule.list("blocks")
    fun action(path: String, body: JsonObject = JsonObject(emptyMap())) {
        scope.launch {
            busy = true
            error = ""
            try {
                val saved: JsonObject =
                    vm.api.send(
                        "POST",
                        "/hook-rules/${segment(id)}/$path",
                        body.patch("ruleId" to JsonPrimitive(id)),
                    )
                if (path == "duplicate") duplicateId = java.util.UUID.randomUUID().toString()
                changed(saved)
            } catch (e: Exception) {
                if (e is CancellationException) throw e
                error = e.message.orEmpty()
            } finally {
                busy = false
            }
        }
    }
    Column(verticalArrangement = Arrangement.spacedBy(16.dp)) {
        Text(
            definition.text("name"),
            style = MaterialTheme.typography.headlineSmall,
            fontWeight = FontWeight.Bold,
        )
        if (definition.text("description").isNotBlank())
            Text(definition.text("description"), color = MaterialTheme.colorScheme.onSurfaceVariant)
        Text(
            "Version ${rule.text("version")} · créée par $creator",
            style = MaterialTheme.typography.bodySmall,
        )
        val trigger = describeTrigger(definition.obj("trigger"), catalog)
        Surface(
            shape = RoundedCornerShape(18.dp),
            color = MaterialTheme.colorScheme.surfaceContainerLow,
        ) {
            Column(
                Modifier.fillMaxWidth().padding(16.dp),
                verticalArrangement = Arrangement.spacedBy(6.dp),
            ) {
                Text(
                    "QUAND",
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.primary,
                )
                Text(
                    "${trigger.first} · ${trigger.second}",
                    style = MaterialTheme.typography.titleSmall,
                )
                rule["nextRun"]?.jsonPrimitive?.longOrNull?.let { next ->
                    Text(
                        "Prochaine exécution ${upcomingStamp(next)}",
                        style = MaterialTheme.typography.bodySmall,
                    )
                }
                Text(
                    "Avec les droits de ${ruleAgentName(state, definition.text("executionAgentId"))}",
                    style = MaterialTheme.typography.bodySmall,
                )
            }
        }
        blocks.forEach { block ->
            Surface(
                shape = RoundedCornerShape(16.dp),
                color = MaterialTheme.colorScheme.errorContainer,
            ) {
                Column(
                    Modifier.fillMaxWidth().padding(14.dp),
                    verticalArrangement = Arrangement.spacedBy(4.dp),
                ) {
                    Text(
                        ruleBlockTitles[block.text("kind")] ?: block.text("kind"),
                        fontWeight = FontWeight.SemiBold,
                    )
                    Text(block.text("reason"), style = MaterialTheme.typography.bodySmall)
                    Text(
                        ruleBlockLifters[block.text("kind")].orEmpty(),
                        style = MaterialTheme.typography.bodySmall,
                    )
                }
            }
        }
        if (blocks.isEmpty())
            Text(
                "● Active · les événements peuvent déclencher cette règle.",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.primary,
            )
        rule.list("warnings").forEach { warning ->
            Surface(
                shape = RoundedCornerShape(16.dp),
                color = MaterialTheme.colorScheme.tertiaryContainer,
            ) {
                Text(
                    warning.text("message"),
                    Modifier.padding(14.dp),
                    style = MaterialTheme.typography.bodySmall,
                )
            }
        }
        FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            if (rule.allowed("runNow"))
                OutlinedButton(
                    enabled = !busy,
                    onClick = {
                        scope.launch {
                            busy = true
                            error = ""
                            try {
                                latestTriggering = vm.api.runHookRule(HookRuleInput(id)).id
                                tab = "history"
                            } catch (e: Exception) {
                                if (e is CancellationException) throw e
                                error = e.message.orEmpty()
                            } finally {
                                busy = false
                            }
                        }
                    },
                ) {
                    Text("Lancer maintenant")
                }
            if (rule.allowed("edit"))
                Button(onClick = { edit(id) }, enabled = !busy) { Text("Modifier") }
            val disabled = blocks.any { it.text("kind") == "disabled" }
            if (rule.allowed(if (disabled) "enable" else "disable"))
                OutlinedButton(
                    onClick = {
                        action("enabled", ruleObject("enabled" to JsonPrimitive(disabled)))
                    },
                    enabled = !busy,
                ) {
                    Text(if (disabled) "Réactiver" else "Désactiver")
                }
            if (rule.allowed("duplicate"))
                OutlinedButton(
                    onClick = {
                        action(
                            "duplicate",
                            ruleObject("creationId" to JsonPrimitive(duplicateId)),
                        )
                    },
                    enabled = !busy,
                ) {
                    Text("Dupliquer")
                }
            if (rule.allowed("liftBudget") && blocks.any { it.text("kind") == "budgetExhausted" })
                OutlinedButton(onClick = { action("budget") }, enabled = !busy) {
                    Text("Relancer le budget")
                }
            if (rule.allowed("emergencyPause"))
                OutlinedButton(
                    onClick = {
                        action(
                            "emergency-pause",
                            ruleObject(
                                "paused" to
                                    JsonPrimitive(
                                        blocks.none { it.text("kind") == "emergencyPaused" }
                                    )
                            ),
                        )
                    },
                    enabled = !busy,
                ) {
                    Text(
                        if (blocks.any { it.text("kind") == "emergencyPaused" }) "Lever la pause"
                        else "Pause d’urgence"
                    )
                }
            if (rule.allowed("delete"))
                TextButton(onClick = { deleting = true }, enabled = !busy) {
                    Text("Supprimer", color = MaterialTheme.colorScheme.error)
                }
        }
        if (error.isNotEmpty()) Text(error, color = MaterialTheme.colorScheme.error)
        if (busy) LinearProgressIndicator(Modifier.fillMaxWidth())
        TabRow(selectedTabIndex = listOf("flow", "history", "versions").indexOf(tab)) {
            listOf("flow" to "Arbre", "history" to "Historique", "versions" to "Versions")
                .forEach { (key, title) ->
                    Tab(selected = tab == key, onClick = { tab = key }, text = { Text(title) })
                }
        }
        when (tab) {
            "flow" ->
                RuleFlowTree(
                    definition.list("steps"),
                    "/steps",
                    catalog,
                    { ruleAgentName(state, it) },
                )
            "history" -> key(latestTriggering) { RuleHistoryPanel(vm, id, openConversation) }
            "versions" ->
                RuleVersionsPanel(vm, id, catalog, { ruleAgentName(state, it) }, openConversation)
        }
    }
    if (deleting)
        AlertDialog(
            onDismissRequest = { deleting = false },
            title = { Text("Supprimer cette règle ?") },
            text = {
                Text("Ses versions, ses valeurs mémorisées et son historique seront supprimés.")
            },
            confirmButton = {
                TextButton(
                    onClick = {
                        deleting = false
                        scope.launch {
                            try {
                                vm.api.request("DELETE", "/hook-rules/${segment(id)}")
                                changed(null)
                            } catch (e: Exception) {
                                if (e is CancellationException) throw e
                                error = e.message.orEmpty()
                            }
                        }
                    }
                ) {
                    Text("Supprimer")
                }
            },
            dismissButton = { TextButton(onClick = { deleting = false }) { Text("Annuler") } },
        )
}

@Composable
private fun RuleVersionsPanel(
    vm: CairnViewModel,
    id: String,
    catalog: HookRuleCatalog?,
    agentName: (String) -> String,
    openConversation: (String) -> Unit,
) {
    var versions by remember(id) { mutableStateOf<List<JsonElement>>(emptyList()) }
    var error by remember { mutableStateOf("") }
    LaunchedEffect(id) {
        try {
            versions =
                vm.api.get<JsonObject>("/hook-rules/${segment(id)}/versions").list("versions")
        } catch (e: Exception) {
            if (e is CancellationException) throw e
            error = e.message.orEmpty()
        }
    }
    Column(verticalArrangement = Arrangement.spacedBy(10.dp)) {
        if (error.isNotEmpty()) Text(error, color = MaterialTheme.colorScheme.error)
        versions.forEach { version ->
            Surface(
                shape = RoundedCornerShape(16.dp),
                color = MaterialTheme.colorScheme.surfaceContainerLow,
            ) {
                Column(Modifier.fillMaxWidth().padding(14.dp)) {
                    Text("Version ${version.text("version")}", fontWeight = FontWeight.SemiBold)
                    Text(
                        relativeStamp(
                            version.jsonObject["savedAt"]?.jsonPrimitive?.longOrNull ?: 0
                        ),
                        style = MaterialTheme.typography.bodySmall,
                    )
                    val source =
                        when (version.text("source")) {
                            "installationAgent" -> "par un agent"
                            "publicMcp" -> "depuis le MCP public"
                            "duplicate" -> "par duplication"
                            "template" -> "depuis un modèle"
                            "migration" -> "depuis une mission migrée"
                            else -> "depuis les applications"
                        }
                    Text("Enregistrée $source", style = MaterialTheme.typography.bodySmall)
                    if (version.text("conversationId").isNotBlank())
                        TextButton(onClick = { openConversation(version.text("conversationId")) }) {
                            Text("Ouvrir la conversation d’origine")
                        }
                    version.obj("definition")?.let {
                        Text(it.text("name"))
                        RuleFlowTree(it.list("steps"), "/steps", catalog, agentName)
                    }
                }
            }
        }
    }
}

@Composable
internal fun RuleHistoryPanel(
    vm: CairnViewModel,
    ruleId: String,
    openConversation: (String) -> Unit,
) {
    var entries by remember(ruleId) { mutableStateOf<List<JsonElement>>(emptyList()) }
    var quiet by remember(ruleId) { mutableLongStateOf(0) }
    var refused by remember(ruleId) { mutableLongStateOf(0) }
    var cursor by remember(ruleId) { mutableStateOf<String?>(null) }
    var paged by remember(ruleId) { mutableStateOf(false) }
    var loading by remember(ruleId) { mutableStateOf(false) }
    var expanded by rememberSaveable(ruleId) { mutableStateOf("") }
    var detail by remember(ruleId) { mutableStateOf<JsonObject?>(null) }
    var error by remember { mutableStateOf("") }
    val scope = rememberCoroutineScope()
    suspend fun load(more: Boolean = false) {
        if (loading) return
        loading = true
        try {
            val output: JsonObject =
                vm.api.get(
                    "/hook-rules/${segment(ruleId)}/triggerings" +
                        if (more && cursor != null) "?cursor=${segment(cursor!!)}" else ""
                )
            if (more) paged = true
            entries =
                (if (more) entries + output.list("triggerings") else output.list("triggerings"))
                    .distinctBy { it.text("id") }
            quiet = output["withoutEffect"]?.jsonPrimitive?.longOrNull ?: 0
            refused = output["notAdmitted"]?.jsonPrimitive?.longOrNull ?: 0
            cursor = output["nextCursor"]?.jsonPrimitive?.contentOrNull
            error = ""
        } catch (e: Exception) {
            if (e is CancellationException) throw e
            error = e.message.orEmpty()
        } finally {
            loading = false
        }
    }
    suspend fun loadDetail(id: String) {
        try {
            val loaded: JsonObject = vm.api.get("/hook-rule-triggerings/${segment(id)}")
            if (expanded == id) {
                detail = loaded
                entries = entries.map { if (it.text("id") == id) loaded else it }
                error = ""
            }
        } catch (e: Exception) {
            if (e is CancellationException) throw e
            if (expanded == id) error = e.message.orEmpty()
        }
    }
    Poll("rule-history:${vm.api.cacheScope}:$ruleId", 5000) {
        if (expanded.isEmpty() && !paged) load()
        else if (detail.text("status") in setOf("running", "waiting", "retrying"))
            loadDetail(expanded)
    }
    LaunchedEffect(expanded) {
        detail = null
        if (expanded.isNotBlank()) loadDetail(expanded)
    }
    Column(verticalArrangement = Arrangement.spacedBy(10.dp)) {
        if (quiet > 0)
            Text(
                "$quiet événements sans action ces dernières 24 h. Ils ne consomment pas le budget.",
                style = MaterialTheme.typography.bodySmall,
            )
        if (refused > 0)
            Text(
                "$refused événements non admis : capacité ou délai d’admission dépassé.",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.error,
            )
        if (error.isNotBlank()) Text(error, color = MaterialTheme.colorScheme.error)
        if (entries.isEmpty())
            Text(
                "Les déclenchements apparaîtront ici lorsqu’un événement fera agir la règle.",
                style = MaterialTheme.typography.bodyMedium,
            )
        entries.forEach { entry ->
            val id = entry.text("id")
            Surface(
                shape = RoundedCornerShape(18.dp),
                color = MaterialTheme.colorScheme.surfaceContainerLow,
            ) {
                Column(
                    Modifier.fillMaxWidth().padding(14.dp),
                    verticalArrangement = Arrangement.spacedBy(8.dp),
                ) {
                    Row(
                        Modifier.fillMaxWidth().clickable {
                            expanded = if (expanded == id) "" else id
                        },
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        Column(Modifier.weight(1f)) {
                            Text(
                                "${triggeringStatusTitles[entry.text("status")] ?: entry.text("status")} · ${entry.text("actions")} actions",
                                fontWeight = FontWeight.SemiBold,
                            )
                            Text(
                                "v${entry.text("version")} · ${relativeStamp(entry.jsonObject["createdAt"]?.jsonPrimitive?.longOrNull ?: 0)}",
                                style = MaterialTheme.typography.bodySmall,
                            )
                        }
                        Text(if (expanded == id) "⌃" else "⌄")
                    }
                    (entry.jsonObject["resumesAt"] as? JsonPrimitive)
                        ?.longOrNull
                        ?.takeIf { entry.text("status") != "completed" }
                        ?.let { at ->
                            Text(
                                "Reprise : ${upcomingStamp(at)}",
                                style = MaterialTheme.typography.bodySmall,
                            )
                        }
                    if (entry.text("error").isNotBlank())
                        Text(
                            entry.text("error"),
                            color = MaterialTheme.colorScheme.error,
                            style = MaterialTheme.typography.bodySmall,
                        )
                    if (expanded == id) {
                        val loaded = detail
                        if (loaded == null) CircularProgressIndicator(Modifier.size(20.dp))
                        else if (loaded["detailsVisible"].flag() != true)
                            Text(
                                "Le détail est réservé au créateur, au propriétaire et aux personnes qui peuvent utiliser l’agent d’exécution.",
                                style = MaterialTheme.typography.bodySmall,
                            )
                        else {
                            val event = loaded.obj("event")
                            val migrated =
                                event
                                    ?.takeIf {
                                        it.text("hook") == "clock" &&
                                            it.text("instanceId").isBlank()
                                    }
                                    ?.obj("payload")
                                    ?.obj("migratedConversation")
                            val conversationId = migrated.text("id")
                            if (conversationId.isNotBlank()) {
                                TextButton(onClick = { openConversation(conversationId) }) {
                                    Text("Ouvrir la conversation · ${migrated.text("title")}")
                                }
                            }
                            loaded.list("steps").forEachIndexed { index, step ->
                                Text(
                                    "${index + 1}. ${step.text("label").ifBlank { step.text("nodeId") }} · ${triggeringStatusTitles[step.text("status")] ?: step.text("status")}",
                                    style = MaterialTheme.typography.labelLarge,
                                )
                                (step.jsonObject["attempts"] as? JsonPrimitive)
                                    ?.intOrNull
                                    ?.takeIf { it > 1 }
                                    ?.let { attempts ->
                                        Text(
                                            "$attempts tentatives",
                                            style = MaterialTheme.typography.bodySmall,
                                        )
                                    }
                                if (step.text("error").isNotEmpty())
                                    Text(
                                        step.text("error"),
                                        color = MaterialTheme.colorScheme.error,
                                    )
                                val started = step.obj("result").text("conversationId")
                                if (
                                    step.text("kind") == "startAgent" &&
                                        step.text("status") == "completed" &&
                                        started.isNotBlank()
                                ) {
                                    TextButton(onClick = { openConversation(started) }) {
                                        Text("Ouvrir la conversation")
                                    }
                                }
                                RuleDataDisclosure("Entrée", step.jsonObject["input"])
                                RuleDataDisclosure("Résultat", step.jsonObject["result"])
                            }
                            RuleDataDisclosure(
                                if (migrated == null) "Événement reçu" else "Exécution d’origine",
                                loaded["event"],
                            )
                        }
                    }
                }
            }
        }
        if (cursor != null)
            OutlinedButton(onClick = { scope.launch { load(true) } }, enabled = !loading) {
                Text("Voir les plus anciens")
            }
    }
}

@Composable
private fun RuleDataDisclosure(title: String, value: JsonElement?) {
    if (value == null || value is JsonNull) return
    var open by remember { mutableStateOf(false) }
    Column {
        TextButton(onClick = { open = !open }) { Text(if (open) "$title ⌃" else "$title ⌄") }
        if (open)
            SelectionContainer {
                Text(
                    wireJson.encodeToString(JsonElement.serializer(), value),
                    style = MaterialTheme.typography.bodySmall,
                )
            }
    }
}

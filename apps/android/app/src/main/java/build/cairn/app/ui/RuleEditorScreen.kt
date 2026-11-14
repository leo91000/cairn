@file:OptIn(
    androidx.compose.material3.ExperimentalMaterial3Api::class,
    androidx.compose.foundation.layout.ExperimentalLayoutApi::class,
)

package build.cairn.app.ui

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import build.cairn.app.data.*
import build.cairn.contracts.HookRuleCatalog
import build.cairn.contracts.HookRuleProblem
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.serialization.json.*

private fun emptyRuleDefinition(
    catalog: HookRuleCatalog,
    agent: String,
    instance: String,
): JsonObject {
    val hook =
        catalog.hooks.firstOrNull {
            wireJson.encodeToJsonElement(it.trigger).text("instanceId") == instance &&
                instance.isNotBlank()
        }
            ?: catalog.hooks.firstOrNull {
                wireJson.encodeToJsonElement(it.trigger).text("type") == "plugin"
            }
            ?: catalog.hooks.firstOrNull()
    val chosen =
        if (agent.isNotBlank() && hook?.agentIds?.contains(agent) == true) agent
        else hook?.agentIds?.firstOrNull() ?: catalog.agents.firstOrNull()?.agentId.orEmpty()
    return ruleObject(
        "name" to JsonPrimitive(""),
        "description" to JsonPrimitive(""),
        "trigger" to
            (hook?.let { wireJson.encodeToJsonElement(it.trigger) }
                ?: ruleObject(
                    "type" to JsonPrimitive("cairn"),
                    "hook" to JsonPrimitive("replyReady"),
                )),
        "executionAgentId" to JsonPrimitive(chosen),
        "steps" to JsonArray(emptyList()),
        "limits" to wireJson.encodeToJsonElement(catalog.limits.defaultLimits),
    )
}

@Composable
fun RuleEditorScreen(
    vm: CairnViewModel,
    state: Workspace,
    ruleId: String,
    agentFilter: String = "",
    instanceFilter: String = "",
    saved: (String) -> Unit,
    back: () -> Unit,
) {
    val cacheScope = vm.api.cacheScope
    var draft by rememberSaveable(cacheScope, ruleId) { mutableStateOf("") }
    var original by rememberSaveable(cacheScope, ruleId) { mutableStateOf("") }
    var revision by rememberSaveable(cacheScope, ruleId) { mutableStateOf("") }
    val creationId = rememberSaveable(cacheScope, ruleId) { java.util.UUID.randomUUID().toString() }
    var selected by rememberSaveable(cacheScope, ruleId) { mutableStateOf("") }
    var catalog by remember(cacheScope) { mutableStateOf<HookRuleCatalog?>(null) }
    var loading by remember { mutableStateOf(true) }
    var checking by remember { mutableStateOf(false) }
    var checked by remember { mutableStateOf(false) }
    var saving by remember { mutableStateOf(false) }
    var canEdit by remember { mutableStateOf(true) }
    var problems by remember { mutableStateOf<List<HookRuleProblem>>(emptyList()) }
    var warnings by remember { mutableStateOf<List<String>>(emptyList()) }
    var error by remember { mutableStateOf("") }
    var discard by remember { mutableStateOf(false) }
    var removing by remember { mutableStateOf<String?>(null) }
    val scope = rememberCoroutineScope()
    val definition =
        remember(draft) {
            if (draft.isBlank()) null else wireJson.parseToJsonElement(draft).jsonObject
        }
    val dirty = draft.isNotBlank() && draft != original
    fun leave() {
        if (dirty) discard = true else back()
    }
    BackHandler { leave() }
    LaunchedEffect(ruleId, cacheScope) {
        loading = true
        try {
            val loaded: HookRuleCatalog = vm.api.get("/hook-rules/catalog")
            catalog = loaded
            val rule: JsonObject? =
                if (ruleId.isBlank()) null else vm.api.get("/hook-rules/${segment(ruleId)}")
            canEdit = rule == null || rule.allowed("edit")
            if (draft.isBlank()) {
                draft =
                    (rule?.obj("definition")
                            ?: emptyRuleDefinition(loaded, agentFilter, instanceFilter))
                        .toString()
                original = draft
                revision = rule?.text("revision").orEmpty()
            }
        } catch (e: Exception) {
            if (e is CancellationException) throw e
            error = e.message.orEmpty()
        } finally {
            loading = false
        }
    }
    LaunchedEffect(draft, loading) {
        checked = false
        if (loading || definition == null) return@LaunchedEffect
        checking = true
        delay(350)
        try {
            val result: JsonObject =
                vm.api.send(
                    "POST",
                    "/hook-rules/validate",
                    ruleObject(
                        "definition" to definition,
                        "ruleId" to ruleId.takeIf { it.isNotBlank() }?.let(::JsonPrimitive),
                    ),
                )
            problems =
                result.list("problems").map { wireJson.decodeFromJsonElement<HookRuleProblem>(it) }
            warnings = result.list("warnings").map { it.text("message") }
            checked = true
            checking = false
        } catch (e: Exception) {
            if (e is CancellationException) throw e
            checking = false
            error = e.message.orEmpty()
        }
    }
    fun update(next: JsonElement) {
        draft = next.toString()
        checked = false
    }
    fun save() {
        if (definition == null || saving || !checked || problems.isNotEmpty() || !canEdit) return
        saving = true
        error = ""
        scope.launch {
            try {
                val result: JsonObject =
                    if (ruleId.isBlank())
                        vm.api.send(
                            "POST",
                            "/hook-rules",
                            ruleObject(
                                "creationId" to JsonPrimitive(creationId),
                                "definition" to definition,
                                "enabled" to JsonPrimitive(true),
                            ),
                        )
                    else
                        vm.api.send(
                            "PUT",
                            "/hook-rules/${segment(ruleId)}",
                            ruleObject(
                                "revision" to JsonPrimitive(revision),
                                "definition" to definition,
                            ),
                        )
                original = draft
                vm.notify(if (ruleId.isBlank()) "Règle créée" else "Nouvelle version enregistrée")
                saved(result.text("id"))
            } catch (e: Exception) {
                if (e is CancellationException) throw e
                error = e.message.orEmpty()
            } finally {
                saving = false
            }
        }
    }
    Column(Modifier.fillMaxSize().testTag("rule-editor")) {
        Row(
            Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 8.dp),
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            TextButton(onClick = ::leave) { Text("‹ Règles") }
            Spacer(Modifier.weight(1f))
            Button(
                onClick = ::save,
                enabled = canEdit && checked && problems.isEmpty() && !saving && !loading,
            ) {
                Text(
                    if (saving) "Enregistrement…"
                    else if (ruleId.isBlank()) "Créer" else "Enregistrer"
                )
            }
        }
        HorizontalDivider()
        if (loading) LinearProgressIndicator(Modifier.fillMaxWidth())
        if (error.isNotBlank())
            Text(error, Modifier.padding(16.dp), color = MaterialTheme.colorScheme.error)
        if (!canEdit)
            Text(
                "Seul le créateur peut modifier cette règle. Dupliquez-la pour en créer une à vous.",
                Modifier.padding(16.dp),
                color = MaterialTheme.colorScheme.error,
            )
        if (definition != null && catalog != null && canEdit) {
            val currentCatalog = catalog!!
            val hook =
                currentCatalog.hooks.firstOrNull {
                    triggerKey(wireJson.encodeToJsonElement(it.trigger)) ==
                        triggerKey(definition.obj("trigger"))
                }
            val agentChoices =
                currentCatalog.agents.filter { hook == null || it.agentId in hook.agentIds }
            Column(
                Modifier.weight(1f)
                    .verticalScroll(rememberScrollState())
                    .padding(horizontal = 20.dp, vertical = 16.dp),
                verticalArrangement = Arrangement.spacedBy(18.dp),
            ) {
                Text(
                    if (ruleId.isBlank()) "Créer une règle" else "Modifier la règle",
                    style = MaterialTheme.typography.headlineSmall,
                    fontWeight = FontWeight.Bold,
                )
                Field(
                    "Nom de la règle",
                    definition.text("name"),
                    { update(definition.patch("name" to JsonPrimitive(it))) },
                )
                Text(
                    if (checking || !checked) "Vérification…"
                    else if (problems.isEmpty()) "● Prête à être enregistrée"
                    else "${problems.size} points à corriger",
                    color =
                        if (checked && problems.isNotEmpty()) MaterialTheme.colorScheme.error
                        else MaterialTheme.colorScheme.primary,
                    style = MaterialTheme.typography.labelMedium,
                )
                Surface(
                    shape = RoundedCornerShape(20.dp),
                    color = MaterialTheme.colorScheme.primaryContainer.copy(alpha = 0.35f),
                ) {
                    Column(
                        Modifier.padding(16.dp),
                        verticalArrangement = Arrangement.spacedBy(12.dp),
                    ) {
                        Text("QUAND CECI ARRIVE", style = MaterialTheme.typography.labelSmall)
                        Choice(
                            "Événement",
                            triggerKey(definition.obj("trigger")),
                            currentCatalog.hooks.map {
                                triggerKey(wireJson.encodeToJsonElement(it.trigger)) to it.title
                            },
                        ) { key ->
                            val next =
                                currentCatalog.hooks.first {
                                    triggerKey(wireJson.encodeToJsonElement(it.trigger)) == key
                                }
                            val agent =
                                definition.text("executionAgentId").takeIf { it in next.agentIds }
                                    ?: next.agentIds.firstOrNull().orEmpty()
                            update(
                                definition.patch(
                                    "trigger" to wireJson.encodeToJsonElement(next.trigger),
                                    "executionAgentId" to JsonPrimitive(agent),
                                )
                            )
                        }
                        if (hook?.description?.isNotBlank() == true)
                            Text(hook.description, style = MaterialTheme.typography.bodySmall)
                        Choice(
                            "Avec les droits de",
                            definition.text("executionAgentId"),
                            agentChoices.map { it.agentId to it.name },
                        ) {
                            update(definition.patch("executionAgentId" to JsonPrimitive(it)))
                        }
                    }
                }
                Text(
                    "FAIRE CELA",
                    style = MaterialTheme.typography.labelMedium,
                    color = MaterialTheme.colorScheme.primary,
                )
                RuleFlowTree(
                    definition.list("steps"),
                    "/steps",
                    currentCatalog,
                    { ruleAgentName(state, it) },
                    selected,
                    canEdit,
                    select = { selected = it },
                    insert = { container, index, kind ->
                        update(
                            definition.inserted(
                                container,
                                index,
                                newRuleStep(definition, kind, currentCatalog),
                            )
                        )
                        selected = "$container/$index"
                    },
                    remove = { removing = it },
                    move = { path, offset ->
                        update(definition.moved(path, offset))
                        selected = ""
                    },
                )
                Text("Limites et description", style = MaterialTheme.typography.titleMedium)
                Field(
                    "Description (facultative)",
                    definition.text("description"),
                    { update(definition.patch("description" to JsonPrimitive(it))) },
                    lines = 2,
                )
                val limits = definition.obj("limits").orEmpty()
                Field(
                    "Déclenchements par heure",
                    limits.text("triggeringsPerHour"),
                    {
                        update(
                            definition.patch(
                                "limits" to
                                    limits.patch(
                                        "triggeringsPerHour" to
                                            (it.toLongOrNull()?.let(::JsonPrimitive)
                                                ?: JsonPrimitive(it))
                                    )
                            )
                        )
                    },
                )
                Field(
                    "Conversations démarrées ou orientées par heure",
                    limits.text("conversationsPerHour"),
                    {
                        update(
                            definition.patch(
                                "limits" to
                                    limits.patch(
                                        "conversationsPerHour" to
                                            (it.toLongOrNull()?.let(::JsonPrimitive)
                                                ?: JsonPrimitive(it))
                                    )
                            )
                        )
                    },
                )
                Text(
                    "Au dépassement, la règle s’arrête et son créateur est prévenu. Les déclenchements déjà comptés peuvent finir.",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                problems.forEach { problem ->
                    Text(
                        problem.message,
                        color = MaterialTheme.colorScheme.error,
                        style = MaterialTheme.typography.bodySmall,
                    )
                }
                warnings.forEach { warning ->
                    Text(
                        warning,
                        color = MaterialTheme.colorScheme.tertiary,
                        style = MaterialTheme.typography.bodySmall,
                    )
                }
                Spacer(Modifier.height(16.dp))
            }
            val node = definition.at(selected) as? JsonObject
            if (selected.isNotBlank() && node != null && canEdit)
                ModalBottomSheet(
                    onDismissRequest = { selected = "" },
                    sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true),
                ) {
                    val peopleField =
                        hook?.resourceFields?.firstOrNull {
                            it.resourceType.contains("person", true) ||
                                it.resourceType.contains("user", true) ||
                                it.path.contains("author", true)
                        }
                    val source =
                        if (hook?.pluginId != null && peopleField != null)
                            RuleResourceSource(
                                hook.pluginId!!,
                                definition.obj("trigger").text("instanceId"),
                                peopleField.resourceType,
                            )
                        else null
                    Column(
                        Modifier.fillMaxWidth()
                            .fillMaxHeight(0.92f)
                            .verticalScroll(rememberScrollState())
                            .imePadding()
                            .padding(horizontal = 20.dp)
                            .padding(bottom = 28.dp)
                    ) {
                        RuleStepPanel(
                            vm,
                            state,
                            node,
                            currentCatalog,
                            definition.text("executionAgentId"),
                            ruleVariablesAt(definition, selected, currentCatalog),
                            source,
                            { update(definition.with(selected, it)) },
                        )
                        problemsUnder(problems, selected).forEach {
                            Text(
                                it.message,
                                color = MaterialTheme.colorScheme.error,
                                modifier = Modifier.padding(top = 8.dp),
                            )
                        }
                        Button(
                            onClick = { selected = "" },
                            modifier = Modifier.fillMaxWidth().padding(top = 16.dp),
                        ) {
                            Text("Terminer cette étape")
                        }
                    }
                }
        }
    }
    if (discard)
        AlertDialog(
            onDismissRequest = { discard = false },
            title = { Text("Quitter sans enregistrer ?") },
            text = { Text("Les changements de cette règle seront perdus.") },
            confirmButton = {
                TextButton(
                    onClick = {
                        discard = false
                        back()
                    }
                ) {
                    Text("Quitter")
                }
            },
            dismissButton = {
                TextButton(onClick = { discard = false }) { Text("Continuer à modifier") }
            },
        )
    if (removing != null && definition != null)
        AlertDialog(
            onDismissRequest = { removing = null },
            title = { Text("Supprimer cette étape ?") },
            text = { Text("Les chemins qu’elle contient seront aussi supprimés.") },
            confirmButton = {
                TextButton(
                    onClick = {
                        update(definition.with(removing!!, null))
                        selected = ""
                        removing = null
                    }
                ) {
                    Text("Supprimer")
                }
            },
            dismissButton = { TextButton(onClick = { removing = null }) { Text("Annuler") } },
        )
}

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
import build.cairn.contracts.HookRuleTrigger
import build.cairn.contracts.RuleCatalogHook
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlinx.serialization.json.*

internal fun rulePeopleSource(hook: RuleCatalogHook?, definition: JsonObject): RuleResourceSource? {
    val pluginId = hook?.pluginId ?: return null
    val field =
        hook.resourceFields.firstOrNull {
            it.resourceType.contains("person", true) ||
                it.resourceType.contains("user", true) ||
                it.path.contains("author", true) ||
                it.path.contains("sender", true)
        } ?: return null
    return RuleResourceSource(
        pluginId,
        definition.obj("trigger").text("instanceId"),
        field.resourceType,
        definition.text("executionAgentId"),
    )
}

private fun emptyRuleDefinition(
    catalog: HookRuleCatalog,
    agent: String,
    instance: String,
): JsonObject {
    val availableHooks =
        if (agent.isNotBlank()) catalog.hooks.filter { agent in it.agentIds } else catalog.hooks
    val hook =
        availableHooks.firstOrNull {
            wireJson.encodeToJsonElement(it.trigger).text("instanceId") == instance &&
                instance.isNotBlank()
        }
            ?: availableHooks.firstOrNull {
                wireJson.encodeToJsonElement(it.trigger).text("type") == "plugin"
            }
            ?: availableHooks.firstOrNull()
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
    val cacheScope = "${vm.api.cacheScope}:${state.session.account?.id.orEmpty()}"
    val draftSession =
        rememberSaveable(cacheScope, ruleId) { java.util.UUID.randomUUID().toString() }
    val retained =
        rememberSaveable(
            cacheScope,
            ruleId,
            draftSession,
            saver = vm.ruleDrafts.saver(cacheScope, draftSession),
        ) {
            vm.ruleDrafts.fresh(cacheScope, draftSession)
        }
    LaunchedEffect(cacheScope) { withContext(Dispatchers.IO) { vm.ruleDrafts.sweep() } }
    var draft by retained.draft
    var original by retained.original
    var revision by rememberSaveable(cacheScope, ruleId) { mutableStateOf("") }
    var loadedVersion by rememberSaveable(cacheScope, ruleId) { mutableIntStateOf(0) }
    var conflict by remember(cacheScope, ruleId) { mutableStateOf<JsonObject?>(null) }
    var comparingConflict by remember { mutableStateOf(false) }
    var recoveredRuleId by rememberSaveable(cacheScope, ruleId) { mutableStateOf("") }
    var creationDefinition by retained.creation
    val targetRuleId = ruleId.ifBlank { recoveredRuleId }
    val creationId = rememberSaveable(cacheScope, ruleId) { java.util.UUID.randomUUID().toString() }
    var selected by rememberSaveable(cacheScope, ruleId) { mutableStateOf("") }
    var configuring by rememberSaveable(cacheScope, ruleId) { mutableStateOf(ruleId.isBlank()) }
    var catalog by remember(cacheScope) { mutableStateOf<HookRuleCatalog?>(null) }
    var loading by remember { mutableStateOf(true) }
    var checking by remember { mutableStateOf(false) }
    var checked by remember { mutableStateOf(false) }
    var validationError by remember { mutableStateOf("") }
    var validationAttempt by remember { mutableIntStateOf(0) }
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
    val localJsonProblems = definition?.let(::ruleJsonDraftProblems).orEmpty()
    val dirty = draft.isNotBlank() && draft != original
    fun leave() {
        if (dirty) discard = true
        else {
            vm.ruleDrafts.discard(cacheScope, draftSession, retained)
            back()
        }
    }
    BackHandler { leave() }
    LaunchedEffect(ruleId, cacheScope) {
        loading = true
        try {
            val loaded: HookRuleCatalog = vm.api.get("/hook-rules/catalog")
            catalog = loaded
            val rule: JsonObject? =
                if (targetRuleId.isBlank()) null
                else vm.api.get("/hook-rules/${segment(targetRuleId)}")
            if (rule == null && loaded.agents.isEmpty()) {
                error =
                    "Un accès à un agent est nécessaire pour créer une règle. Demandez-le au propriétaire."
                return@LaunchedEffect
            }
            canEdit = rule == null || rule.allowed("edit")
            if (draft.isBlank()) {
                draft =
                    (rule?.obj("definition")
                            ?: emptyRuleDefinition(loaded, agentFilter, instanceFilter))
                        .toString()
                original = draft
                revision = rule?.text("revision").orEmpty()
                loadedVersion = rule?.text("version")?.toIntOrNull() ?: 0
            }
        } catch (e: Exception) {
            if (e is CancellationException) throw e
            error = e.message.orEmpty()
        } finally {
            loading = false
        }
    }
    LaunchedEffect(draft, loading, validationAttempt) {
        checked = false
        validationError = ""
        if (loading || definition == null || !canEdit) return@LaunchedEffect
        checking = true
        if (localJsonProblems.isNotEmpty()) {
            problems = localJsonProblems
            warnings = emptyList()
            checked = true
            checking = false
            return@LaunchedEffect
        }
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
            validationError = e.message.orEmpty().ifBlank { "La vérification n’a pas abouti." }
        }
    }
    fun update(next: JsonElement) {
        if (!canEdit) return
        draft = next.toString()
        checked = false
    }
    fun save() {
        if (
            definition == null ||
                saving ||
                !checked ||
                problems.isNotEmpty() ||
                localJsonProblems.isNotEmpty() ||
                !canEdit
        )
            return
        val submittedDraft = draft
        val submittedDefinition = wireJson.parseToJsonElement(submittedDraft).jsonObject
        saving = true
        error = ""
        scope.launch {
            try {
                suspend fun persist(): JsonObject =
                    if (targetRuleId.isBlank()) {
                        if (creationDefinition.isBlank()) creationDefinition = submittedDraft
                        try {
                            vm.api.send(
                                "POST",
                                "/hook-rules",
                                ruleObject(
                                    "creationId" to JsonPrimitive(creationId),
                                    "definition" to wireJson.parseToJsonElement(creationDefinition),
                                    "enabled" to JsonPrimitive(true),
                                ),
                            )
                        } catch (cause: ApiException) {
                            if (cause.status in 400..499 && cause.status != 408)
                                creationDefinition = ""
                            throw cause
                        }
                    } else
                        vm.api.send(
                            "PUT",
                            "/hook-rules/${segment(targetRuleId)}",
                            ruleObject(
                                "ruleId" to JsonPrimitive(targetRuleId),
                                "revision" to JsonPrimitive(revision),
                                "definition" to submittedDefinition,
                            ),
                        )
                val result =
                    try {
                        persist()
                    } catch (cause: ApiException) {
                        if (cause.status != 409 || targetRuleId.isBlank()) throw cause
                        val latest: JsonObject = vm.api.get("/hook-rules/${segment(targetRuleId)}")
                        if (
                            sameRuleDefinition(
                                latest.obj("definition").orEmpty(),
                                submittedDefinition,
                            )
                        ) {
                            latest
                        } else {
                            if (!latest.allowed("edit")) {
                                canEdit = false
                                selected = ""
                                error =
                                    "Vous ne pouvez plus modifier cette règle. Votre brouillon est conservé."
                                return@launch
                            }
                            if (latest.text("version").toIntOrNull() != loadedVersion) {
                                conflict = latest
                                return@launch
                            }
                            revision = latest.text("revision")
                            persist()
                        }
                    }
                check(
                    result.text("id").isNotBlank() &&
                        result.obj("definition") != null &&
                        result.obj("allowed")?.get("edit").flag() != null
                ) {
                    "La réponse de sauvegarde est incomplète. Votre brouillon est conservé ; réessayez."
                }
                if (
                    !sameRuleDefinition(
                        result.obj("definition").orEmpty(),
                        wireJson.parseToJsonElement(draft).jsonObject,
                    )
                ) {
                    recoveredRuleId = result.text("id")
                    revision = result.text("revision")
                    loadedVersion = result.text("version").toIntOrNull() ?: 0
                    canEdit = result.allowed("edit")
                    conflict = if (canEdit) result else null
                    error =
                        "La sauvegarde a réussi. Vos dernières modifications sont conservées ; enregistrez-les dans la version suivante."
                    return@launch
                }
                original = draft
                vm.notify(
                    if (targetRuleId.isBlank()) "Règle créée" else "Nouvelle version enregistrée"
                )
                vm.ruleDrafts.discard(cacheScope, draftSession, retained)
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
                enabled =
                    canEdit &&
                        checked &&
                        problems.isEmpty() &&
                        localJsonProblems.isEmpty() &&
                        !saving &&
                        !loading &&
                        conflict == null,
            ) {
                Text(
                    if (saving) "Enregistrement…"
                    else if (targetRuleId.isBlank()) "Créer" else "Enregistrer"
                )
            }
        }
        HorizontalDivider()
        if (loading) LinearProgressIndicator(Modifier.fillMaxWidth())
        if (error.isNotBlank())
            Text(error, Modifier.padding(16.dp), color = MaterialTheme.colorScheme.error)
        if (retained.problem.value.isNotBlank()) {
            Text(
                retained.problem.value,
                Modifier.padding(16.dp),
                color = MaterialTheme.colorScheme.error,
            )
            CopyButton("Copier le brouillon", draft, sensitive = true)
        }
        if (!canEdit)
            Text(
                "Seul le créateur peut modifier cette règle. Dupliquez-la pour en créer une à vous.",
                Modifier.padding(16.dp),
                color = MaterialTheme.colorScheme.error,
            )
        conflict?.let { latest ->
            Column(
                Modifier.heightIn(max = 300.dp)
                    .verticalScroll(rememberScrollState())
                    .padding(16.dp),
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                Text(
                    "La version ${latest.text("version")} a été enregistrée pendant votre modification. Votre brouillon est conservé."
                )
                TextButton(onClick = { comparingConflict = !comparingConflict }) {
                    Text("Voir la version enregistrée")
                }
                if (comparingConflict)
                    RuleFlowTree(
                        latest.obj("definition").list("steps"),
                        "/steps",
                        catalog,
                        { ruleAgentName(state, it) },
                    )
                TextButton(
                    onClick = {
                        draft = latest.obj("definition").toString()
                        original = draft
                        revision = latest.text("revision")
                        loadedVersion = latest.text("version").toIntOrNull() ?: 0
                        conflict = null
                        selected = ""
                    }
                ) {
                    Text("Utiliser la version enregistrée")
                }
                Button(
                    onClick = {
                        revision = latest.text("revision")
                        loadedVersion = latest.text("version").toIntOrNull() ?: 0
                        conflict = null
                        save()
                    },
                    enabled =
                        checked && problems.isEmpty() && localJsonProblems.isEmpty() && !saving,
                ) {
                    Text("Enregistrer mon brouillon à la suite")
                }
            }
        }
        if (definition != null && catalog != null) {
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
                    if (targetRuleId.isBlank()) "Créer une règle" else "Modifier la règle",
                    style = MaterialTheme.typography.headlineSmall,
                    fontWeight = FontWeight.Bold,
                )
                Field(
                    "Nom de la règle",
                    definition.text("name"),
                    { update(definition.patch("name" to JsonPrimitive(it))) },
                    enabled = canEdit,
                )
                if (!canEdit) CopyButton("Copier le brouillon", draft, sensitive = true)
                Text(
                    if (!canEdit) "Brouillon conservé en lecture seule"
                    else if (validationError.isNotEmpty()) "Vérification impossible"
                    else if (checking || !checked) "Vérification…"
                    else if (problems.isEmpty()) "● Prête à être enregistrée"
                    else "${problems.size} points à corriger",
                    color =
                        if (checked && problems.isNotEmpty()) MaterialTheme.colorScheme.error
                        else MaterialTheme.colorScheme.primary,
                    style = MaterialTheme.typography.labelMedium,
                )
                if (validationError.isNotEmpty()) {
                    Text(validationError, color = MaterialTheme.colorScheme.error)
                    TextButton(onClick = { validationAttempt++ }) {
                        Text("Réessayer la vérification")
                    }
                }
                OutlinedCard(onClick = { configuring = !configuring }) {
                    Column(
                        Modifier.fillMaxWidth().padding(12.dp),
                        verticalArrangement = Arrangement.spacedBy(4.dp),
                    ) {
                        Text(
                            hook?.title
                                ?: describeTrigger(definition.obj("trigger"), currentCatalog)
                                    .second,
                            style = MaterialTheme.typography.titleSmall,
                        )
                        Text(
                            "Droits de ${ruleAgentName(state, definition.text("executionAgentId"))}",
                            style = MaterialTheme.typography.bodySmall,
                        )
                        Text(
                            if (configuring) "Fermer les réglages"
                            else "Modifier le déclencheur et l’agent",
                            style = MaterialTheme.typography.labelSmall,
                            color = MaterialTheme.colorScheme.primary,
                        )
                    }
                }
                if (configuring)
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
                                ruleChoicesWithCurrent(
                                    currentCatalog.hooks.map {
                                        triggerKey(wireJson.encodeToJsonElement(it.trigger)) to
                                            it.title
                                    },
                                    triggerKey(definition.obj("trigger")),
                                    describeTrigger(definition.obj("trigger"), currentCatalog)
                                        .second,
                                ),
                                enabled = canEdit,
                            ) { key ->
                                val next =
                                    currentCatalog.hooks.firstOrNull {
                                        triggerKey(wireJson.encodeToJsonElement(it.trigger)) == key
                                    } ?: return@Choice
                                val agent =
                                    definition.text("executionAgentId").takeIf {
                                        it in next.agentIds
                                    } ?: next.agentIds.firstOrNull().orEmpty()
                                update(
                                    definition.patch(
                                        "trigger" to wireJson.encodeToJsonElement(next.trigger),
                                        "executionAgentId" to JsonPrimitive(agent),
                                    )
                                )
                            }
                            val clock =
                                definition.obj("trigger")?.takeIf { it.text("type") == "clock" }
                            if (clock != null)
                                key(original, revision) {
                                    ClockScheduleFields(
                                        wireJson.decodeFromJsonElement<HookRuleTrigger.C>(clock)
                                    ) {
                                        update(
                                            definition.patch(
                                                "trigger" to
                                                    wireJson.encodeToJsonElement<HookRuleTrigger>(
                                                        it
                                                    )
                                            )
                                        )
                                    }
                                }
                            if (hook?.description?.isNotBlank() == true)
                                Text(hook.description, style = MaterialTheme.typography.bodySmall)
                            Choice(
                                "Avec les droits de",
                                definition.text("executionAgentId"),
                                ruleChoicesWithCurrent(
                                    agentChoices.map { it.agentId to it.name },
                                    definition.text("executionAgentId"),
                                    ruleAgentName(state, definition.text("executionAgentId")),
                                ),
                                enabled = canEdit,
                            ) {
                                update(definition.patch("executionAgentId" to JsonPrimitive(it)))
                            }
                        }
                    }
                RuleFlowTree(
                    definition.list("steps"),
                    "/steps",
                    currentCatalog,
                    { ruleAgentName(state, it) },
                    selected,
                    canEdit,
                    problems = problems,
                    executionAgentId = definition.text("executionAgentId"),
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
                    enabled = canEdit,
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
                    enabled = canEdit,
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
                    enabled = canEdit,
                )
                Text(
                    "Au dépassement, la règle s’arrête et son créateur est prévenu. Les déclenchements déjà comptés peuvent finir.",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                problems
                    .filter { !it.path.startsWith("/steps/") }
                    .forEach { problem ->
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
                    val source = rulePeopleSource(hook, definition)
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
                        vm.ruleDrafts.discard(cacheScope, draftSession, retained)
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

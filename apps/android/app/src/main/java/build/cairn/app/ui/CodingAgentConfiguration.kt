package build.cairn.app.ui

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.mapSaver
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.snapshots.SnapshotStateMap
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp
import build.cairn.app.data.CairnViewModel
import build.cairn.contracts.CodingAgentConfiguration
import build.cairn.contracts.CodingAgentConfigurationInput
import build.cairn.contracts.CodingAgentSetting
import build.cairn.contracts.CodingAgentSettingChange
import build.cairn.contracts.EditCodingAgentConfigurationInput
import build.cairn.contracts.getCodingAgentConfiguration
import build.cairn.contracts.previewCodingAgentConfiguration
import build.cairn.contracts.saveCodingAgentConfiguration
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.serialization.decodeFromString
import kotlinx.serialization.encodeToString
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.jsonPrimitive

private fun nativeSettingDraftSaver() =
    mapSaver<SnapshotStateMap<String, String>>(
        save = { it.toMap() },
        restore = { values ->
            mutableStateMapOf<String, String>().apply {
                for ((key, value) in values) this[key] = value as String
            }
        },
    )

@Composable
internal fun CodingAgentConfigurationScreen(
    vm: CairnViewModel,
    provider: String,
    agentId: String?,
    back: () -> Unit,
) {
    CodingAgentConfigurationEditor(
        provider,
        back,
        load = {
            vm.api.getCodingAgentConfiguration(CodingAgentConfigurationInput(agentId, provider))
        },
        preview = { vm.api.previewCodingAgentConfiguration(it) },
        save = {
            val result = vm.api.saveCodingAgentConfiguration(it)
            try {
                vm.refresh()
            } catch (e: CancellationException) {
                throw e
            } catch (e: Exception) {
                vm.report(e)
            }
            result
        },
    )
}

/** Both presentations edit the same native document, including unmapped options. */
@Composable
internal fun CodingAgentConfigurationEditor(
    provider: String,
    back: () -> Unit,
    load: suspend () -> CodingAgentConfiguration,
    preview: suspend (EditCodingAgentConfigurationInput) -> CodingAgentConfiguration,
    save: suspend (EditCodingAgentConfigurationInput) -> CodingAgentConfiguration,
) {
    var currentJson by rememberSaveable { mutableStateOf<String?>(null) }
    var current by remember {
        mutableStateOf(currentJson?.let { Json.decodeFromString<CodingAgentConfiguration>(it) })
    }
    var document by rememberSaveable { mutableStateOf("") }
    var baseline by rememberSaveable { mutableStateOf("") }
    var mode by rememberSaveable { mutableStateOf("form") }
    var loading by remember { mutableStateOf(true) }
    var saving by remember { mutableStateOf(false) }
    var checking by remember { mutableStateOf(false) }
    var error by remember { mutableStateOf<String?>(null) }
    var validation by remember { mutableStateOf<String?>(null) }
    var discard by remember { mutableStateOf(false) }
    var search by rememberSaveable { mutableStateOf("") }
    var generation by remember { mutableIntStateOf(0) }
    var fieldRevision by rememberSaveable { mutableIntStateOf(0) }
    var checkJob by remember { mutableStateOf<Job?>(null) }
    val pending =
        rememberSaveable(saver = nativeSettingDraftSaver()) { mutableStateMapOf<String, String>() }
    val fieldErrors =
        rememberSaveable(saver = nativeSettingDraftSaver()) { mutableStateMapOf<String, String>() }
    var inheritedResets by rememberSaveable { mutableStateOf(emptyList<String>()) }
    val scope = rememberCoroutineScope()
    val dirty =
        document != baseline ||
            pending.isNotEmpty() ||
            fieldErrors.isNotEmpty() ||
            inheritedResets.isNotEmpty()

    fun accept(result: CodingAgentConfiguration) {
        current = result
        currentJson = Json.encodeToString(result.copy(document = "", inheritedDocument = ""))
    }

    fun input(changes: List<CodingAgentSettingChange> = emptyList()) =
        EditCodingAgentConfigurationInput(
            agentId = current?.agentId,
            changes = changes,
            inheritPaths = inheritedResets,
            document = document,
            provider = provider,
            revision = current!!.revision,
        )

    fun reload() {
        checkJob?.cancel()
        val request = ++generation
        scope.launch {
            loading = true
            error = null
            try {
                val result = load()
                if (request != generation) return@launch
                accept(result)
                document = result.document
                baseline = result.document
                validation = null
                pending.clear()
                fieldErrors.clear()
                fieldRevision++
                inheritedResets = emptyList()
            } catch (e: CancellationException) {
                throw e
            } catch (e: Exception) {
                error = e.message
            } finally {
                if (request == generation) {
                    loading = false
                    checking = false
                }
            }
        }
    }

    fun check(
        changes: List<CodingAgentSettingChange> = emptyList(),
        debounce: Boolean = false,
        reset: Boolean = false,
    ) {
        if (current == null || saving) return
        checkJob?.cancel()
        val request = ++generation
        for (change in changes) {
            pending[change.path] = change.value
            inheritedResets =
                if (change.value == "null") (inheritedResets + change.path).distinct()
                else inheritedResets - change.path
        }
        val body =
            input(
                    if (reset) emptyList()
                    else pending.map { CodingAgentSettingChange(it.key, it.value) }
                )
                .copy(reset = reset)
        checking = true
        checkJob = scope.launch {
            try {
                if (debounce) delay(350)
                val result = preview(body)
                if (request == generation) {
                    accept(result)
                    document = result.document
                    validation = null
                    pending.clear()
                    if (reset) {
                        fieldErrors.clear()
                        fieldRevision++
                    }
                    inheritedResets = inheritedResets.filterNot {
                        result.customizedPaths.contains(it)
                    }
                }
            } catch (e: CancellationException) {
                throw e
            } catch (e: Exception) {
                if (request == generation) validation = e.message
            } finally {
                if (request == generation) checking = false
            }
        }
    }

    fun change(field: CodingAgentSetting, entered: String) {
        val value =
            when {
                entered.isEmpty() -> JsonNull
                field.kind == "integer" -> {
                    val number = entered.toLongOrNull()
                    if (number == null || number <= 0 || number > 9_007_199_254_740_991L) {
                        checkJob?.cancel()
                        generation++
                        checking = false
                        fieldErrors[field.path] = "${field.label} : saisissez un entier positif."
                        pending.remove(field.path)
                        return
                    }
                    JsonPrimitive(number)
                }
                field.kind == "boolean" -> JsonPrimitive(entered == "true")
                else -> JsonPrimitive(entered)
            }
        fieldErrors.remove(field.path)
        check(listOf(CodingAgentSettingChange(field.path, value.toString())))
    }

    fun close() {
        if (dirty || checking || validation != null) discard = true else back()
    }
    BackHandler { close() }
    LaunchedEffect(provider) {
        if (current == null || !dirty) reload()
        else {
            loading = false
            check()
        }
    }
    Column(Modifier.fillMaxSize()) {
        DetailHeader(
            "Configuration ${providerLabel(provider)}",
            current?.agentName ?: "Valeurs de l’installation",
            ::close,
        ) {}
        Column(
            Modifier.weight(1f).verticalScroll(rememberScrollState()).padding(20.dp),
            verticalArrangement = Arrangement.spacedBy(16.dp),
        ) {
            if (loading) LinearProgressIndicator(Modifier.fillMaxWidth())
            error?.let {
                Text(it, color = MaterialTheme.colorScheme.error)
                OutlinedButton(onClick = { reload() }) { Text("Recharger la configuration") }
            }
            current
                ?.takeUnless { loading }
                ?.let { configuration ->
                    Text(
                        if (configuration.agentId == null)
                            "Les agents héritent de ces valeurs et peuvent personnaliser leurs propres réglages."
                        else
                            "Les valeurs non personnalisées continuent de suivre les réglages de l’installation."
                    )
                    Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                        FilterChip(
                            mode == "form",
                            { mode = "form" },
                            { Text("Réglages") },
                            enabled = !checking,
                        )
                        FilterChip(
                            mode == "text",
                            { mode = "text" },
                            { Text(configuration.filename) },
                            enabled = !checking,
                        )
                    }
                    validation?.let { Text(it, color = MaterialTheme.colorScheme.error) }
                    TextButton(
                        onClick = {
                            inheritedResets =
                                (inheritedResets + configuration.customizedPaths).distinct()
                            check(reset = true)
                        },
                        enabled =
                            !saving &&
                                !checking &&
                                configuration.customizedPaths.isNotEmpty() &&
                                configuration.customizedPaths.size <= 1000,
                    ) {
                        Text(
                            if (configuration.agentId == null) "Valeurs par défaut du moteur"
                            else "Réinitialiser les personnalisations"
                        )
                    }
                    for (message in fieldErrors.values) Text(
                        message,
                        color = MaterialTheme.colorScheme.error,
                    )
                    if (mode == "text") {
                        Text(configuration.filename, fontFamily = FontFamily.Monospace)
                        if (configuration.agentId != null)
                            Note("Configuration effective, valeurs héritées comprises.")
                        OutlinedTextField(
                            value = document,
                            onValueChange = {
                                document = it
                                pending.clear()
                                fieldErrors.clear()
                                check(debounce = true)
                            },
                            modifier =
                                Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()),
                            label = { Text("Configuration native") },
                            textStyle =
                                MaterialTheme.typography.bodySmall.copy(
                                    fontFamily = FontFamily.Monospace
                                ),
                            keyboardOptions = InputKeyboards.Literal,
                            minLines = 14,
                            maxLines = 24,
                            enabled = !saving,
                        )
                        Note(
                            if (checking) "Vérification…"
                            else if (validation != null) "Corrigez le document avant d’enregistrer."
                            else "Les options avancées sont conservées en changeant de mode."
                        )
                    } else {
                        Field(
                            "Rechercher un réglage",
                            search,
                            { search = it },
                            keyboardOptions = InputKeyboards.Literal,
                        )
                        val groups =
                            configuration.settings
                                .map { it.inFrench() }
                                .filter {
                                    search.isBlank() ||
                                        "${it.label} ${it.path} ${it.description}"
                                            .contains(search, ignoreCase = true)
                                }
                                .groupBy { it.group }
                        if (groups.isEmpty())
                            Note(
                                "Aucun réglage trouvé. Le mode texte permet aussi les options natives avancées."
                            )
                        for ((group, fields) in groups) {
                            Panel {
                                Text(group, style = MaterialTheme.typography.titleMedium)
                                for (field in fields) {
                                    key(field.path, fieldRevision) {
                                        NativeConfigurationField(
                                            field,
                                            !saving,
                                            { change(field, it) },
                                        )
                                    }
                                    Text(
                                        field.description,
                                        style = MaterialTheme.typography.bodySmall,
                                    )
                                    if (configuration.agentId != null)
                                        Note(
                                            if (field.inherited) "Hérité de l’installation"
                                            else "Personnalisé pour cet agent"
                                        )
                                    if (
                                        if (configuration.agentId == null) field.value != null
                                        else !field.inherited
                                    )
                                        TextButton(
                                            onClick = { change(field, "") },
                                            enabled = !saving && !checking,
                                        ) {
                                            Text(
                                                if (configuration.agentId == null)
                                                    "Valeur automatique"
                                                else "Revenir à la valeur de l’installation"
                                            )
                                        }
                                }
                            }
                        }
                        TextButton(onClick = { mode = "text" }, enabled = !checking && !saving) {
                            Text("Modifier les options natives avancées")
                        }
                    }
                    var managed by remember { mutableStateOf(false) }
                    OutlinedButton(onClick = { managed = !managed }) {
                        Text("Réglages gérés par Cairn")
                    }
                    if (managed) {
                        for (item in configuration.managed) {
                            Text(
                                item.path,
                                fontFamily = FontFamily.Monospace,
                                style = MaterialTheme.typography.labelMedium,
                            )
                            Text(
                                nativeManagedReason(item.path),
                                style = MaterialTheme.typography.bodySmall,
                            )
                        }
                    }
                    Note(
                        "Les changements s’appliquent au prochain message, y compris aux conversations existantes. Un tour en cours garde sa configuration."
                    )
                    Note(
                        "Le processus natif peut redémarrer entre deux messages. La conversation et les fichiers sont conservés."
                    )
                }
        }
        HorizontalDivider()
        Row(
            Modifier.fillMaxWidth().padding(16.dp),
            horizontalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            OutlinedButton(
                onClick = { reload() },
                enabled = dirty && !saving,
                modifier = Modifier.weight(1f),
            ) {
                Text("Annuler")
            }
            Button(
                onClick = {
                    scope.launch {
                        saving = true
                        error = null
                        try {
                            val result = save(input())
                            accept(result)
                            document = result.document
                            baseline = result.document
                            inheritedResets = emptyList()
                        } catch (e: CancellationException) {
                            throw e
                        } catch (e: Exception) {
                            error = e.message
                        } finally {
                            saving = false
                        }
                    }
                },
                enabled =
                    current != null &&
                        !loading &&
                        dirty &&
                        !saving &&
                        !checking &&
                        validation == null &&
                        fieldErrors.isEmpty(),
                modifier = Modifier.weight(1f),
            ) {
                Text(if (saving) "Enregistrement…" else "Enregistrer")
            }
        }
    }
    if (discard)
        AlertDialog(
            onDismissRequest = { discard = false },
            title = { Text("Abandonner les modifications ?") },
            text = { Text("La configuration n’a pas été enregistrée.") },
            confirmButton = { TextButton(onClick = back) { Text("Abandonner") } },
            dismissButton = {
                TextButton(onClick = { discard = false }) { Text("Continuer à modifier") }
            },
        )
}

@Composable
private fun NativeConfigurationField(
    field: CodingAgentSetting,
    enabled: Boolean,
    change: (String) -> Unit,
) {
    val selected =
        field.value?.let {
            runCatching { Json.parseToJsonElement(it).jsonPrimitive.content }.getOrDefault("")
        } ?: ""
    var entered by rememberSaveable(field.path, selected) { mutableStateOf(selected) }
    if (field.kind == "select" || field.kind == "boolean") {
        val choices =
            listOf("" to "Automatique / héritée") +
                if (field.kind == "boolean") listOf("true" to "Activé", "false" to "Désactivé")
                else field.choices.map { it to it }
        Choice(field.label, selected, choices, enabled, change)
    } else {
        Field(
            field.label,
            entered,
            {
                entered = it
                change(it)
            },
            enabled = enabled,
            lines = if (field.path == "/developer_instructions") 4 else 1,
            keyboardOptions =
                if (field.kind == "integer") InputKeyboards.Number else InputKeyboards.Literal,
        )
    }
}

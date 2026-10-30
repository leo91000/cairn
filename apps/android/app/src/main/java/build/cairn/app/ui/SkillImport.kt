@file:OptIn(androidx.compose.material3.ExperimentalMaterial3Api::class)

package build.cairn.app.ui

import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.border
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.listSaver
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalSoftwareKeyboardController
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import build.cairn.app.data.*
import build.cairn.contracts.ListPluginPackagesInput
import build.cairn.contracts.listPluginPackages
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.launch

/**
 * Imports a new skill from a `.zip`/`.skill` archive or from a plugin's skill source, such as a
 * repository of a GitHub account.
 */
@Composable
internal fun SkillImportSheet(
    vm: CairnViewModel,
    state: Workspace,
    initialScope: String,
    close: () -> Unit,
    imported: (SkillTarget) -> Unit,
) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    var source by rememberSaveable { mutableStateOf("archive") }
    var skillScope by rememberSaveable {
        mutableStateOf(initialScope.takeIf { it != "all" } ?: "global")
    }
    // The plugin's skill source ("plugin/source"), its instance (the first enabled one unless the
    // owner picks another) and the location's fields, rendered like instance settings.
    var sourceKey by rememberSaveable { mutableStateOf("") }
    var instance by rememberSaveable { mutableStateOf("") }
    var values by
        rememberSaveable(sourceKey, saver = stringMapSaver) {
            mutableStateOf(emptyMap<String, String>())
        }
    var folder by rememberSaveable { mutableStateOf("") }
    var catalog by remember { mutableStateOf<PluginCatalog?>(null) }
    var catalogError by remember { mutableStateOf<String?>(null) }
    var archiveName by rememberSaveable { mutableStateOf<String?>(null) }
    var archiveSize by rememberSaveable { mutableLongStateOf(0L) }
    var upload by rememberSaveable { mutableStateOf<String?>(null) }
    var candidates by rememberSaveable { mutableStateOf(emptyList<String>()) }
    var chosen by rememberSaveable { mutableStateOf<String?>(null) }
    var working by remember { mutableStateOf<String?>(null) }
    var error by remember { mutableStateOf<String?>(null) }
    LaunchedEffect(Unit) {
        try {
            catalog = vm.api.listPluginPackages(ListPluginPackagesInput())
        } catch (e: CancellationException) {
            throw e
        } catch (e: Exception) {
            catalogError = e.message ?: "Plugins indisponibles."
        }
    }
    val sources = catalog?.skillSources().orEmpty()
    val pluginSource = sources.find { it.key == sourceKey } ?: sources.firstOrNull()
    val fields = pluginSource?.source?.settingFields.orEmpty()
    val chosenInstance =
        pluginSource?.instances?.let { instances ->
            instances.find { it.id == instance } ?: instances.firstOrNull()
        }
    // Field defaults until the owner types something.
    val location = fields.associate { it.key to formValue(it, null) } + values

    fun fail(e: Exception) {
        if (e is CancellationException) throw e
        if (e is ApiException && e.status == 401) vm.report(e)
        error =
            if (e is ApiException && e.status == 409)
                "Un skill du même nom existe déjà dans cette portée."
            else e.message ?: "Import impossible. Réessayez."
    }

    val picker =
        rememberLauncherForActivityResult(ActivityResultContracts.OpenDocument()) { uri ->
            if (uri == null) return@rememberLauncherForActivityResult
            scope.launch {
                working = "Envoi de l’archive…"
                error = null
                try {
                    val (name, bytes) =
                        readDocument(context.contentResolver, uri, SkillLimits().uploadBytes)
                    val staged = vm.api.stageSkillUpload(bytes)
                    archiveName = name
                    archiveSize = bytes.size.toLong()
                    upload = staged.id
                    candidates = emptyList()
                    chosen = null
                } catch (e: Exception) {
                    fail(e)
                } finally {
                    working = null
                }
            }
        }

    fun submit() {
        scope.launch {
            working = "Import en cours…"
            error = null
            try {
                val result =
                    if (source == "archive")
                        vm.api.importSkill(skillScope, requireNotNull(upload), path = chosen)
                    else {
                        val origin = requireNotNull(pluginSource) { "Choisissez une source." }
                        vm.api.importPluginSkill(
                            skillScope,
                            PluginSkillImport(
                                origin.entry.id,
                                origin.source.name,
                                chosenInstance?.id,
                                pluginSettings(fields, location),
                                // A chosen candidate is a folder of the location.
                                folder = chosen ?: folder,
                            ),
                        )
                    }
                val bundle = result.imported
                if (bundle == null) {
                    candidates = result.candidates
                    chosen = null
                    if (result.candidates.isEmpty()) error = "Aucun SKILL.md trouvé."
                    return@launch
                }
                vm.notify("Skill « ${bundle.name} » importé")
                try {
                    vm.refresh()
                } catch (e: Exception) {
                    if (e is CancellationException) throw e
                }
                imported(SkillTarget(bundle.scope.ifBlank { skillScope }, bundle.name))
                close()
            } catch (e: Exception) {
                fail(e)
            } finally {
                working = null
            }
        }
    }

    ModalBottomSheet(
        onDismissRequest = { if (working == null) close() },
        sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true),
    ) {
        val keyboard = LocalSoftwareKeyboardController.current
        Column(
            Modifier.testTag("skill-import-sheet")
                .verticalScroll(rememberScrollState())
                .padding(horizontal = 20.dp)
                .padding(bottom = 24.dp)
                .imePadding(),
            verticalArrangement = Arrangement.spacedBy(14.dp),
        ) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(
                    "Importer un skill",
                    Modifier.weight(1f),
                    style = MaterialTheme.typography.titleLarge,
                )
                ActionIcon(
                    "Fermer l’import",
                    CairnIcons.Close,
                    enabled = working == null,
                    onClick = close,
                )
            }
            Text(
                "Le nom du skill vient de son SKILL.md. Les fichiers cachés et __MACOSX sont ignorés.",
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            SingleChoiceSegmentedButtonRow(Modifier.fillMaxWidth()) {
                listOf("archive" to "Archive", "plugin" to "Plugin").forEachIndexed {
                    index,
                    (key, label) ->
                    SegmentedButton(
                        source == key,
                        {
                            source = key
                            candidates = emptyList()
                            chosen = null
                            error = null
                        },
                        SegmentedButtonDefaults.itemShape(index, 2),
                        Modifier.testTag("skill-import-$key"),
                        icon = {
                            Icon(
                                if (key == "archive") CairnIcons.Upload else CairnIcons.Plug,
                                null,
                                Modifier.size(16.dp),
                            )
                        },
                    ) {
                        Text(label)
                    }
                }
            }
            Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
                Eyebrow("Portée")
                Row(
                    Modifier.horizontalScroll(rememberScrollState()),
                    horizontalArrangement = Arrangement.spacedBy(8.dp),
                ) {
                    (listOf("global" to "Global") + state.projects.map { it.id to it.name })
                        .forEach { (key, label) ->
                            SignalChip(
                                label,
                                skillScope == key,
                                leading = if (key == "global") null else identityColor(key),
                            ) {
                                skillScope = key
                            }
                        }
                }
            }
            if (source == "archive") {
                val name = archiveName
                if (name == null)
                    Surface(
                        onClick = {
                            picker.launch(
                                arrayOf(
                                    "application/zip",
                                    "application/x-zip-compressed",
                                    "application/octet-stream",
                                )
                            )
                        },
                        enabled = working == null,
                        shape = RoundedCornerShape(20.dp),
                        color = MaterialTheme.colorScheme.surfaceVariant.copy(alpha = 0.5f),
                        modifier =
                            Modifier.fillMaxWidth()
                                .border(
                                    1.dp,
                                    MaterialTheme.colorScheme.outlineVariant,
                                    RoundedCornerShape(20.dp),
                                ),
                    ) {
                        Column(
                            Modifier.padding(vertical = 26.dp, horizontal = 16.dp),
                            horizontalAlignment = Alignment.CenterHorizontally,
                            verticalArrangement = Arrangement.spacedBy(6.dp),
                        ) {
                            Icon(
                                CairnIcons.Upload,
                                null,
                                Modifier.size(28.dp),
                                tint = MaterialTheme.colorScheme.primary,
                            )
                            Text(
                                "Choisir un fichier .zip ou .skill",
                                style = MaterialTheme.typography.titleSmall,
                            )
                            Text(
                                "Archive de ${formatBytes(SkillLimits().uploadBytes)} au plus",
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        }
                    }
                else
                    SignalCard(Modifier.fillMaxWidth(), outlined = true) {
                        Row(verticalAlignment = Alignment.CenterVertically) {
                            SkillKindBadge(SkillFileKind.BINARY, 40.dp)
                            Spacer(Modifier.width(12.dp))
                            Column(Modifier.weight(1f)) {
                                Text(
                                    name,
                                    style = MaterialTheme.typography.titleSmall,
                                    maxLines = 1,
                                    overflow = TextOverflow.Ellipsis,
                                )
                                Text(
                                    "${formatBytes(archiveSize)} · prête à importer",
                                    style = MaterialTheme.typography.bodySmall,
                                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                                )
                            }
                            TextButton(
                                onClick = {
                                    picker.launch(
                                        arrayOf(
                                            "application/zip",
                                            "application/x-zip-compressed",
                                            "application/octet-stream",
                                        )
                                    )
                                },
                                enabled = working == null,
                            ) {
                                Text("Changer")
                            }
                        }
                    }
            } else if (pluginSource == null) {
                if (catalog == null && catalogError == null)
                    LinearProgressIndicator(Modifier.fillMaxWidth())
                else
                    Text(
                        catalogError
                            ?: "Aucun plugin actif ne fournit de skills. Ajoutez-en un dans Atelier → Plugins.",
                        style = MaterialTheme.typography.bodyMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
            } else {
                fun changed() {
                    candidates = emptyList()
                    chosen = null
                }
                if (sources.size > 1)
                    Choice(
                        "Source",
                        pluginSource.key,
                        sources.map { it.key to it.label },
                        enabled = working == null,
                    ) {
                        if (it != pluginSource.key) {
                            sourceKey = it
                            instance = ""
                            changed()
                        }
                    }
                else Text(pluginSource.label, style = MaterialTheme.typography.titleSmall)
                if (pluginSource.source.description.isNotBlank())
                    Text(
                        pluginSource.source.description,
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                if (pluginSource.instances.size > 1 && chosenInstance != null) {
                    Choice(
                        pluginSource.entry.manifest?.noun() ?: "Instance",
                        chosenInstance.id,
                        pluginSource.instances.map { it.id to it.name },
                        enabled = working == null,
                    ) {
                        instance = it
                        changed()
                    }
                }
                Column(
                    Modifier.testTag("skill-import-fields"),
                    verticalArrangement = Arrangement.spacedBy(10.dp),
                ) {
                    PluginFields(
                        fields,
                        location,
                        { key, value ->
                            values = location + (key to value)
                            changed()
                        },
                        emptyMap(),
                        { _, _ -> },
                        enabled = working == null,
                    )
                }
                OutlinedTextField(
                    folder,
                    {
                        folder = it
                        changed()
                    },
                    Modifier.fillMaxWidth().testTag("skill-import-path"),
                    label = { Text("Dossier") },
                    placeholder = { Text("facultatif") },
                    singleLine = true,
                    keyboardOptions = InputKeyboards.Literal,
                    leadingIcon = { Icon(CairnIcons.Folder, null, Modifier.size(18.dp)) },
                )
            }
            if (candidates.isNotEmpty())
                SignalCard(
                    Modifier.fillMaxWidth().testTag("skill-import-candidates"),
                    color = MaterialTheme.colorScheme.primaryContainer.copy(alpha = 0.55f),
                    outlined = false,
                ) {
                    Text("Plusieurs skills trouvés", style = MaterialTheme.typography.titleSmall)
                    Text(
                        "Choisissez le dossier à importer.",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                    Spacer(Modifier.height(6.dp))
                    candidates.forEach { candidate ->
                        Row(
                            Modifier.fillMaxWidth()
                                .heightIn(min = 48.dp)
                                .clip(RoundedCornerShape(12.dp))
                                .selectable(chosen == candidate, role = Role.RadioButton) {
                                    chosen = candidate
                                }
                                .testTag("skill-candidate-$candidate")
                                .padding(horizontal = 4.dp),
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            RadioButton(chosen == candidate, null)
                            Spacer(Modifier.width(8.dp))
                            Icon(
                                CairnIcons.Folder,
                                null,
                                Modifier.size(18.dp),
                                tint = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                            Spacer(Modifier.width(8.dp))
                            Column(Modifier.weight(1f)) {
                                Text(
                                    candidate.substringAfterLast('/'),
                                    style = MaterialTheme.typography.bodyMedium,
                                )
                                if (candidate.contains('/'))
                                    Text(
                                        candidate,
                                        style = MaterialTheme.typography.labelSmall,
                                        fontFamily = FontFamily.Monospace,
                                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                                    )
                            }
                        }
                    }
                }
            error?.let {
                Surface(
                    shape = RoundedCornerShape(14.dp),
                    color = signal.attentionSoft,
                    modifier = Modifier.fillMaxWidth(),
                ) {
                    Text(
                        it,
                        Modifier.padding(12.dp),
                        style = MaterialTheme.typography.bodyMedium,
                        color = signal.attention,
                    )
                }
            }
            working?.let {
                Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
                    LinearProgressIndicator(Modifier.fillMaxWidth())
                    Text(
                        it,
                        style = MaterialTheme.typography.labelMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
            val ready =
                working == null &&
                    (candidates.isEmpty() || chosen != null) &&
                    (if (source == "archive") upload != null
                    else pluginSource != null && !requiredMissing(fields, location))
            SignalButton(
                if (chosen != null) "Importer ${chosen!!.substringAfterLast('/')}" else "Importer",
                Modifier.fillMaxWidth().testTag("skill-import-submit"),
                icon = CairnIcons.Download,
                container = MaterialTheme.colorScheme.primary,
                content = MaterialTheme.colorScheme.onPrimary,
                enabled = ready,
                expand = true,
                onClick = {
                    // Candidates appear below the fields: make room for them.
                    keyboard?.hide()
                    submit()
                },
            )
        }
    }
}

/** Typed location fields survive recreation; they never hold a secret. */
private val stringMapSaver =
    listSaver<MutableState<Map<String, String>>, String>(
        save = { state -> state.value.flatMap { (key, value) -> listOf(key, value) } },
        restore = { saved ->
            mutableStateOf(saved.chunked(2).associate { (key, value) -> key to value })
        },
    )

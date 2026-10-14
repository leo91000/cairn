@file:OptIn(androidx.compose.material3.ExperimentalMaterial3Api::class)

package build.cairn.app.ui

import android.graphics.BitmapFactory
import androidx.activity.compose.BackHandler
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextDecoration
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import build.cairn.app.data.*
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

internal fun skillKindIcon(kind: SkillFileKind): ImageVector =
    when (kind) {
        SkillFileKind.SKILL -> CairnIcons.Spark
        SkillFileKind.MARKDOWN -> CairnIcons.Markdown
        SkillFileKind.SCRIPT -> CairnIcons.Terminal
        SkillFileKind.IMAGE -> CairnIcons.Image
        SkillFileKind.BINARY -> CairnIcons.Binary
        SkillFileKind.TEXT -> CairnIcons.File
    }

@Composable
internal fun skillKindColor(kind: SkillFileKind): Color =
    when (kind) {
        SkillFileKind.SKILL -> MaterialTheme.colorScheme.primary
        SkillFileKind.MARKDOWN -> Color(0xFF3F7FBF)
        SkillFileKind.SCRIPT -> signal.success
        SkillFileKind.IMAGE -> Color(0xFFB23F8C)
        SkillFileKind.BINARY -> signal.warning
        SkillFileKind.TEXT -> MaterialTheme.colorScheme.onSurfaceVariant
    }

@Composable
internal fun SkillKindBadge(kind: SkillFileKind, size: androidx.compose.ui.unit.Dp = 28.dp) {
    val color = skillKindColor(kind)
    Box(
        Modifier.size(size)
            .clip(RoundedCornerShape(size * 0.3f))
            .background(color.copy(alpha = 0.13f)),
        contentAlignment = Alignment.Center,
    ) {
        Icon(skillKindIcon(kind), null, Modifier.size(size * 0.6f), tint = color)
    }
}

/** Small rounded label: file counts, statuses, flags. */
@Composable
internal fun SkillPill(
    text: String,
    color: Color = MaterialTheme.colorScheme.onSurfaceVariant,
    container: Color = MaterialTheme.colorScheme.surfaceVariant,
    icon: ImageVector? = null,
) {
    Row(
        Modifier.clip(CircleShape)
            .background(container)
            .padding(horizontal = 9.dp, vertical = 3.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(4.dp),
    ) {
        if (icon != null) Icon(icon, null, Modifier.size(13.dp), tint = color)
        Text(
            text,
            style = MaterialTheme.typography.labelMedium,
            fontWeight = FontWeight.SemiBold,
            color = color,
            maxLines = 1,
        )
    }
}

@Composable
private fun StatusPill(status: DraftStatus) {
    val (label, color) =
        when (status) {
            DraftStatus.NEW -> "nouveau" to signal.success
            DraftStatus.MODIFIED -> "modifié" to MaterialTheme.colorScheme.primary
            DraftStatus.MOVED -> "déplacé" to MaterialTheme.colorScheme.primary
            DraftStatus.DELETED -> "supprimé" to signal.attention
            DraftStatus.UNCHANGED -> return
        }
    SkillPill(label, color, color.copy(alpha = 0.12f))
}

internal fun plural(count: Int, one: String, many: String = one + "s") =
    "$count ${if (count == 1) one else many}"

private sealed interface PathPrompt {
    val title: String
    val initial: String

    data class NewFile(val folder: String) : PathPrompt {
        override val title = "Nouveau fichier"
        override val initial = if (folder.isEmpty()) "references/notes.md" else "$folder/"
    }

    data class NewFolder(val parent: String) : PathPrompt {
        override val title = "Nouveau dossier"
        override val initial = if (parent.isEmpty()) "references" else "$parent/"
    }

    data class MoveFile(val from: String) : PathPrompt {
        override val title = "Renommer ou déplacer"
        override val initial = from
    }

    data class MoveFolder(val from: String) : PathPrompt {
        override val title = "Renommer le dossier"
        override val initial = from
    }
}

/** Full-screen editor of one skill: file tree, file editor and saved-state actions. */
@Composable
internal fun SkillWorkbench(
    vm: CairnViewModel,
    state: Workspace,
    model: SkillWorkbenchModel,
    opened: (SkillTarget) -> Unit,
    close: () -> Unit,
) {
    val draft = model.draft
    val target = model.target ?: return
    val owner = state.isOwner
    val context = LocalContext.current
    val focusMode = LocalFocusMode.current
    DisposableEffect(Unit) {
        focusMode(true)
        onDispose { focusMode(false) }
    }
    var discarding by rememberSaveable { mutableStateOf(false) }
    var deletingSkill by rememberSaveable { mutableStateOf(false) }
    var menu by remember { mutableStateOf(false) }
    var actionsFor by rememberSaveable { mutableStateOf<String?>(null) }
    var prompt by remember { mutableStateOf<PathPrompt?>(null) }
    var changesSheet by rememberSaveable { mutableStateOf(false) }
    var sourceSheet by rememberSaveable { mutableStateOf(false) }
    var importFolder by rememberSaveable { mutableStateOf("") }
    var replacing by rememberSaveable { mutableStateOf<String?>(null) }
    val dirty = draft != null && draft.dirty
    val changes = remember(draft) { draft?.changes().orEmpty() }
    val scopeName =
        if (target.scope == "global") "Global"
        else state.projects.find { it.id == target.scope }?.name ?: "Projet"

    val importer =
        rememberLauncherForActivityResult(ActivityResultContracts.OpenMultipleDocuments()) {
            if (it.isNotEmpty()) model.addDocuments(vm, context.contentResolver, it, importFolder)
        }
    val replacer =
        rememberLauncherForActivityResult(ActivityResultContracts.OpenDocument()) { uri ->
            val path = replacing
            if (uri != null && path != null)
                model.replaceDocument(vm, context.contentResolver, path, uri)
        }
    val exporter =
        rememberLauncherForActivityResult(
            ActivityResultContracts.CreateDocument("application/zip")
        ) { uri ->
            if (uri != null) model.export(vm, context.contentResolver, uri)
        }

    fun leave() {
        if (dirty && owner) discarding = true else close()
    }

    // A surface also sets the content colour, so text stays legible in both themes.
    Surface(Modifier.fillMaxSize(), color = MaterialTheme.colorScheme.background) {
        BoxWithConstraints(Modifier.fillMaxSize()) {
            val wide = maxWidth >= 700.dp
            val editing = model.selected
            BackHandler(!wide && editing != null) { model.selected = null }
            BackHandler(wide || editing == null) { leave() }
            Column(Modifier.fillMaxSize().testTag("skill-workbench")) {
                WorkbenchHeader(
                    title = target.name,
                    subtitle =
                        buildString {
                            append(scopeName)
                            if (target.create) append(" · Nouveau skill")
                            else if (!owner) append(" · Lecture seule")
                            else if (changes.isNotEmpty())
                                append(" · ${plural(changes.size, "modification")}")
                        },
                    back = ::leave,
                ) {
                    if (owner)
                        SignalButton(
                            if (target.create) "Créer" else "Enregistrer",
                            Modifier.testTag("skill-save"),
                            icon = CairnIcons.Check,
                            container = MaterialTheme.colorScheme.primary,
                            content = MaterialTheme.colorScheme.onPrimary,
                            enabled = dirty && !model.busy && draft?.problems().isNullOrEmpty(),
                            height = 38.dp,
                        ) {
                            model.save(vm, opened)
                        }
                    if (!target.create)
                        Box {
                            ActionIcon("Plus d’actions", CairnIcons.More) { menu = true }
                            DropdownMenu(menu, { menu = false }) {
                                DropdownMenuItem(
                                    text = { Text("Télécharger le .zip") },
                                    leadingIcon = { Icon(CairnIcons.Download, null) },
                                    enabled = !model.busy,
                                    onClick = {
                                        menu = false
                                        exporter.launch("${target.name}.zip")
                                    },
                                )
                                if (draft?.base?.source != null)
                                    DropdownMenuItem(
                                        text = { Text("Source GitHub") },
                                        leadingIcon = { Icon(CairnIcons.Branch, null) },
                                        onClick = {
                                            menu = false
                                            sourceSheet = true
                                        },
                                    )
                                if (owner)
                                    DropdownMenuItem(
                                        text = { Text("Recharger") },
                                        leadingIcon = { Icon(CairnIcons.Retry, null) },
                                        enabled = !model.busy,
                                        onClick = {
                                            menu = false
                                            model.load(vm)
                                        },
                                    )
                                if (owner)
                                    DropdownMenuItem(
                                        text = {
                                            Text(
                                                "Supprimer le skill",
                                                color = MaterialTheme.colorScheme.error,
                                            )
                                        },
                                        leadingIcon = {
                                            Icon(
                                                CairnIcons.Trash,
                                                null,
                                                tint = MaterialTheme.colorScheme.error,
                                            )
                                        },
                                        onClick = {
                                            menu = false
                                            deletingSkill = true
                                        },
                                    )
                            }
                        }
                }
                if (model.busy) LinearProgressIndicator(Modifier.fillMaxWidth())
                model.progress?.let {
                    Text(
                        it,
                        Modifier.padding(horizontal = 20.dp, vertical = 4.dp),
                        style = MaterialTheme.typography.labelMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
                model.conflict?.let { message ->
                    Banner(
                        message,
                        "Rechargez pour récupérer la dernière version. Vos modifications locales seront perdues.",
                        signal.attention,
                        signal.attentionSoft,
                        Modifier.testTag("skill-conflict"),
                    ) {
                        if (!target.create)
                            SignalButton("Recharger", height = 34.dp) { model.load(vm) }
                    }
                }
                model.error?.let { message ->
                    Banner(message, "", signal.attention, signal.attentionSoft) {
                        TextButton(onClick = { model.error = null }) { Text("Fermer") }
                    }
                }
                if (draft == null) {
                    Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
                        if (model.loading) CircularProgressIndicator()
                        else
                            Column(horizontalAlignment = Alignment.CenterHorizontally) {
                                Text(
                                    "Skill indisponible",
                                    style = MaterialTheme.typography.titleMedium,
                                )
                                TextButton(onClick = { model.load(vm) }) { Text("Réessayer") }
                            }
                    }
                    return@Column
                }
                val files: @Composable (Modifier) -> Unit = { modifier ->
                    FilePane(
                        draft,
                        model,
                        owner && !model.busy,
                        scopeName,
                        modifier,
                        open = {
                            model.selected = it
                            model.ensureLoaded(vm, it)
                        },
                        actions = { actionsFor = it },
                        prompt = { prompt = it },
                        importFiles = {
                            importFolder = ""
                            importer.launch(arrayOf("*/*"))
                        },
                        showChanges = { changesSheet = true },
                        showSource = { sourceSheet = true },
                    )
                }
                if (wide)
                    Row(Modifier.fillMaxSize()) {
                        files(Modifier.width(340.dp).fillMaxHeight())
                        VerticalDivider()
                        val path = editing ?: SKILL_FILE
                        LaunchedEffect(path) { model.ensureLoaded(vm, path) }
                        FileEditor(
                            vm,
                            model,
                            draft,
                            path,
                            owner,
                            Modifier.weight(1f).fillMaxHeight(),
                            back = null,
                            replace = {
                                replacing = path
                                replacer.launch(arrayOf("*/*"))
                            },
                            actions = { if (!model.busy) actionsFor = path },
                        )
                    }
                else if (editing != null)
                    FileEditor(
                        vm,
                        model,
                        draft,
                        editing,
                        owner,
                        Modifier.fillMaxSize(),
                        back = { model.selected = null },
                        replace = {
                            replacing = editing
                            replacer.launch(arrayOf("*/*"))
                        },
                        actions = { if (!model.busy) actionsFor = editing },
                    )
                else files(Modifier.fillMaxSize())
            }
        }
    }
    actionsFor?.let { path ->
        val currentDraft = draft ?: return@let
        FileActionsSheet(
            currentDraft,
            path,
            close = { actionsFor = null },
            rename = {
                actionsFor = null
                prompt =
                    if (currentDraft.file(path) == null) PathPrompt.MoveFolder(path)
                    else PathPrompt.MoveFile(path)
            },
            newFile = {
                actionsFor = null
                prompt = PathPrompt.NewFile(path)
            },
            newFolder = {
                actionsFor = null
                prompt = PathPrompt.NewFolder(path)
            },
            importHere = {
                actionsFor = null
                importFolder = path
                importer.launch(arrayOf("*/*"))
            },
            replace = {
                actionsFor = null
                replacing = path
                replacer.launch(arrayOf("*/*"))
            },
            executable = { value -> model.error = model.update { it.setExecutable(path, value) } },
            delete = {
                actionsFor = null
                if (model.selected == path || model.selected?.startsWith("$path/") == true)
                    model.selected = null
                model.error = model.update {
                    if (it.file(path) == null) it.deleteFolder(path) else it.delete(path)
                }
            },
        )
    }
    prompt?.let { current ->
        val currentDraft = draft ?: return@let
        PathSheet(
            current,
            close = { prompt = null },
            validate = { path ->
                skillPathProblem(path)
                    ?: when {
                        current is PathPrompt.MoveFile && path == current.from -> null
                        current is PathPrompt.MoveFolder && path == current.from -> null
                        currentDraft.file(path) != null ||
                            path in currentDraft.folders ||
                            currentDraft.files.any { it.path.startsWith("$path/") } ->
                            "Ce chemin est déjà utilisé."
                        else -> null
                    }
            },
        ) { path ->
            val problem = model.update {
                when (current) {
                    is PathPrompt.NewFile -> it.put(path, SkillBody.Text(""))
                    is PathPrompt.NewFolder -> it.addFolder(path)
                    is PathPrompt.MoveFile -> it.move(current.from, path)
                    is PathPrompt.MoveFolder -> it.moveFolder(current.from, path)
                }
            }
            if (problem == null) {
                prompt = null
                when (current) {
                    is PathPrompt.NewFile -> model.selected = path
                    is PathPrompt.MoveFile ->
                        if (model.selected == current.from) model.selected = path
                    is PathPrompt.MoveFolder ->
                        model.selected?.let {
                            if (it.startsWith("${current.from}/"))
                                model.selected = path + it.removePrefix(current.from)
                        }
                    is PathPrompt.NewFolder -> model.collapsed = model.collapsed - path
                }
            }
            problem
        }
    }
    if (changesSheet && draft != null)
        ChangesSheet(
            draft,
            changes,
            close = { changesSheet = false },
            discard = {
                changesSheet = false
                if (target.create) close() else model.load(vm)
            },
        )
    if (sourceSheet && draft?.base?.source != null)
        SkillSourceSheet(vm, model, draft, owner) { sourceSheet = false }
    if (discarding)
        AlertDialog(
            onDismissRequest = { discarding = false },
            title = { Text("Abandonner les modifications ?") },
            text = {
                Text(
                    if (target.create) "Ce nouveau skill n’a pas été créé."
                    else
                        "${plural(changes.size, "modification")} non enregistrée${if (changes.size > 1) "s" else ""} sera perdue${if (changes.size > 1) "s" else ""}."
                )
            },
            confirmButton = {
                TextButton(
                    onClick = {
                        discarding = false
                        close()
                    }
                ) {
                    Text("Abandonner", color = MaterialTheme.colorScheme.error)
                }
            },
            dismissButton = {
                TextButton(onClick = { discarding = false }) { Text("Continuer l’édition") }
            },
        )
    if (deletingSkill)
        Confirm(
            "Supprimer ce skill ?",
            "Le skill et tous ses fichiers seront supprimés. Un skill utilisé par une tâche ne peut pas être supprimé.",
            state.busy,
            null,
            { deletingSkill = false },
        ) {
            deletingSkill = false
            model.delete(vm, close)
        }
}

@Composable
private fun WorkbenchHeader(
    title: String,
    subtitle: String,
    back: () -> Unit,
    actions: @Composable RowScope.() -> Unit,
) {
    Row(
        Modifier.fillMaxWidth().padding(start = 4.dp, end = 8.dp, top = 6.dp, bottom = 6.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        ActionIcon("Retour", CairnIcons.Back, onClick = back)
        Column(Modifier.weight(1f).padding(start = 2.dp)) {
            Text(
                title,
                style = MaterialTheme.typography.titleLarge,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            Text(
                subtitle,
                style = MaterialTheme.typography.labelMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
        }
        Row(
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(2.dp),
            content = actions,
        )
    }
}

@Composable
internal fun Banner(
    title: String,
    text: String,
    color: Color,
    container: Color,
    modifier: Modifier = Modifier,
    /** Screen-edge margin; sheets that already pad their content pass 0. */
    inset: androidx.compose.ui.unit.Dp = 16.dp,
    action: @Composable () -> Unit = {},
) {
    Surface(
        modifier.fillMaxWidth().padding(horizontal = inset, vertical = 6.dp),
        shape = RoundedCornerShape(18.dp),
        color = container,
    ) {
        Row(
            Modifier.padding(start = 14.dp, end = 8.dp, top = 10.dp, bottom = 10.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Icon(CairnIcons.Alert, null, Modifier.size(20.dp), tint = color)
            Spacer(Modifier.width(10.dp))
            Column(Modifier.weight(1f)) {
                Text(title, style = MaterialTheme.typography.titleSmall)
                if (text.isNotBlank())
                    Text(
                        text,
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
            }
            action()
        }
    }
}

@Composable
private fun FilePane(
    draft: SkillDraft,
    model: SkillWorkbenchModel,
    owner: Boolean,
    scopeName: String,
    modifier: Modifier,
    open: (String) -> Unit,
    actions: (String) -> Unit,
    prompt: (PathPrompt) -> Unit,
    importFiles: () -> Unit,
    showChanges: () -> Unit,
    showSource: () -> Unit,
) {
    val deleted = draft.deleted
    val rows =
        remember(draft.files, draft.folders, deleted, model.collapsed) {
            skillTree(
                draft.files.map { it.path },
                draft.folders,
                deleted.map { it.path },
                model.collapsed,
            )
        }
    val changes = remember(draft) { draft.changes() }
    val problems = remember(draft) { draft.problems() }
    LazyColumn(
        modifier.testTag("skill-files"),
        contentPadding = PaddingValues(start = 12.dp, end = 12.dp, top = 4.dp, bottom = 32.dp),
    ) {
        item(key = "overview") {
            SkillOverview(draft, scopeName, changes.size, showChanges, showSource)
        }
        if (problems.isNotEmpty() && owner)
            item(key = "problems") {
                Surface(
                    Modifier.fillMaxWidth().padding(top = 10.dp),
                    shape = RoundedCornerShape(18.dp),
                    color = signal.warningSoft,
                ) {
                    Column(
                        Modifier.padding(14.dp),
                        verticalArrangement = Arrangement.spacedBy(4.dp),
                    ) {
                        Text(
                            "À corriger avant d’enregistrer",
                            style = MaterialTheme.typography.titleSmall,
                            color = signal.warning,
                        )
                        problems.forEach {
                            Text(it, style = MaterialTheme.typography.bodySmall)
                        }
                    }
                }
            }
        item(key = "files-header") {
            Row(
                Modifier.fillMaxWidth().padding(start = 6.dp, top = 18.dp, bottom = 4.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Eyebrow("Fichiers", Modifier.weight(1f))
                if (owner) {
                    ActionIcon("Nouveau fichier", CairnIcons.FilePlus) {
                        prompt(PathPrompt.NewFile(""))
                    }
                    ActionIcon("Nouveau dossier", CairnIcons.FolderPlus) {
                        prompt(PathPrompt.NewFolder(""))
                    }
                    ActionIcon(
                        "Ajouter depuis l’appareil",
                        CairnIcons.Upload,
                        onClick = importFiles,
                    )
                }
            }
        }
        items(
            rows,
            key = {
                val kind = if (it.folder) "folder" else if (it.deleted) "deleted" else "file"
                "$kind:${it.path}"
            },
        ) { row ->
            TreeRow(
                row,
                draft,
                selected = model.selected == row.path && !row.deleted,
                collapsed = row.path in model.collapsed,
                owner = owner,
                click = {
                    if (row.folder)
                        model.collapsed =
                            if (row.path in model.collapsed) model.collapsed - row.path
                            else model.collapsed + row.path
                    else if (!row.deleted) open(row.path)
                },
                actions = { actions(row.path) },
                restore = { model.error = model.update { it.restore(row.path) } },
            )
        }
    }
}

@Composable
private fun SkillOverview(
    draft: SkillDraft,
    scopeName: String,
    changeCount: Int,
    showChanges: () -> Unit,
    showSource: () -> Unit,
) {
    val skillText = (draft.content(SKILL_FILE) as? SkillBody.Text)?.value
    val description =
        skillText?.let { skillFrontmatter(it)["description"] } ?: draft.base?.description.orEmpty()
    SignalCard(Modifier.fillMaxWidth(), padding = PaddingValues(16.dp)) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Box(
                Modifier.size(44.dp)
                    .clip(RoundedCornerShape(14.dp))
                    .background(identityColor(draft.name)),
                contentAlignment = Alignment.Center,
            ) {
                Icon(CairnIcons.Spark, null, Modifier.size(24.dp), tint = Color.White)
            }
            Spacer(Modifier.width(12.dp))
            Column(Modifier.weight(1f)) {
                Text(
                    draft.name,
                    style = MaterialTheme.typography.titleMedium,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
                ProjectLabel(scopeName, if (scopeName == "Global") null else scopeName)
            }
        }
        if (description.isNotBlank())
            Text(
                description,
                Modifier.padding(top = 10.dp),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                maxLines = 3,
                overflow = TextOverflow.Ellipsis,
            )
        val limits = draft.limits
        Row(
            Modifier.padding(top = 12.dp).horizontalScroll(rememberScrollState()),
            horizontalArrangement = Arrangement.spacedBy(6.dp),
        ) {
            SkillPill(plural(draft.files.size, "fichier"), icon = CairnIcons.File)
            SkillPill(formatBytes(draft.totalBytes), icon = CairnIcons.Layers)
            if (draft.executables > 0)
                SkillPill(
                    plural(draft.executables, "script"),
                    signal.success,
                    signal.success.copy(alpha = 0.12f),
                    CairnIcons.Terminal,
                )
        }
        val usage = (draft.totalBytes.toFloat() / limits.bundleBytes).coerceIn(0f, 1f)
        LinearProgressIndicator(
            { usage },
            Modifier.fillMaxWidth().padding(top = 12.dp).height(4.dp).clip(CircleShape),
            color = if (usage > 0.9f) signal.attention else MaterialTheme.colorScheme.primary,
            trackColor = MaterialTheme.colorScheme.surfaceVariant,
            drawStopIndicator = {},
        )
        Text(
            "${formatBytes(draft.totalBytes)} sur ${formatBytes(limits.bundleBytes)} · ${draft.files.size}/${limits.files} fichiers",
            Modifier.padding(top = 4.dp),
            style = MaterialTheme.typography.labelSmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        draft.base?.source?.let { source ->
            Surface(
                onClick = showSource,
                modifier =
                    Modifier.fillMaxWidth().padding(top = 12.dp).testTag("skill-source-chip"),
                shape = RoundedCornerShape(14.dp),
                color = MaterialTheme.colorScheme.surfaceVariant,
            ) {
                Row(
                    Modifier.padding(horizontal = 12.dp, vertical = 10.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Icon(CairnIcons.Branch, null, Modifier.size(18.dp))
                    Spacer(Modifier.width(8.dp))
                    Column(Modifier.weight(1f)) {
                        Text(
                            source.label,
                            style = MaterialTheme.typography.labelLarge,
                            fontFamily = FontFamily.Monospace,
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis,
                        )
                        Text(
                            listOf(source.path.ifBlank { "racine" }, source.ref)
                                .filter { it.isNotBlank() }
                                .joinToString(" · "),
                            style = MaterialTheme.typography.labelSmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis,
                        )
                    }
                    if (source.modified) SkillPill("modifié", signal.warning, signal.warningSoft)
                    Icon(
                        CairnIcons.Right,
                        null,
                        Modifier.padding(start = 4.dp).size(16.dp),
                        tint = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
        }
        if (changeCount > 0 && draft.base != null)
            Surface(
                onClick = showChanges,
                modifier = Modifier.fillMaxWidth().padding(top = 10.dp).testTag("skill-changes"),
                shape = RoundedCornerShape(14.dp),
                color = MaterialTheme.colorScheme.primaryContainer,
            ) {
                Row(
                    Modifier.padding(horizontal = 12.dp, vertical = 10.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Box(
                        Modifier.size(8.dp)
                            .background(MaterialTheme.colorScheme.primary, CircleShape)
                    )
                    Spacer(Modifier.width(10.dp))
                    Text(
                        "${plural(changeCount, "modification")} non enregistrée${if (changeCount > 1) "s" else ""}",
                        Modifier.weight(1f),
                        style = MaterialTheme.typography.labelLarge,
                        color = MaterialTheme.colorScheme.onPrimaryContainer,
                    )
                    Text(
                        "Voir",
                        style = MaterialTheme.typography.labelLarge,
                        fontWeight = FontWeight.Bold,
                        color = MaterialTheme.colorScheme.primary,
                    )
                }
            }
    }
}

@Composable
private fun TreeRow(
    row: SkillTreeRow,
    draft: SkillDraft,
    selected: Boolean,
    collapsed: Boolean,
    owner: Boolean,
    click: () -> Unit,
    actions: () -> Unit,
    restore: () -> Unit,
) {
    val file = if (row.folder || row.deleted) null else draft.file(row.path)
    val removed = if (row.deleted) draft.deleted.find { it.path == row.path } else null
    val status =
        when {
            row.deleted -> DraftStatus.DELETED
            file != null -> draft.status(file)
            else -> DraftStatus.UNCHANGED
        }
    val muted = MaterialTheme.colorScheme.onSurfaceVariant
    Row(
        Modifier.fillMaxWidth()
            .heightIn(min = 48.dp)
            .clip(RoundedCornerShape(12.dp))
            .background(
                if (selected) MaterialTheme.colorScheme.primary.copy(alpha = 0.10f)
                else Color.Transparent
            )
            .clickable(onClick = click)
            .testTag(if (row.folder) "skill-folder-${row.path}" else "skill-file-${row.path}")
            .padding(start = 6.dp + 18.dp * row.depth, end = 2.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        if (row.folder) {
            Icon(
                if (collapsed) CairnIcons.Right else CairnIcons.Down,
                if (collapsed) "Déplier" else "Replier",
                Modifier.size(16.dp),
                tint = muted,
            )
            Spacer(Modifier.width(6.dp))
            Icon(CairnIcons.Folder, null, Modifier.size(22.dp), tint = muted)
        } else {
            Spacer(Modifier.width(22.dp))
            SkillKindBadge(
                skillFileKind(
                    row.path,
                    file?.text ?: removed?.text ?: true,
                    file?.executable ?: removed?.executable ?: false,
                )
            )
        }
        Spacer(Modifier.width(10.dp))
        Column(Modifier.weight(1f).padding(vertical = 6.dp)) {
            Text(
                row.name,
                style =
                    MaterialTheme.typography.bodyMedium.copy(
                        fontWeight = if (row.folder) FontWeight.SemiBold else FontWeight.Medium,
                        textDecoration = if (row.deleted) TextDecoration.LineThrough else null,
                    ),
                color = if (row.deleted) muted else MaterialTheme.colorScheme.onSurface,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            if (file != null && status == DraftStatus.MOVED && file.origin != null)
                Text(
                    "depuis ${file.origin}",
                    style = MaterialTheme.typography.labelSmall,
                    color = muted,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
        }
        Row(
            horizontalArrangement = Arrangement.spacedBy(4.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            if (file?.executable == true)
                Icon(
                    CairnIcons.Terminal,
                    "Exécutable",
                    Modifier.size(16.dp),
                    tint = signal.success,
                )
            StatusPill(status)
            val size = file?.size ?: removed?.size
            if (size != null && status == DraftStatus.UNCHANGED)
                Text(formatBytes(size), style = MaterialTheme.typography.labelSmall, color = muted)
        }
        if (owner && row.deleted)
            ActionIcon("Restaurer ${row.name}", CairnIcons.Undo, onClick = restore)
        else if (owner && row.path != SKILL_FILE)
            ActionIcon("Actions pour ${row.name}", CairnIcons.More, onClick = actions)
        else Spacer(Modifier.width(12.dp))
    }
}

@Composable
private fun FileEditor(
    vm: CairnViewModel,
    model: SkillWorkbenchModel,
    draft: SkillDraft,
    path: String,
    owner: Boolean,
    modifier: Modifier,
    back: (() -> Unit)?,
    replace: () -> Unit,
    actions: () -> Unit,
) {
    val file = draft.file(path)
    if (file == null) {
        Box(modifier, contentAlignment = Alignment.Center) {
            Text("Fichier introuvable", color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
        return
    }
    val kind = skillFileKind(file.path, file.text, file.executable)
    val content = draft.content(path)
    val loading = content == null && file.origin != null && file.origin in model.fileLoading
    LaunchedEffect(path, content == null) { if (content == null) model.ensureLoaded(vm, path) }
    var preview by
        rememberSaveable(path) {
            mutableStateOf(!owner && kind in setOf(SkillFileKind.SKILL, SkillFileKind.MARKDOWN))
        }
    Column(modifier.testTag("skill-editor")) {
        Row(
            Modifier.fillMaxWidth().padding(start = 4.dp, end = 8.dp, top = 4.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            if (back != null) ActionIcon("Fichiers", CairnIcons.Back, onClick = back)
            else Spacer(Modifier.width(12.dp))
            SkillKindBadge(kind, 34.dp)
            Spacer(Modifier.width(10.dp))
            Column(Modifier.weight(1f)) {
                Text(
                    file.path.substringAfterLast('/'),
                    style = MaterialTheme.typography.titleMedium,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
                Text(
                    listOfNotNull(
                            file.path.substringBeforeLast('/', "").ifBlank { null },
                            formatBytes(file.size),
                            if (file.text) "UTF-8" else "binaire",
                        )
                        .joinToString(" · "),
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
            }
            StatusPill(draft.status(file))
            if (owner && path != SKILL_FILE)
                ActionIcon(
                    "Actions pour ${file.path.substringAfterLast('/')}",
                    CairnIcons.More,
                    onClick = actions,
                )
        }
        Row(
            Modifier.fillMaxWidth()
                .horizontalScroll(rememberScrollState())
                .padding(horizontal = 16.dp, vertical = 8.dp),
            horizontalArrangement = Arrangement.spacedBy(8.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            if (kind == SkillFileKind.SKILL || kind == SkillFileKind.MARKDOWN)
                SingleChoiceSegmentedButtonRow {
                    SegmentedButton(
                        !preview,
                        { preview = false },
                        SegmentedButtonDefaults.itemShape(0, 2),
                        Modifier.testTag("skill-mode-source"),
                    ) {
                        Text(if (owner) "Modifier" else "Source")
                    }
                    SegmentedButton(
                        preview,
                        { preview = true },
                        SegmentedButtonDefaults.itemShape(1, 2),
                        Modifier.testTag("skill-mode-preview"),
                    ) {
                        Text("Aperçu")
                    }
                }
            val runnable =
                kind == SkillFileKind.SCRIPT ||
                    kind == SkillFileKind.TEXT ||
                    kind == SkillFileKind.MARKDOWN
            if (path != SKILL_FILE && ((owner && runnable) || file.executable))
                FilterChip(
                    file.executable,
                    {
                        if (owner)
                            model.error = model.update { it.setExecutable(path, !file.executable) }
                    },
                    { Text("Exécutable") },
                    Modifier.testTag("skill-executable"),
                    enabled = owner && !model.busy,
                    leadingIcon = { Icon(CairnIcons.Terminal, null, Modifier.size(16.dp)) },
                )
            if (owner && (kind == SkillFileKind.IMAGE || kind == SkillFileKind.BINARY))
                AssistChip(
                    replace,
                    { Text("Remplacer") },
                    leadingIcon = { Icon(CairnIcons.Upload, null, Modifier.size(16.dp)) },
                )
        }
        HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant.copy(alpha = 0.6f))
        when {
            loading || content == null ->
                Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
                    if (loading) CircularProgressIndicator()
                    else TextButton(onClick = { model.ensureLoaded(vm, path) }) { Text("Charger") }
                }
            kind == SkillFileKind.IMAGE ->
                ImagePreview(content.bytes(), file.size, Modifier.fillMaxSize())
            content is SkillBody.Binary -> BinaryInfo(file, owner, replace, Modifier.fillMaxSize())
            preview ->
                Column(
                    Modifier.fillMaxSize()
                        .verticalScroll(rememberScrollState())
                        .padding(20.dp)
                        .testTag("skill-preview")
                ) {
                    MarkdownPreview((content as SkillBody.Text).value)
                }
            else ->
                CodeEditor(
                    (content as SkillBody.Text).value,
                    readOnly = !owner || model.busy,
                    Modifier.fillMaxSize(),
                ) { text ->
                    if (owner) model.update { it.edit(path, text) }
                }
        }
    }
}

/** Markdown with its frontmatter shown as a compact metadata card instead of raw YAML. */
@Composable
private fun MarkdownPreview(text: String) {
    val frontmatter = remember(text) { skillFrontmatter(text) }
    val body = remember(text) { skillMarkdownBody(text) }
    if (frontmatter.isNotEmpty()) {
        Surface(
            Modifier.fillMaxWidth().padding(bottom = 16.dp),
            shape = RoundedCornerShape(16.dp),
            color = MaterialTheme.colorScheme.surfaceVariant.copy(alpha = 0.6f),
        ) {
            Column(Modifier.padding(14.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
                frontmatter.forEach { (key, value) ->
                    Row {
                        Text(
                            key,
                            Modifier.width(96.dp),
                            style = MaterialTheme.typography.labelMedium,
                            fontFamily = FontFamily.Monospace,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                        Text(value, style = MaterialTheme.typography.bodyMedium)
                    }
                }
            }
        }
    }
    Markdown(body)
}

/** Monospace editor with a line-number gutter; long lines scroll sideways instead of wrapping. */
@Composable
private fun CodeEditor(
    text: String,
    readOnly: Boolean,
    modifier: Modifier,
    change: (String) -> Unit,
) {
    val lines = remember(text) { text.count { it == '\n' } + 1 }
    val numbers = remember(lines) { (1..lines).joinToString("\n") }
    val style =
        TextStyle(
            fontFamily = FontFamily.Monospace,
            fontSize = 13.sp,
            lineHeight = 20.sp,
            color = MaterialTheme.colorScheme.onSurface,
        )
    val gutterColor = MaterialTheme.colorScheme.surfaceVariant.copy(alpha = 0.5f)
    var gutter by remember { mutableFloatStateOf(0f) }
    BoxWithConstraints(
        // The gutter band spans the whole editor height, even below short files.
        modifier.drawBehind { drawRect(gutterColor, size = Size(gutter, size.height)) }
    ) {
        val width = maxWidth
        Row(Modifier.fillMaxSize().verticalScroll(rememberScrollState())) {
            Text(
                numbers,
                Modifier.onSizeChanged { gutter = it.width.toFloat() }
                    .padding(start = 10.dp, end = 8.dp, top = 12.dp, bottom = 48.dp)
                    .semantics { contentDescription = "$lines lignes" },
                style =
                    style.copy(
                        color = MaterialTheme.colorScheme.onSurfaceVariant.copy(alpha = 0.7f),
                        textAlign = TextAlign.End,
                    ),
            )
            Box(Modifier.weight(1f).horizontalScroll(rememberScrollState())) {
                BasicTextField(
                    text,
                    change,
                    Modifier.widthIn(min = width - 48.dp)
                        .padding(start = 12.dp, end = 24.dp, top = 12.dp, bottom = 48.dp)
                        .testTag("skill-code-editor"),
                    readOnly = readOnly,
                    textStyle = style,
                    cursorBrush = SolidColor(MaterialTheme.colorScheme.primary),
                    keyboardOptions = InputKeyboards.Literal,
                )
            }
        }
    }
}

@Composable
private fun ImagePreview(bytes: ByteArray, size: Long, modifier: Modifier) {
    val image by
        produceState<android.graphics.Bitmap?>(null, bytes) {
            value =
                withContext(Dispatchers.Default) {
                    runCatching {
                        val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
                        BitmapFactory.decodeByteArray(bytes, 0, bytes.size, bounds)
                        val sample =
                            Integer.highestOneBit(
                                (maxOf(bounds.outWidth, bounds.outHeight) / 2048).coerceAtLeast(1)
                            )
                        BitmapFactory.decodeByteArray(
                            bytes,
                            0,
                            bytes.size,
                            BitmapFactory.Options().apply { inSampleSize = sample },
                        )
                    }
                        .getOrNull()
                }
        }
    val light = MaterialTheme.colorScheme.surface
    val dark = MaterialTheme.colorScheme.surfaceVariant
    Column(
        modifier.verticalScroll(rememberScrollState()).padding(20.dp).testTag("skill-image"),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Box(
            Modifier.fillMaxWidth()
                .heightIn(min = 220.dp)
                .clip(RoundedCornerShape(20.dp))
                .border(1.dp, MaterialTheme.colorScheme.outlineVariant, RoundedCornerShape(20.dp))
                .drawBehind {
                    val cell = 12.dp.toPx()
                    drawRect(light)
                    var y = 0f
                    var row = 0
                    while (y < this.size.height) {
                        var x = if (row % 2 == 0) 0f else cell
                        while (x < this.size.width) {
                            drawRect(dark, Offset(x, y), Size(cell, cell))
                            x += cell * 2
                        }
                        y += cell
                        row++
                    }
                }
                .padding(16.dp),
            contentAlignment = Alignment.Center,
        ) {
            val bitmap = image
            if (bitmap != null)
                Image(
                    bitmap.asImageBitmap(),
                    "Aperçu de l’image",
                    Modifier.fillMaxWidth()
                        .heightIn(max = 460.dp)
                        .aspectRatio(bitmap.width.toFloat() / bitmap.height.coerceAtLeast(1)),
                    contentScale = ContentScale.Fit,
                )
            else Text("Aperçu indisponible", color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
        Text(
            listOfNotNull(
                    image?.let { "${it.width} × ${it.height} px" },
                    formatBytes(size),
                )
                .joinToString(" · "),
            style = MaterialTheme.typography.labelMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

@Composable
private fun BinaryInfo(file: DraftFile, owner: Boolean, replace: () -> Unit, modifier: Modifier) {
    Box(modifier.padding(24.dp), contentAlignment = Alignment.Center) {
        SignalCard(Modifier.fillMaxWidth().widthIn(max = 420.dp), padding = PaddingValues(24.dp)) {
            Column(
                Modifier.fillMaxWidth(),
                horizontalAlignment = Alignment.CenterHorizontally,
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                SkillKindBadge(SkillFileKind.BINARY, 56.dp)
                Text("Fichier binaire", style = MaterialTheme.typography.titleMedium)
                Text(
                    "${formatBytes(file.size)} · livré tel quel avec le skill",
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    textAlign = TextAlign.Center,
                )
                if (owner)
                    SignalButton(
                        "Remplacer depuis l’appareil",
                        icon = CairnIcons.Upload,
                        onClick = replace,
                    )
            }
        }
    }
}

@Composable
private fun FileActionsSheet(
    draft: SkillDraft,
    path: String,
    close: () -> Unit,
    rename: () -> Unit,
    newFile: () -> Unit,
    newFolder: () -> Unit,
    importHere: () -> Unit,
    replace: () -> Unit,
    executable: (Boolean) -> Unit,
    delete: () -> Unit,
) {
    val file = draft.file(path)
    val folderFiles = if (file == null) draft.files.count { it.path.startsWith("$path/") } else 0
    ModalBottomSheet(
        onDismissRequest = close,
        sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true),
    ) {
        Column(Modifier.testTag("skill-file-actions").padding(bottom = 16.dp)) {
            Row(
                Modifier.fillMaxWidth().padding(horizontal = 20.dp, vertical = 4.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                if (file != null)
                    SkillKindBadge(skillFileKind(file.path, file.text, file.executable), 36.dp)
                else Icon(CairnIcons.Folder, null, Modifier.size(32.dp))
                Spacer(Modifier.width(12.dp))
                Column(Modifier.weight(1f)) {
                    Text(
                        path.substringAfterLast('/'),
                        style = MaterialTheme.typography.titleMedium,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                    Text(
                        if (file != null) "$path · ${formatBytes(file.size)}"
                        else "Dossier · ${plural(folderFiles, "fichier")}",
                        style = MaterialTheme.typography.labelMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
            }
            Spacer(Modifier.height(8.dp))
            if (file == null) {
                SheetAction("Nouveau fichier ici", CairnIcons.FilePlus, onClick = newFile)
                SheetAction("Nouveau sous-dossier", CairnIcons.FolderPlus, onClick = newFolder)
                SheetAction(
                    "Ajouter des fichiers depuis l’appareil",
                    CairnIcons.Upload,
                    onClick = importHere,
                )
                SheetAction("Renommer le dossier", CairnIcons.Pencil, onClick = rename)
            } else {
                SheetAction("Renommer ou déplacer", CairnIcons.Move, onClick = rename)
                Row(
                    Modifier.fillMaxWidth()
                        .heightIn(min = 56.dp)
                        .clickable { executable(!file.executable) }
                        .padding(horizontal = 20.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Icon(CairnIcons.Terminal, null, Modifier.size(22.dp))
                    Spacer(Modifier.width(16.dp))
                    Column(Modifier.weight(1f)) {
                        Text("Exécutable", style = MaterialTheme.typography.bodyLarge)
                        Text(
                            "Les scripts exécutables gardent leur mode dans la VM.",
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                    Switch(
                        file.executable,
                        { executable(it) },
                        Modifier.testTag("skill-executable-switch"),
                    )
                }
                SheetAction("Remplacer depuis l’appareil", CairnIcons.Upload, onClick = replace)
            }
            SheetAction(
                if (file == null) "Supprimer le dossier" else "Supprimer",
                CairnIcons.Trash,
                MaterialTheme.colorScheme.error,
                onClick = delete,
            )
        }
    }
}

@Composable
private fun SheetAction(
    label: String,
    icon: ImageVector,
    color: Color = MaterialTheme.colorScheme.onSurface,
    onClick: () -> Unit,
) {
    Row(
        Modifier.fillMaxWidth()
            .heightIn(min = 56.dp)
            .clickable(onClick = onClick)
            .padding(horizontal = 20.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Icon(icon, null, Modifier.size(22.dp), tint = color)
        Spacer(Modifier.width(16.dp))
        Text(label, style = MaterialTheme.typography.bodyLarge, color = color)
    }
}

@Composable
private fun PathSheet(
    prompt: PathPrompt,
    close: () -> Unit,
    validate: (String) -> String?,
    confirm: (String) -> String?,
) {
    var value by
        remember(prompt) {
            mutableStateOf(
                androidx.compose.ui.text.input.TextFieldValue(
                    prompt.initial,
                    androidx.compose.ui.text.TextRange(prompt.initial.length),
                )
            )
        }
    var refused by remember(prompt) { mutableStateOf<String?>(null) }
    val path = value.text.trim().trim('/')
    val problem = validate(path)
    FormSheet(
        prompt.title,
        close,
        when (prompt) {
            is PathPrompt.NewFile,
            is PathPrompt.NewFolder -> "Créer"
            else -> "Renommer"
        },
        confirmEnabled = problem == null,
        confirmTag = "skill-path-confirm",
        confirm = { refused = confirm(path) },
    ) {
        OutlinedTextField(
            value,
            {
                value = it
                refused = null
            },
            Modifier.fillMaxWidth().testTag("skill-path-field"),
            label = { Text("Chemin dans le skill") },
            singleLine = true,
            textStyle = LocalTextStyle.current.copy(fontFamily = FontFamily.Monospace),
            keyboardOptions = InputKeyboards.Literal,
            isError = (refused ?: problem) != null && path.isNotEmpty(),
            supportingText = {
                Text(
                    (refused ?: problem)?.takeIf { path.isNotEmpty() }
                        ?: "Séparez les dossiers par /, par exemple scripts/check.sh."
                )
            },
        )
    }
}

/** Short form in a bottom sheet: title, fields, then cancel and confirm actions. */
@Composable
internal fun FormSheet(
    title: String,
    close: () -> Unit,
    confirmLabel: String,
    confirmEnabled: Boolean,
    confirmTag: String,
    confirm: () -> Unit,
    content: @Composable ColumnScope.() -> Unit,
) {
    ModalBottomSheet(
        onDismissRequest = close,
        sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true),
    ) {
        Column(
            Modifier.fillMaxWidth()
                .verticalScroll(rememberScrollState())
                .padding(horizontal = 20.dp)
                .padding(bottom = 20.dp)
                .imePadding(),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Text(title, style = MaterialTheme.typography.titleLarge)
            content()
            Row(
                Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.spacedBy(8.dp, Alignment.End),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                TextButton(onClick = close) { Text("Annuler") }
                SignalButton(
                    confirmLabel,
                    Modifier.testTag(confirmTag),
                    container = MaterialTheme.colorScheme.primary,
                    content = MaterialTheme.colorScheme.onPrimary,
                    enabled = confirmEnabled,
                    onClick = confirm,
                )
            }
        }
    }
}

@Composable
private fun ChangesSheet(
    draft: SkillDraft,
    changes: List<SkillChange>,
    close: () -> Unit,
    discard: () -> Unit,
) {
    ModalBottomSheet(
        onDismissRequest = close,
        sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true),
    ) {
        Column(
            Modifier.testTag("skill-changes-sheet")
                .verticalScroll(rememberScrollState())
                .padding(horizontal = 20.dp)
                .padding(bottom = 20.dp),
            verticalArrangement = Arrangement.spacedBy(10.dp),
        ) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(
                    "Modifications non enregistrées",
                    Modifier.weight(1f),
                    style = MaterialTheme.typography.titleLarge,
                )
                ActionIcon("Fermer les modifications", CairnIcons.Close, onClick = close)
            }
            Text(
                "Elles seront envoyées ensemble à l’enregistrement, à partir de la révision ${draft.base?.revision?.take(8).orEmpty()}.",
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            changes.forEach { change ->
                val (icon, label, color) =
                    when (change) {
                        is SkillChange.Write ->
                            Triple(
                                CairnIcons.Pencil,
                                if (draft.file(change.path)?.origin == null)
                                    "Ajout de ${change.path}"
                                else "Contenu de ${change.path}",
                                signal.success,
                            )
                        is SkillChange.Delete ->
                            Triple(
                                CairnIcons.Trash,
                                "Suppression de ${change.path}",
                                signal.attention,
                            )
                        is SkillChange.Move ->
                            Triple(
                                CairnIcons.Move,
                                "${change.from} → ${change.to}",
                                MaterialTheme.colorScheme.primary,
                            )
                        is SkillChange.Mode ->
                            Triple(
                                CairnIcons.Terminal,
                                "${change.path} ${if (change.executable) "devient exécutable" else "n’est plus exécutable"}",
                                MaterialTheme.colorScheme.primary,
                            )
                    }
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Box(
                        Modifier.size(30.dp)
                            .clip(RoundedCornerShape(9.dp))
                            .background(color.copy(alpha = 0.13f)),
                        contentAlignment = Alignment.Center,
                    ) {
                        Icon(icon, null, Modifier.size(17.dp), tint = color)
                    }
                    Spacer(Modifier.width(12.dp))
                    Text(
                        label,
                        style = MaterialTheme.typography.bodyMedium,
                        fontFamily = FontFamily.Monospace,
                    )
                }
            }
            Spacer(Modifier.height(4.dp))
            OutlinedButton(onClick = discard, modifier = Modifier.fillMaxWidth()) {
                Icon(CairnIcons.Undo, null, Modifier.size(18.dp))
                Spacer(Modifier.width(8.dp))
                Text("Annuler toutes les modifications")
            }
        }
    }
}

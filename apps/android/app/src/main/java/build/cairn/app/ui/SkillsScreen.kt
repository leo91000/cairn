package build.cairn.app.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.viewmodel.compose.viewModel
import build.cairn.app.data.*

/** Skills library: cards per skill, then a full-screen workbench for the opened one. */
@Composable
fun SkillsScreen(vm: CairnViewModel, state: Workspace) {
    var query by rememberSaveable { mutableStateOf("") }
    var scope by rememberSaveable { mutableStateOf("all") }
    var opened by rememberSaveable { mutableStateOf<String?>(null) }
    var creating by rememberSaveable { mutableStateOf(false) }
    var importing by rememberSaveable { mutableStateOf(false) }
    val workbench = viewModel<SkillWorkbenchModel>(key = "skill-workbench")
    val target = opened?.let(SkillTarget::parse)
    if (target != null) {
        LaunchedEffect(target) { workbench.open(vm, target) }
        SkillWorkbench(
            vm,
            state,
            workbench,
            opened = { opened = it.key },
            close = {
                opened = null
                workbench.reset()
            },
        )
        return
    }
    val skills =
        state.skills.filter {
            (scope == "all" || it.scope == scope) &&
                "${it.name} ${it.description} ${it.source?.repository.orEmpty()}"
                    .contains(query.trim(), true)
        }
    Column(
        Modifier.fillMaxSize()
            .verticalScroll(rememberScrollState())
            .testTag("skills")
            .padding(horizontal = 16.dp)
            .padding(top = 12.dp, bottom = 32.dp),
        verticalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        ScreenTitle(
            "Skills",
            if (state.isOwner) "Méthodes, scripts et références que vos agents emportent."
            else "Méthodes partagées dans cette installation.",
            Modifier.padding(start = 4.dp, bottom = 4.dp),
        )
        if (state.isOwner)
            Row(
                Modifier.fillMaxWidth().padding(vertical = 2.dp),
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                SignalButton(
                    "Nouveau skill",
                    Modifier.weight(1f).testTag("skill-new"),
                    icon = CairnIcons.Plus,
                    expand = true,
                ) {
                    vm.clearMessage()
                    creating = true
                }
                SignalButton(
                    "Importer",
                    Modifier.weight(1f).testTag("skill-import"),
                    icon = CairnIcons.Download,
                    container = MaterialTheme.colorScheme.surface,
                    content = MaterialTheme.colorScheme.onSurface,
                    border = true,
                    expand = true,
                ) {
                    vm.clearMessage()
                    importing = true
                }
            }
        SearchField("Rechercher un skill", query, { query = it })
        Row(
            Modifier.horizontalScroll(rememberScrollState()),
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            SignalChip("Tous", scope == "all", state.skills.size) { scope = "all" }
            SignalChip("Global", scope == "global", state.skills.count { it.scope == "global" }) {
                scope = "global"
            }
            state.projects
                .filter { project -> state.skills.any { it.scope == project.id } }
                .forEach { project ->
                    SignalChip(
                        project.name,
                        scope == project.id,
                        state.skills.count { it.scope == project.id },
                        identityColor(project.id),
                    ) {
                        scope = project.id
                    }
                }
        }
        if (skills.isEmpty())
            SignalCard(Modifier.fillMaxWidth(), padding = PaddingValues(24.dp)) {
                Column(
                    Modifier.fillMaxWidth(),
                    horizontalAlignment = Alignment.CenterHorizontally,
                    verticalArrangement = Arrangement.spacedBy(8.dp),
                ) {
                    SkillKindBadge(SkillFileKind.SKILL, 48.dp)
                    Text(
                        if (state.skills.isEmpty()) "Aucun skill" else "Aucun résultat",
                        style = MaterialTheme.typography.titleMedium,
                    )
                    Text(
                        if (state.skills.isEmpty())
                            "Créez un skill ou importez-en un depuis une archive ou GitHub."
                        else "Aucun skill ne correspond à cette recherche.",
                        style = MaterialTheme.typography.bodyMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
        skills.forEach { skill ->
            SkillCard(skill, scopeLabel(state, skill.scope)) {
                vm.clearMessage()
                opened = SkillTarget(skill.scope, skill.name).key
            }
        }
    }
    if (creating)
        NewSkillSheet(state, scope, close = { creating = false }) { skillScope, name ->
            creating = false
            opened = SkillTarget(skillScope, name, create = true).key
        }
    if (importing)
        SkillImportSheet(vm, state, scope, close = { importing = false }) {
            opened = it.key
        }
}

internal fun scopeLabel(state: Workspace, scope: String) =
    if (scope == "global") "Global" else state.projects.find { it.id == scope }?.name ?: "Projet"

@Composable
private fun SkillCard(skill: Skill, scopeName: String, open: () -> Unit) {
    SignalCard(
        Modifier.fillMaxWidth().testTag("skill-card-${skill.scope}-${skill.name}"),
        onClick = open,
        padding = PaddingValues(16.dp),
    ) {
        Row(verticalAlignment = Alignment.Top) {
            Box(
                Modifier.size(42.dp)
                    .clip(RoundedCornerShape(13.dp))
                    .background(
                        if (skill.valid) identityColor(skill.name)
                        else MaterialTheme.colorScheme.surfaceVariant
                    ),
                contentAlignment = Alignment.Center,
            ) {
                Icon(
                    if (skill.valid) CairnIcons.Spark else CairnIcons.Alert,
                    null,
                    Modifier.size(22.dp),
                    tint = if (skill.valid) Color.White else signal.attention,
                )
            }
            Spacer(Modifier.width(12.dp))
            Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(3.dp)) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text(
                        skill.name,
                        Modifier.weight(1f, fill = false),
                        style = MaterialTheme.typography.titleMedium,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                    if (!skill.valid) {
                        Spacer(Modifier.width(8.dp))
                        SkillPill("à corriger", signal.attention, signal.attentionSoft)
                    }
                }
                ProjectLabel(scopeName, if (skill.scope == "global") null else skill.scope)
            }
            Icon(
                CairnIcons.Right,
                null,
                Modifier.padding(top = 4.dp).size(16.dp),
                tint = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        if (skill.description.isNotBlank())
            Text(
                skill.description,
                Modifier.padding(top = 10.dp),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                maxLines = 2,
                overflow = TextOverflow.Ellipsis,
            )
        skill.error?.let {
            Text(
                it,
                Modifier.padding(top = 6.dp),
                style = MaterialTheme.typography.bodySmall,
                color = signal.attention,
            )
        }
        Row(
            Modifier.padding(top = 12.dp).horizontalScroll(rememberScrollState()),
            horizontalArrangement = Arrangement.spacedBy(6.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            if (skill.files > 0) {
                SkillPill(plural(skill.files, "fichier"), icon = CairnIcons.File)
                SkillPill(formatBytes(skill.bytes))
            }
            if (skill.executables > 0)
                SkillPill(
                    plural(skill.executables, "script"),
                    signal.success,
                    signal.success.copy(alpha = 0.12f),
                    CairnIcons.Terminal,
                )
            skill.source?.let { source ->
                Row(
                    Modifier.clip(androidx.compose.foundation.shape.CircleShape)
                        .background(signal.ink.copy(alpha = 0.07f))
                        .padding(horizontal = 9.dp, vertical = 3.dp)
                        .testTag("skill-source-${skill.name}"),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Icon(CairnIcons.Branch, null, Modifier.size(13.dp))
                    Spacer(Modifier.width(4.dp))
                    Text(
                        source.label,
                        style = MaterialTheme.typography.labelMedium,
                        fontFamily = FontFamily.Monospace,
                        maxLines = 1,
                    )
                    if (source.modified) {
                        Spacer(Modifier.width(6.dp))
                        Text(
                            "modifié",
                            style = MaterialTheme.typography.labelMedium,
                            color = signal.warning,
                        )
                    }
                }
            }
        }
    }
}

@Composable
private fun NewSkillSheet(
    state: Workspace,
    initialScope: String,
    close: () -> Unit,
    create: (scope: String, name: String) -> Unit,
) {
    var name by rememberSaveable { mutableStateOf("") }
    var scope by rememberSaveable {
        mutableStateOf(initialScope.takeIf { it != "all" } ?: "global")
    }
    val taken = state.skills.any { it.scope == scope && it.name == name }
    val problem =
        when {
            name.isEmpty() -> null
            !skillNamePattern.matches(name) ->
                "Lettres minuscules, chiffres et tirets ; commence par une lettre ou un chiffre."
            taken -> "Un skill porte déjà ce nom dans cette portée."
            else -> null
        }
    FormSheet(
        "Nouveau skill",
        close,
        "Créer",
        confirmEnabled = name.isNotEmpty() && problem == null,
        confirmTag = "skill-new-confirm",
        confirm = { create(scope, name) },
    ) {
        OutlinedTextField(
            name,
            { name = it.trim() },
            Modifier.fillMaxWidth().testTag("skill-new-name"),
            label = { Text("Nom du dossier") },
            placeholder = { Text("revue-de-code") },
            singleLine = true,
            textStyle = LocalTextStyle.current.copy(fontFamily = FontFamily.Monospace),
            keyboardOptions = InputKeyboards.Literal,
            isError = problem != null,
            supportingText = { Text(problem ?: "Ce nom sera aussi celui déclaré dans SKILL.md.") },
        )
        Eyebrow("Portée")
        Row(
            Modifier.horizontalScroll(rememberScrollState()),
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            (listOf("global" to "Global") + state.projects.map { it.id to it.name }).forEach {
                (key, label) ->
                SignalChip(
                    label,
                    scope == key,
                    leading = if (key == "global") null else identityColor(key),
                ) {
                    scope = key
                }
            }
        }
    }
}

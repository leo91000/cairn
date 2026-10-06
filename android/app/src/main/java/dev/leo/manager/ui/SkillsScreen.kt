package dev.leo.manager.ui

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Modifier
import dev.leo.manager.data.*

@Composable
fun SkillsScreen(vm: LeoViewModel, state: Workspace) {
    var query by rememberSaveable { mutableStateOf("") }
    var scope by rememberSaveable { mutableStateOf("all") }
    var editing by rememberSaveable { mutableStateOf<String?>(null) }
    var deleting by rememberSaveable { mutableStateOf<String?>(null) }
    val skills =
        state.skills.filter {
            (scope == "all" || it.scope == scope) &&
                "${it.name} ${it.description}".contains(query, true)
        }
    Page {
        Heading("Skills")
        if (state.isOwner)
            Button(
                onClick = {
                    vm.clearMessage()
                    editing = ""
                }
            ) {
                Text("Créer un skill")
            }
        SearchField("Rechercher", query, { query = it })
        Choice(
            "Portée",
            scope,
            listOf("all" to "Toutes", "global" to "Globale") +
                state.projects.map { it.id to it.name },
        ) {
            scope = it
        }
        if (skills.isEmpty()) Empty("Aucun skill", "Ajoutez vos méthodes de travail réutilisables.")
        skills.forEach { skill ->
            Panel {
                Text(skill.name, style = MaterialTheme.typography.titleMedium)
                Status(
                    if (!skill.valid) "À corriger"
                    else if (skill.scope == "global") "Global"
                    else state.projects.find { it.id == skill.scope }?.name ?: "Projet"
                )
                Text(skill.description)
                skill.error?.let { Text(it, color = MaterialTheme.colorScheme.error) }
                Row {
                    ActionIcon(
                        "Ouvrir",
                        LeoIcons.File,
                        onClick = {
                            vm.clearMessage()
                            editing = "${skill.scope}/${skill.name}"
                        },
                    )
                    if (state.isOwner)
                        ActionIcon(
                            "Supprimer",
                            Icons.Default.Delete,
                            onClick = {
                                vm.clearMessage()
                                deleting = "${skill.scope}/${skill.name}"
                            },
                        )
                }
            }
        }
    }
    editing?.let { key ->
        val original = state.skills.find { "${it.scope}/${it.name}" == key }
        if (state.isOwner) SkillEditor(vm, state, original) { editing = null }
        else if (original != null)
            AlertDialog(
                onDismissRequest = { editing = null },
                title = { Text(original.name) },
                text = {
                    androidx.compose.foundation.layout.Column(
                        Modifier.verticalScroll(androidx.compose.foundation.rememberScrollState())
                    ) {
                        Markdown(original.content)
                    }
                },
                confirmButton = { TextButton(onClick = { editing = null }) { Text("Fermer") } },
            )
    }
    deleting?.let { key ->
        Confirm(
            "Supprimer ce skill ?",
            "Le skill et ses fichiers seront supprimés. Un skill utilisé par une tâche ne peut pas être supprimé.",
            state.busy,
            state.error,
            { deleting = null },
        ) {
            vm.perform {
                delete("/skills/" + key.split('/').joinToString("/") { segment(it) })
                deleting = null
            }
        }
    }
}

@Composable
private fun SkillEditor(vm: LeoViewModel, state: Workspace, initial: Skill?, close: () -> Unit) {
    var name by rememberSaveable { mutableStateOf(initial?.name ?: "my-skill") }
    var scope by rememberSaveable { mutableStateOf(initial?.scope ?: "global") }
    var content by rememberSaveable {
        mutableStateOf(
            initial?.content
                ?: "---\nname: my-skill\ndescription: Décrire quand utiliser ce skill.\n---\n\n# Mon skill\n\nDécrire la méthode et les critères de réussite.\n"
        )
    }
    var preview by rememberSaveable { mutableStateOf(false) }
    var files by remember { mutableStateOf<List<String>>(emptyList()) }
    var filesRevision by remember { mutableIntStateOf(0) }
    var supportingFile by rememberSaveable { mutableStateOf<String?>(null) }
    val path = "/skills/${segment(scope)}/${segment(name)}"
    LaunchedEffect(initial?.name, filesRevision) {
        if (initial != null)
            try {
                files = vm.api.get("$path/files")
            } catch (e: Exception) {
                vm.report(e)
            }
    }
    Editor(
        if (initial == null) "Créer un skill" else initial.name,
        state.busy,
        state.error,
        close,
        save = {
            vm.perform {
                api.request("PUT", path, body("content" to content))
                refresh()
                close()
            }
        },
        valid =
            name.matches(Regex("[a-z0-9][a-z0-9-]*")) &&
                content.isNotBlank() &&
                content.length <= 100000,
    ) {
        Field(
            "Nom du dossier",
            name,
            { name = it },
            enabled = initial == null,
            keyboardOptions = InputKeyboards.Literal,
        )
        if (initial == null)
            Choice(
                "Portée",
                scope,
                listOf("global" to "Globale") + state.projects.map { it.id to it.name },
            ) {
                scope = it
            }
        Toggle("Aperçu Markdown", preview) { preview = it }
        if (preview) Markdown(content)
        else
            Field(
                "SKILL.md",
                content,
                { content = it },
                12,
                keyboardOptions = InputKeyboards.Literal,
            )
        if (initial != null) {
            Text("Fichiers de support", style = MaterialTheme.typography.titleMedium)
            files
                .filter { it != "SKILL.md" }
                .forEach { file ->
                    OutlinedButton(
                        onClick = {
                            vm.clearMessage()
                            supportingFile = file
                        }
                    ) {
                        Text(file)
                    }
                }
            TextButton(
                onClick = {
                    vm.clearMessage()
                    supportingFile = ""
                }
            ) {
                Text("Ajouter un fichier")
            }
        } else Text("Enregistrez le skill pour ajouter des fichiers de support.")
    }
    supportingFile?.let { file ->
        SupportingFileEditor(vm, state, path, file) {
            supportingFile = null
            filesRevision++
        }
    }
}

@Composable
private fun SupportingFileEditor(
    vm: LeoViewModel,
    state: Workspace,
    skillPath: String,
    initialPath: String,
    close: () -> Unit,
) {
    var path by rememberSaveable { mutableStateOf(initialPath) }
    var content by rememberSaveable { mutableStateOf("") }
    var ready by rememberSaveable { mutableStateOf(initialPath.isEmpty()) }
    var preview by rememberSaveable { mutableStateOf(false) }
    LaunchedEffect(initialPath) {
        if (!ready)
            try {
                content =
                    vm.api.get<FileContent>("$skillPath/file?path=${segment(initialPath)}").content
                ready = true
            } catch (e: Exception) {
                vm.report(e)
            }
    }
    Editor(
        "Fichier de support",
        state.busy,
        state.error,
        close,
        save = {
            vm.perform {
                api.request("PUT", "$skillPath/file", body("path" to path, "content" to content))
                close()
            }
        },
        valid = ready && path.isNotBlank() && path != "SKILL.md" && content.length <= 100000,
    ) {
        if (!ready) LinearProgressIndicator(Modifier.fillMaxWidth())
        Field(
            "Chemin relatif (ex. references/guide.md)",
            path,
            { path = it },
            enabled = initialPath.isEmpty(),
            keyboardOptions = InputKeyboards.Literal,
        )
        Toggle("Aperçu Markdown", preview) { preview = it }
        if (preview) Markdown(content)
        else
            Field(
                "Contenu",
                content,
                { content = it },
                15,
                enabled = ready,
                keyboardOptions = InputKeyboards.Literal,
            )
    }
}

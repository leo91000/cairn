package build.cairn.app.ui

import androidx.compose.foundation.layout.*
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import build.cairn.app.data.*
import build.cairn.contracts.CreateProjectInput
import build.cairn.contracts.ListPluginPackagesInput
import build.cairn.contracts.ProjectInput
import build.cairn.contracts.ProjectSettings
import build.cairn.contracts.ReplaceProjectInput
import build.cairn.contracts.createProject
import build.cairn.contracts.deleteProject
import build.cairn.contracts.listPluginPackages
import build.cairn.contracts.replaceProject
import kotlinx.serialization.json.*
import kotlinx.serialization.json.decodeFromJsonElement

@Composable
fun ResourcesScreen(
    vm: CairnViewModel,
    state: Workspace,
    agents: Boolean,
    /** Opens an agent's page; "new" creates one. */
    openAgent: (String) -> Unit = {},
    chat: (String, String) -> Unit = { _, _ -> },
) {
    var editing by rememberSaveable { mutableStateOf<String?>(null) }
    var deleting by rememberSaveable { mutableStateOf<String?>(null) }
    var query by rememberSaveable { mutableStateOf("") }
    val kind = if (agents) "agents" else "projects"
    Page {
        Heading(
            if (agents) "Vos agents" else "Vos projets",
            if (agents) "Une équipe qui connaît votre façon de travailler."
            else "Les dépôts sur lesquels vos agents travaillent.",
        )
        if (state.isOwner)
            Button(
                onClick = {
                    vm.clearMessage()
                    if (agents) openAgent("new") else editing = ""
                }
            ) {
                Text(if (agents) "Créer un agent" else "Ajouter un projet")
            }
        SearchField("Rechercher", query, { query = it })
        if ((if (agents) state.agents.size else state.projects.size) == 0) Empty()
        if (agents)
            state.agents
                .filter { "${it.name} ${it.description}".contains(query, true) }
                .forEach { agent ->
                    SignalCard(
                        Modifier.fillMaxWidth().semantics { contentDescription = agent.name },
                        onClick =
                            if (state.isOwner) {
                                {
                                    vm.clearMessage()
                                    openAgent(agent.id)
                                }
                            } else null,
                        padding = PaddingValues(18.dp),
                    ) {
                        Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                            Row(verticalAlignment = Alignment.CenterVertically) {
                                AgentAvatar(agent.name, agent.id)
                                Spacer(Modifier.width(12.dp))
                                Text(
                                    agent.name,
                                    Modifier.weight(1f),
                                    style = MaterialTheme.typography.titleMedium,
                                )
                                // Members start conversations only with agents they may use.
                                if (canUseAgent(state.usableAgents, agent.id))
                                    ActionIcon(
                                        "Discuter avec cet agent",
                                        CairnIcons.Chat,
                                        onClick = { chat(agent.id, "") },
                                    )
                            }
                            if (agent.description.isNotBlank()) Text(agent.description)
                            Text(
                                agentAccessSummary(agent),
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                            Text(
                                "${agent.model.ifBlank { "Modèle par défaut" }} · ${agent.reasoning} · ${if (agent.timeoutMinutes == 0) "Sans limite" else "${agent.timeoutMinutes} min"}",
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        }
                    }
                }
        else
            state.projects
                .filter { "${it.name} ${it.path}".contains(query, true) }
                .forEach { project ->
                    Panel {
                        Text(project.name, style = MaterialTheme.typography.titleMedium)
                        if (project.description.isNotBlank()) Text(project.description)
                        Code(project.path)
                        Text(
                            "${project.baseBranch} · ${if (project.sourceMode == "local") "Branche locale" else "Branche distante"}"
                        )
                        if (project.origin.isNotEmpty()) Code(project.origin)
                        Row {
                            ActionIcon(
                                "Démarrer une conversation",
                                CairnIcons.Chat,
                                onClick = {
                                    chat(defaultAgent(state.usableAgents, state.agents), project.id)
                                },
                            )
                            if (state.isOwner) {
                                ActionIcon(
                                    "Modifier",
                                    Icons.Default.Edit,
                                    onClick = {
                                        vm.clearMessage()
                                        editing = project.id
                                    },
                                )
                                ActionIcon(
                                    "Supprimer",
                                    Icons.Default.Delete,
                                    onClick = {
                                        vm.clearMessage()
                                        deleting = project.id
                                    },
                                )
                            }
                        }
                    }
                }
    }
    editing?.let { id ->
        if (!agents)
            ProjectEditor(vm, state, state.projects.find { it.id == id }) {
                editing = null
            }
    }
    deleting?.let { id ->
        Confirm(
            "Supprimer cette ressource ?",
            "Le serveur refusera la suppression si une tâche utilise encore cette ressource.",
            state.busy,
            state.error,
            { deleting = null },
        ) {
            vm.perform {
                if (kind == "projects") api.deleteProject(ProjectInput(id))
                else delete("/$kind/${segment(id)}")
                deleting = null
            }
        }
    }
}

@Composable
internal fun ProjectEditor(
    vm: CairnViewModel,
    state: Workspace,
    initial: Project?,
    close: () -> Unit,
) {
    var form by
        rememberForm(
            ProjectSettings(
                name = initial?.name.orEmpty(),
                path = initial?.path.orEmpty(),
                description = initial?.description.orEmpty(),
                baseBranch = initial?.baseBranch ?: "main",
                sourceMode = initial?.sourceMode ?: "remote",
            )
        )
    var baseBranch by rememberSaveable { mutableStateOf(initial?.baseBranch ?: "main") }
    var revision by rememberSaveable { mutableStateOf(initial?.revision.orEmpty()) }
    val saving = rememberResourceSave(initial?.takeIf { it.revision.isNotEmpty() }?.id.orEmpty())
    var saveError by rememberSaveable { mutableStateOf<String?>(null) }
    // "local", or the key of a plugin's project source to import from.
    var mode by rememberSaveable { mutableStateOf("local") }
    var instanceChoice by rememberSaveable { mutableStateOf("") }
    var item by rememberSaveable { mutableStateOf("") }
    val creating = saving.id.isEmpty()
    var catalog by remember { mutableStateOf<PluginCatalog?>(null) }
    if (creating)
        LaunchedEffect(Unit) {
            // Without the catalogue, projects can still be added from a server folder.
            catalog =
                runCatching { vm.api.listPluginPackages(ListPluginPackagesInput()) }.getOrNull()
        }
    val sources = catalog?.projectSources().orEmpty()
    val source = sources.find { it.key == mode }?.takeIf { creating }
    // The first enabled instance unless the owner picks another one.
    val instance =
        source?.instances?.let { instances ->
            instances.find { it.id == instanceChoice } ?: instances.firstOrNull()
        }
    val importing = source != null
    fun currentSettings() = form.copy(baseBranch = baseBranch)
    Editor(
        if (saving.id.isEmpty()) "Ajouter un projet" else "Modifier le projet",
        state.busy,
        saveError
            ?: if (!state.busy && saving.id.isBlank() && saving.pendingCreation) {
                "Sauvegarde du projet non confirmée. Réessayez pour retrouver le même projet. ${state.error.orEmpty()}"
                    .trim()
            } else {
                state.error
            },
        close,
        save = {
            vm.perform {
                saveError = null
                try {
                    currentSettings()
                } catch (_: IllegalArgumentException) {
                    saveError = "Saisissez un nom de branche de base valide."
                    return@perform
                }
                try {
                    saving.save(
                        api,
                        "/projects",
                        value = { wireJson.encodeToJsonElement(currentSettings()) },
                        createPath = if (importing) "/projects/import" else "/projects",
                        createValue = {
                            val settings = currentSettings()
                            if (source != null && instance != null)
                                body(
                                    "pluginId" to source.entry.id,
                                    "source" to source.source.name,
                                    "instance" to instance.id,
                                    "id" to item,
                                    "name" to form.name,
                                    "description" to form.description,
                                    "baseBranch" to settings.baseBranch,
                                )
                            else wireJson.encodeToJsonElement(settings)
                        },
                        request = { method, route, payload ->
                            if (route == "/projects/import")
                                api.send<JsonElement>(method, route, payload)
                            else {
                                val saved =
                                    if (method == "PUT")
                                        api.replaceProject(
                                            ReplaceProjectInput(
                                                saving.id,
                                                revision,
                                                currentSettings(),
                                            )
                                        )
                                    else {
                                        val original = JsonObject(payload.jsonObject - "creationId")
                                        api.createProject(
                                            CreateProjectInput(
                                                creationId =
                                                    payload.jsonObject["creationId"]
                                                        ?.jsonPrimitive
                                                        ?.content,
                                                settings =
                                                    wireJson.decodeFromJsonElement<ProjectSettings>(
                                                        original
                                                    ),
                                            )
                                        )
                                    }
                                revision = saved.revision
                                wireJson.encodeToJsonElement(saved)
                            }
                        },
                        onCreated = {
                            val saved = wireJson.decodeFromJsonElement<Project>(it)
                            revision = saved.revision
                            if (importing) {
                                form =
                                    form.copy(
                                        path = saved.path,
                                        sourceMode = saved.sourceMode,
                                    )
                            }
                        },
                    )
                } catch (error: Exception) {
                    if (saving.id.isNotBlank() && saving.id != initial?.id)
                        saveError =
                            "Projet enregistré, mais les dernières modifications n’ont pas été enregistrées. Réessayez."
                    throw error
                }
                try {
                    refresh()
                } catch (error: Exception) {
                    saveError = "Projet enregistré, mais le rechargement a échoué. Réessayez."
                    throw error
                }
                close()
            }
        },
        valid =
            form.name.isNotBlank() &&
                form.name.length <= 100 &&
                (if (importing) instance != null && item.isNotBlank()
                else form.path.isNotBlank()) &&
                baseBranch.isNotBlank(),
    ) {
        if (creating) {
            val editable = !state.busy && !saving.pendingCreation
            // Plugins with project sources (e.g. GitHub) import from their enabled instances.
            if (sources.isNotEmpty())
                Choice(
                    "Ajouter depuis",
                    mode,
                    listOf("local" to "Dossier sur le serveur") +
                        sources.map { it.key to it.label },
                    enabled = editable,
                ) {
                    if (it != mode) item = ""
                    mode = it
                }
            if (source != null && instance != null) {
                if (source.instances.size > 1)
                    Choice(
                        source.entry.manifest?.noun() ?: "Instance",
                        instance.id,
                        source.instances.map { it.id to it.name },
                        enabled = editable,
                    ) {
                        if (it != instance.id) item = ""
                        instanceChoice = it
                    }
                PluginProjectPicker(vm.api, source, instance.id, item, editable) {
                    item = it.id
                    form =
                        form.copy(
                            name = (it.projectName?.ifBlank { null } ?: it.name).take(100),
                            description = it.description.take(500),
                        )
                    if (it.defaultBranch.isNotBlank()) baseBranch = it.defaultBranch
                }
            }
        }
        Field("Nom", form.name, { form = form.copy(name = it) })
        Field("Description", form.description, { form = form.copy(description = it) }, 3)
        if (!importing) {
            Choice(
                "Démarrer depuis",
                form.sourceMode,
                listOf(
                    "remote" to "Dernière branche distante",
                    "local" to "Instantané de la branche locale",
                ),
            ) {
                form = form.copy(sourceMode = it)
            }
            Text(
                if (form.sourceMode == "remote")
                    "Une copie fraîche de la branche distante ; les fichiers locaux restent intacts."
                else "Utilise les fichiers commités de la branche locale.",
                style = MaterialTheme.typography.bodySmall,
            )
            Field(
                "Chemin sur le serveur",
                form.path,
                { form = form.copy(path = it) },
                keyboardOptions = InputKeyboards.Literal,
            )
        }
        Field(
            "Branche de base",
            baseBranch,
            {
                baseBranch = it
                saveError = null
            },
            keyboardOptions = InputKeyboards.Literal,
        )
    }
}

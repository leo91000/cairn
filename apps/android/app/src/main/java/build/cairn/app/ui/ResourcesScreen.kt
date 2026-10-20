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
import kotlinx.serialization.json.decodeFromJsonElement
import kotlinx.serialization.json.encodeToJsonElement

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
                                ActionIcon(
                                    "Discuter avec cet agent",
                                    CairnIcons.Chat,
                                    onClick = { chat(agent.id, "") },
                                )
                            }
                            if (agent.description.isNotBlank()) Text(agent.description)
                            Text(
                                agentAccessSummary(
                                    agent,
                                    older = olderInstallation(state.agents),
                                ),
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
                        project.origin?.let { Code(it) }
                        Row {
                            ActionIcon(
                                "Démarrer une conversation",
                                CairnIcons.Chat,
                                onClick = { chat(MAIN_AGENT_ID, project.id) },
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
            ProjectEditor(vm, state, state.projects.find { it.id == id } ?: Project()) {
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
                delete("/$kind/${segment(id)}")
                deleting = null
            }
        }
    }
}

@Composable
internal fun ProjectEditor(
    vm: CairnViewModel,
    state: Workspace,
    initial: Project,
    close: () -> Unit,
) {
    var form by rememberForm(initial)
    val saving = rememberResourceSave(initial.id)
    var saveError by rememberSaveable { mutableStateOf<String?>(null) }
    var mode by rememberSaveable { mutableStateOf("local") }
    var repository by rememberSaveable { mutableStateOf("") }
    val github = saving.id.isEmpty() && mode == "github"
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
                    saving.save(
                        api,
                        "/projects",
                        value = { wireJson.encodeToJsonElement(form) },
                        createPath = if (github) "/projects/github" else "/projects",
                        createValue = {
                            if (github)
                                body(
                                    "repository" to repository,
                                    "name" to form.name,
                                    "description" to form.description,
                                    "baseBranch" to form.baseBranch,
                                )
                            else wireJson.encodeToJsonElement(form)
                        },
                        onCreated = {
                            if (github) {
                                val saved = wireJson.decodeFromJsonElement<Project>(it)
                                form =
                                    form.copy(
                                        id = saved.id,
                                        path = saved.path,
                                        sourceMode = saved.sourceMode,
                                    )
                            }
                        },
                    )
                } catch (error: Exception) {
                    if (saving.id.isNotBlank() && saving.id != initial.id)
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
                (if (github) repository.isNotBlank() else form.path.isNotBlank()) &&
                form.baseBranch.isNotBlank(),
    ) {
        if (saving.id.isEmpty()) {
            Choice(
                "Ajouter depuis",
                mode,
                listOf("local" to "Dossier sur le serveur", "github" to "GitHub"),
                enabled = !state.busy && !saving.pendingCreation,
            ) {
                mode = it
            }
            if (github)
                GithubRepositoryPicker(vm.api, repository, !state.busy && !saving.pendingCreation) {
                    repository = it.fullName
                    form =
                        form.copy(
                            name = it.name.take(100),
                            description = it.description.take(500),
                            baseBranch = it.defaultBranch,
                        )
                }
        }
        Field("Nom", form.name, { form = form.copy(name = it) })
        Field("Description", form.description, { form = form.copy(description = it) }, 3)
        if (!github) {
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
            form.baseBranch,
            { form = form.copy(baseBranch = it) },
            keyboardOptions = InputKeyboards.Literal,
        )
    }
}

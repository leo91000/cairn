package dev.leo.manager.ui

import androidx.compose.foundation.layout.*
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import dev.leo.manager.data.*
import kotlinx.serialization.json.encodeToJsonElement

@Composable
fun ResourcesScreen(
    vm: LeoViewModel,
    state: Workspace,
    agents: Boolean,
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
        Button(
            onClick = {
                vm.clearMessage()
                editing = ""
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
                    Panel {
                        AgentAvatar(agent.name, agent.id)
                        Text(agent.name, style = MaterialTheme.typography.titleMedium)
                        if (agent.description.isNotBlank()) Text(agent.description)
                        Text(
                            "${agent.model.ifBlank { "Modèle par défaut" }} · ${agent.reasoning} · ${if (agent.timeoutMinutes == 0) "Sans limite" else "${agent.timeoutMinutes} min"}"
                        )
                        Row {
                            ActionIcon(
                                "Discuter avec cet agent",
                                LeoIcons.Chat,
                                onClick = { chat(agent.id, "") },
                            )
                            ActionIcon(
                                "Modifier",
                                Icons.Default.Edit,
                                onClick = {
                                    vm.clearMessage()
                                    editing = agent.id
                                },
                            )
                            ActionIcon(
                                "Supprimer",
                                Icons.Default.Delete,
                                enabled = agent.id != MAIN_AGENT_ID,
                                onClick = {
                                    vm.clearMessage()
                                    deleting = agent.id
                                },
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
                                LeoIcons.Chat,
                                onClick = { chat(MAIN_AGENT_ID, project.id) },
                            )
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
    editing?.let { id ->
        if (agents)
            AgentEditor(vm, state, state.agents.find { it.id == id } ?: Agent()) { editing = null }
        else
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
private fun AgentEditor(vm: LeoViewModel, state: Workspace, initial: Agent, close: () -> Unit) {
    var nodes by remember { mutableStateOf<List<ExecutionNode>>(emptyList()) }
    LaunchedEffect(initial.id) {
        try { nodes = vm.api.get("/nodes") } catch (e: Exception) { vm.report(e) }
    }
    var form by rememberForm(initial)
    var timeout by rememberSaveable { mutableStateOf(initial.timeoutMinutes.toString()) }
    val minutes = timeout.toIntOrNull()
    var newToken by remember { mutableStateOf("") }
    var savedId by rememberSaveable { mutableStateOf(initial.id) }
    Editor(
        if (initial.id.isEmpty()) "Créer un agent" else "Modifier l’agent",
        state.busy,
        state.error,
        close,
        save = {
            vm.perform {
                val saved =
                    api.send<Agent>(
                        if (savedId.isBlank()) "POST" else "PUT",
                        "/agents" + if (savedId.isBlank()) "" else "/${segment(savedId)}",
                        wireJson.encodeToJsonElement(form.copy(timeoutMinutes = minutes!!)),
                    )
                savedId = saved.id
                if (newToken.isNotBlank() && !form.access.github)
                    api.request(
                        "PUT",
                        "/agents/${segment(savedId)}/github-token",
                        body("token" to newToken),
                    )
                newToken = ""
                refresh()
                close()
            }
        },
        valid =
            form.name.isNotBlank() &&
                form.name.length <= 100 &&
                minutes != null &&
                minutes in 0..720 &&
                form.instructions.length <= 20000 &&
                form.description.length <= 500 &&
                form.model.length <= 100 &&
                form.reasoning.length <= 40 &&
                Regex("(?:[a-z][a-z0-9_-]*)?").matches(form.reasoning),
    ) {
        AgentPortraitEditor(vm, state, initial)
        Field("Nom", form.name, { form = form.copy(name = it) })
        Field("Description", form.description, { form = form.copy(description = it) }, 3)
        ModelPicker(
            if (form.provider == "claude") state.claudeModels else state.models,
            form.model,
            form.reasoning,
            enabled = !state.busy,
            refresh = { vm.refreshModels(form.provider) },
            provider = form.provider,
            changeProvider = { form = form.copy(provider = it, model = "", reasoning = "") },
            field = true,
        ) { model, reasoning ->
            form = form.copy(model = model, reasoning = reasoning)
        }
        Field("Instructions", form.instructions, { form = form.copy(instructions = it) }, 6)
        Toggle("Sans limite de temps", minutes == 0) {
            timeout = if (it) "0" else "120"
        }
        if (minutes != 0) {
            Field(
                "Limite en minutes (1–720)",
                timeout,
                { timeout = it },
                keyboardOptions = InputKeyboards.Number,
            )
        }
        Toggle("Toutes les nodes, y compris futures", form.access.nodes == null) {
            form = form.copy(access = form.access.copy(nodes = if (it) null else emptyList()))
        }
        if (form.access.nodes != null) {
            val choices = listOf(LOCAL_NODE_ID to "Runner actuel") + nodes.filter { !it.local && !it.revoked }.map { it.id to it.name }
            choices.forEach { (id, name) ->
                Toggle(name, id in form.access.nodes.orEmpty()) { enabled ->
                    val selected = form.access.nodes.orEmpty()
                    form = form.copy(access = form.access.copy(nodes = if (enabled) selected + id else selected - id))
                }
            }
        }
        Text("Accès de l’agent", style = MaterialTheme.typography.titleMedium)
        Choice(
            "Environnement",
            form.access.sandbox,
            listOf(
                "yolo" to "Accès complet",
                "workspace-write" to "Écriture dans l’espace de travail",
                "read-only" to "Lecture seule",
            ),
        ) {
            form = form.copy(access = form.access.copy(sandbox = it))
        }
        if (form.access.projects == null)
            Toggle("Connexion GitHub partagée", form.access.github) {
                form = form.copy(access = form.access.copy(github = it))
            }
        else
            Text(
                "Projets limités : utilisez un jeton GitHub dédié.",
                style = MaterialTheme.typography.bodySmall,
            )
        if (initial.id.isBlank() && !form.access.github)
            SecretField("Jeton GitHub dédié (facultatif)", newToken) { newToken = it }
        Toggle("Tous les projets", form.access.projects == null) {
            form =
                form.copy(
                    access =
                        form.access.copy(
                            projects = if (it) null else emptyList(),
                            github = if (it) form.access.github else false,
                        )
                )
        }
        if (form.access.projects != null)
            state.projects.forEach { project ->
                Toggle(project.name, project.id in form.access.projects.orEmpty()) { enabled ->
                    val selected = form.access.projects.orEmpty()
                    form =
                        form.copy(
                            access =
                                form.access.copy(
                                    projects =
                                        if (enabled) selected + project.id
                                        else selected - project.id
                                )
                        )
                }
            }
        Toggle("Tous les skills", form.access.skills == null) {
            form = form.copy(access = form.access.copy(skills = if (it) null else emptyList()))
        }
        if (form.access.skills != null)
            state.skills
                .filter {
                    it.valid &&
                        (it.scope == "global" ||
                            form.access.projects == null ||
                            it.scope in form.access.projects.orEmpty())
                }
                .forEach { skill ->
                    val key = "${skill.scope}/${skill.name}"
                    Toggle(key, key in form.access.skills.orEmpty()) { enabled ->
                        val selected = form.access.skills.orEmpty()
                        form =
                            form.copy(
                                access =
                                    form.access.copy(
                                        skills = if (enabled) selected + key else selected - key
                                    )
                            )
                    }
                }
        Toggle("Tous les serveurs MCP", form.access.mcps == null) {
            form =
                form.copy(
                    access =
                        form.access.copy(
                            mcps = if (it) null else emptyList(),
                            mcpTools = emptyMap(),
                        )
                )
        }
        state.mcps.forEach { mcp ->
            val permitted = form.access.mcps == null || mcp.id in form.access.mcps.orEmpty()
            if (form.access.mcps != null)
                Toggle(mcp.name, permitted) { enabled ->
                    form =
                        form.copy(
                            access =
                                form.access.copy(
                                    mcps =
                                        if (enabled) form.access.mcps.orEmpty() + mcp.id
                                        else form.access.mcps.orEmpty() - mcp.id,
                                    mcpTools =
                                        if (enabled) form.access.mcpTools
                                        else form.access.mcpTools - mcp.id,
                                )
                        )
                }
            if (permitted) {
                Text(mcp.name, style = MaterialTheme.typography.titleMedium)
                Toggle("Tous les outils autorisés du serveur", mcp.id !in form.access.mcpTools) {
                    all ->
                    form =
                        form.copy(
                            access =
                                form.access.copy(
                                    mcpTools =
                                        if (all) form.access.mcpTools - mcp.id
                                        else form.access.mcpTools + (mcp.id to emptyList())
                                )
                        )
                }
                if (mcp.id in form.access.mcpTools)
                    mcp.tools
                        .filter { mcp.enabledTools == null || it.name in mcp.enabledTools }
                        .forEach { tool ->
                            Toggle(
                                tool.title ?: tool.name,
                                tool.name in form.access.mcpTools[mcp.id].orEmpty(),
                            ) { selected ->
                                val old = form.access.mcpTools[mcp.id].orEmpty()
                                form =
                                    form.copy(
                                        access =
                                            form.access.copy(
                                                mcpTools =
                                                    form.access.mcpTools +
                                                        (mcp.id to
                                                            if (selected) old + tool.name
                                                            else old - tool.name)
                                            )
                                    )
                            }
                        }
            }
        }
        if (initial.id.isNotBlank() && !form.access.github) AgentGithubToken(vm, state, initial.id)
    }
}

@Composable
private fun ProjectEditor(vm: LeoViewModel, state: Workspace, initial: Project, close: () -> Unit) {
    var form by rememberForm(initial)
    var mode by rememberSaveable { mutableStateOf("local") }
    var repository by rememberSaveable { mutableStateOf("") }
    val github = initial.id.isEmpty() && mode == "github"
    Editor(
        if (initial.id.isEmpty()) "Ajouter un projet" else "Modifier le projet",
        state.busy,
        state.error,
        close,
        save = {
            vm.perform {
                if (github) {
                    api.request("POST", "/projects/github", body("repository" to repository, "name" to form.name, "description" to form.description, "baseBranch" to form.baseBranch))
                    refresh()
                } else save("projects", initial.id, wireJson.encodeToJsonElement(form))
                close()
            }
        },
        valid =
            form.name.isNotBlank() &&
                form.name.length <= 100 &&
                (if (github) repository.isNotBlank() else form.path.isNotBlank()) &&
                form.baseBranch.isNotBlank(),
    ) {
        if (initial.id.isEmpty()) {
            Choice("Ajouter depuis", mode, listOf("local" to "Dossier sur le serveur", "github" to "GitHub")) { if (!state.busy) mode = it }
            if (github) GithubRepositoryPicker(vm.api, repository, !state.busy) {
                repository = it.fullName
                form = form.copy(name = it.name.take(100), description = it.description.take(500), baseBranch = it.defaultBranch)
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

@Composable
private fun AgentGithubToken(vm: LeoViewModel, state: Workspace, id: String) {
    var configured by remember(id) { mutableStateOf(false) }
    var token by remember(id) { mutableStateOf("") }
    var clear by remember { mutableStateOf(false) }
    LaunchedEffect(id) {
        try {
            configured = vm.api.get<Configured>("/agents/$id/github-token").configured
        } catch (e: Exception) {
            vm.report(e)
        }
    }
    Text("Jeton GitHub de cet agent", style = MaterialTheme.typography.titleMedium)
    Text(if (configured) "Un jeton spécifique est configuré." else "Aucun jeton dédié configuré.")
    SecretField("Nouveau jeton", token) { token = it }
    TextButton(
        enabled = token.isNotBlank() && !state.busy,
        onClick = {
            vm.perform {
                api.request("PUT", "/agents/$id/github-token", body("token" to token))
                token = ""
                configured = true
            }
        },
    ) {
        Text("Enregistrer le jeton")
    }
    if (configured) TextButton(onClick = { clear = true }) { Text("Retirer le jeton spécifique") }
    if (clear)
        Confirm(
            "Retirer ce jeton ?",
            "Le jeton dédié sera supprimé. Les autorisations de l’agent restent inchangées.",
            state.busy,
            state.error,
            { clear = false },
        ) {
            vm.perform {
                api.request("PUT", "/agents/$id/github-token", body("token" to ""))
                configured = false
                clear = false
            }
        }
}

@file:OptIn(androidx.compose.material3.ExperimentalMaterial3Api::class)

package build.cairn.app.ui

import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import build.cairn.app.data.*
import build.cairn.contracts.CreateMissionInput
import build.cairn.contracts.MissionSettings
import build.cairn.contracts.ReplaceMissionInput
import build.cairn.contracts.SchedulePreviewInput
import build.cairn.contracts.createMission
import build.cairn.contracts.previewSchedule
import build.cairn.contracts.replaceMission
import kotlinx.serialization.json.decodeFromJsonElement
import kotlinx.serialization.json.encodeToJsonElement
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive

internal fun taskGroup(task: Task, run: Run?): String =
    when {
        run?.active == true -> "En cours"
        run?.status in listOf("failed", "interrupted") ||
            (run?.status == "succeeded" &&
                run.outcome?.status?.let { it != "completed" } == true) -> "À examiner"
        run == null -> "Prêtes"
        else -> "Terminées"
    }

@Composable
internal fun TaskEditor(
    vm: CairnViewModel,
    state: Workspace,
    initial: Task?,
    draft: MissionSettings? = null,
    onSaved: (Task) -> Unit = {},
    close: () -> Unit,
) {
    var form by
        rememberForm(
            initial?.settings()
                ?: draft
                ?: MissionSettings(
                    name = "",
                    prompt = "",
                    agentId = defaultAgent(state.usableAgents, state.agents),
                    skills = null,
                    timezone = java.time.ZoneId.systemDefault().id,
                )
        )
    var revision by rememberSaveable { mutableStateOf(initial?.revision.orEmpty()) }
    val saving = rememberResourceSave(initial?.id.orEmpty())
    var saveError by rememberSaveable { mutableStateOf<String?>(null) }
    var cadence by rememberSaveable {
        mutableStateOf(if (initial?.cron == null) "once" else "custom")
    }
    var tags by rememberSaveable { mutableStateOf(initial?.tags.orEmpty().joinToString(", ")) }
    var occurrences by remember { mutableStateOf<List<Long>>(emptyList()) }
    LaunchedEffect(form.cron, form.timezone) { occurrences = emptyList() }
    val access =
        state.agents.find { it.id == form.agentId }?.access
            ?: AccessPolicy(
                mcpTools = build.cairn.contracts.McpTools(emptyMap()),
                projects = null,
                skills = null,
                mcps = null,
            )
    val projects = state.projects.filter { access.projects == null || it.id in access.projects }
    val available =
        state.skills.filter {
            it.valid &&
                (access.skills == null || "${it.scope}/${it.name}" in access.skills) &&
                (it.scope == "global" || projects.any { project -> project.id == it.scope }) &&
                (form.projectId == null || it.scope == "global" || it.scope == form.projectId)
        }
    Editor(
        if (saving.id.isEmpty()) "Nouvelle mission" else "Modifier la mission",
        state.busy,
        saveError
            ?: if (!state.busy && saving.id.isBlank() && saving.pendingCreation) {
                "Sauvegarde de la mission non confirmée. Réessayez pour retrouver la même mission. ${state.error.orEmpty()}"
                    .trim()
            } else {
                state.error
            },
        close,
        save = {
            vm.perform {
                saveError = null
                val response =
                    try {
                        saving.save(
                            api,
                            "/tasks",
                            value = {
                                wireJson.encodeToJsonElement(
                                    form.copy(
                                        tags =
                                            tags
                                                .split(',')
                                                .map { it.trim() }
                                                .filter { it.isNotEmpty() }
                                    )
                                )
                            },
                            request = { method, _, payload ->
                                val saved =
                                    if (method == "PUT") {
                                        api.replaceMission(
                                            ReplaceMissionInput(
                                                taskId = saving.id,
                                                revision = revision,
                                                settings =
                                                    form.copy(
                                                        tags =
                                                            tags
                                                                .split(',')
                                                                .map { it.trim() }
                                                                .filter { it.isNotEmpty() }
                                                    ),
                                            )
                                        )
                                    } else {
                                        val settings =
                                            wireJson.decodeFromJsonElement<MissionSettings>(
                                                kotlinx.serialization.json.JsonObject(
                                                    payload.jsonObject - "creationId"
                                                )
                                            )
                                        api.createMission(
                                            CreateMissionInput(
                                                settings = settings,
                                                creationId =
                                                    payload.jsonObject["creationId"]
                                                        ?.jsonPrimitive
                                                        ?.content,
                                            )
                                        )
                                    }
                                revision = saved.revision
                                wireJson.encodeToJsonElement(saved)
                            },
                        )
                    } catch (error: Exception) {
                        if (saving.id.isNotBlank() && saving.id != initial?.id)
                            saveError =
                                "Mission enregistrée, mais les dernières modifications n’ont pas été enregistrées. Réessayez."
                        throw error
                    }
                val saved = wireJson.decodeFromJsonElement<Task>(response)
                try {
                    refresh()
                } catch (error: Exception) {
                    saveError = "Mission enregistrée, mais le rechargement a échoué. Réessayez."
                    throw error
                }
                onSaved(saved)
                close()
            }
        },
        valid =
            form.name.isNotBlank() &&
                form.name.length <= 100 &&
                form.prompt.isNotBlank() &&
                form.prompt.length <= 50000 &&
                form.agentId.isNotBlank() &&
                (form.cron == null || !form.cron.isNullOrBlank()),
    ) {
        Field("Nom", form.name, { form = form.copy(name = it) })
        Field("Mission et critères de réussite", form.prompt, { form = form.copy(prompt = it) }, 6)
        // Members schedule only the agents they may use; an edited task keeps showing its own.
        val agents =
            state.agents.filter {
                canUseAgent(state.usableAgents, it.id) || it.id == form.agentId
            }
        Choice("Agent", form.agentId, agents.map { it.id to it.name }) {
            val nextAccess =
                state.agents.find { agent -> agent.id == it }?.access
                    ?: AccessPolicy(
                        mcpTools = build.cairn.contracts.McpTools(emptyMap()),
                        projects = null,
                        skills = null,
                        mcps = null,
                    )
            form =
                form.copy(
                    agentId = it,
                    projectId =
                        form.projectId?.takeIf { project ->
                            nextAccess.projects == null || project in nextAccess.projects
                        },
                    skills =
                        form.skills?.filter { key ->
                            nextAccess.skills == null || key in nextAccess.skills
                        },
                )
        }
        Choice(
            "Projet",
            form.projectId.orEmpty(),
            listOf("" to "Tous les projets autorisés") + projects.map { it.id to it.name },
        ) { id ->
            val keys =
                state.skills
                    .filter { it.valid && (id.isEmpty() || it.scope == "global" || it.scope == id) }
                    .map { "${it.scope}/${it.name}" }
            form =
                form.copy(
                    projectId = id.ifEmpty { null },
                    skills = form.skills?.filter { it in keys },
                )
        }
        Toggle("Utiliser un worktree isolé", form.worktree) { form = form.copy(worktree = it) }
        Choice(
            "Fréquence",
            cadence,
            listOf(
                "once" to "Ponctuelle",
                "daily" to "Chaque jour à 9 h",
                "weekly" to "Chaque lundi à 9 h",
                "custom" to "Cron personnalisé",
            ),
        ) {
            cadence = it
            form =
                form.copy(
                    cron =
                        when (it) {
                            "once" -> null
                            "daily" -> "0 9 * * *"
                            "weekly" -> "0 9 * * 1"
                            else -> form.cron ?: "0 9 * * 1"
                        }
                )
        }
        if (cadence != "once") {
            Field(
                "Expression cron",
                form.cron.orEmpty(),
                {
                    form = form.copy(cron = it)
                    cadence = "custom"
                },
                keyboardOptions = InputKeyboards.Literal,
            )
            Field(
                "Fuseau horaire",
                form.timezone,
                { form = form.copy(timezone = it) },
                keyboardOptions = InputKeyboards.Literal,
            )
            OutlinedButton(
                onClick = {
                    vm.perform {
                        occurrences =
                            api.previewSchedule(
                                    SchedulePreviewInput(
                                        cron = form.cron.orEmpty(),
                                        timezone = form.timezone,
                                    )
                                )
                                .occurrences
                    }
                },
                enabled = !state.busy,
            ) {
                Text("Prévisualiser les prochaines dates")
            }
            occurrences.forEach { Text(date(it)) }
        }
        Toggle("Planification active", form.enabled) { form = form.copy(enabled = it) }
        Field(
            "Étiquettes séparées par des virgules",
            tags,
            { tags = it },
            keyboardOptions = InputKeyboards.Literal,
        )
        Text("Skills", style = MaterialTheme.typography.titleMedium)
        Toggle("Tous les skills autorisés", form.skills == null) {
            form = form.copy(skills = if (it) null else emptyList())
        }
        if (form.skills != null)
            available.forEach { skill ->
                val key = "${skill.scope}/${skill.name}"
                Toggle(
                    "${skill.name} · ${if (skill.scope == "global") "Global" else "Projet"}",
                    key in form.skills.orEmpty(),
                ) { checked ->
                    form =
                        form.copy(
                            skills =
                                if (checked) form.skills.orEmpty() + key
                                else form.skills.orEmpty() - key
                        )
                }
            }
        if (available.isEmpty()) Text("Aucun skill disponible pour ce projet.")
    }
}

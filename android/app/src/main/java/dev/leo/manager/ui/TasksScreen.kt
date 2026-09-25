@file:OptIn(androidx.compose.material3.ExperimentalMaterial3Api::class)

package dev.leo.manager.ui

import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import dev.leo.manager.data.*
import kotlinx.serialization.json.encodeToJsonElement

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
    vm: LeoViewModel,
    state: Workspace,
    initial: Task,
    onSaved: (Task) -> Unit = {},
    close: () -> Unit,
) {
    var form by rememberForm(initial)
    var cadence by rememberSaveable {
        mutableStateOf(if (initial.cron == null) "once" else "custom")
    }
    var tags by rememberSaveable { mutableStateOf(initial.tags.joinToString(", ")) }
    var occurrences by remember { mutableStateOf<List<Long>>(emptyList()) }
    LaunchedEffect(form.cron, form.timezone) { occurrences = emptyList() }
    val access = state.agents.find { it.id == form.agentId }?.access ?: AccessPolicy()
    val projects = state.projects.filter { access.projects == null || it.id in access.projects }
    val available =
        state.skills.filter {
            it.valid &&
                (access.skills == null || "${it.scope}/${it.name}" in access.skills) &&
                (it.scope == "global" || projects.any { project -> project.id == it.scope }) &&
                (form.projectId == null || it.scope == "global" || it.scope == form.projectId)
        }
    Editor(
        if (initial.id.isEmpty()) "Nouvelle mission" else "Modifier la mission",
        state.busy,
        state.error,
        close,
        save = {
            vm.perform {
                val saved =
                    api.send<Task>(
                        if (initial.id.isBlank()) "POST" else "PUT",
                        "/tasks" + if (initial.id.isBlank()) "" else "/${segment(initial.id)}",
                        wireJson.encodeToJsonElement(
                            form.copy(
                                tags = tags.split(',').map { it.trim() }.filter { it.isNotEmpty() }
                            )
                        ),
                    )
                refresh()
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
        Choice("Agent", form.agentId, state.agents.map { it.id to it.name }) {
            val nextAccess = state.agents.find { agent -> agent.id == it }?.access ?: AccessPolicy()
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
                            api.send<Occurrences>(
                                    "POST",
                                    "/schedule/preview",
                                    body(
                                        "cron" to form.cron.orEmpty(),
                                        "timezone" to form.timezone,
                                    ),
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

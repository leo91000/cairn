@file:OptIn(ExperimentalLayoutApi::class, ExperimentalMaterial3Api::class)

package build.cairn.app.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.toggleable
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import build.cairn.app.data.*

@Composable
internal fun AgentHeader(
    agent: Agent,
    creating: Boolean,
    main: Boolean,
    back: () -> Unit,
    chat: () -> Unit,
    delete: () -> Unit,
) {
    var menu by remember { mutableStateOf(false) }
    Row(
        Modifier.fillMaxWidth().padding(horizontal = 4.dp, vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        ActionIcon("Retour", CairnIcons.Back, onClick = back)
        AgentAvatar(agent.name.ifBlank { "?" }, agent.id, 40.dp)
        Spacer(Modifier.width(12.dp))
        Column(Modifier.weight(1f)) {
            Text(
                agent.name.ifBlank { "Nouvel agent" },
                Modifier.semantics { heading() },
                style = MaterialTheme.typography.titleLarge,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            if (agent.description.isNotBlank())
                Text(
                    agent.description,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
        }
        if (!creating)
            Box {
                ActionIcon("Plus d’actions", CairnIcons.More) { menu = true }
                DropdownMenu(menu, { menu = false }) {
                    DropdownMenuItem(
                        text = { Text("Discuter avec cet agent") },
                        onClick = {
                            menu = false
                            chat()
                        },
                    )
                    if (!main)
                        DropdownMenuItem(
                            text = { Text("Supprimer l’agent") },
                            onClick = {
                                menu = false
                                delete()
                            },
                        )
                }
            }
    }
}

@Composable
internal fun InlineError(text: String) {
    Text(
        text,
        Modifier.fillMaxWidth()
            .clip(RoundedCornerShape(12.dp))
            .background(MaterialTheme.colorScheme.errorContainer)
            .padding(12.dp),
        color = MaterialTheme.colorScheme.onErrorContainer,
        style = MaterialTheme.typography.bodyMedium,
    )
}

@Composable
internal fun Note(text: String, modifier: Modifier = Modifier) {
    Text(
        text,
        modifier,
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
    )
}

@Composable
internal fun InfoBox(text: String, modifier: Modifier = Modifier) {
    Text(
        text,
        modifier
            .fillMaxWidth()
            .clip(RoundedCornerShape(14.dp))
            .background(MaterialTheme.colorScheme.secondaryContainer)
            .padding(14.dp),
        style = MaterialTheme.typography.bodyMedium,
        color = MaterialTheme.colorScheme.onSecondaryContainer,
    )
}

@Composable
internal fun PowerChip(text: String) {
    val (container, content) = powerColors()
    Text(
        text,
        Modifier.clip(CircleShape)
            .background(container)
            .padding(horizontal = 9.dp, vertical = 3.dp),
        style = MaterialTheme.typography.labelMedium,
        fontWeight = FontWeight.SemiBold,
        color = content,
        maxLines = 1,
    )
}

@Composable
internal fun LockedChip() {
    Text(
        "🔒 Fixé",
        Modifier.clip(CircleShape)
            .background(MaterialTheme.colorScheme.surfaceVariant)
            .padding(horizontal = 10.dp, vertical = 4.dp),
        style = MaterialTheme.typography.labelMedium,
        fontWeight = FontWeight.SemiBold,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        maxLines = 1,
    )
}

@Composable
internal fun LockedSection() {
    InfoBox(
        "L’agent principal a toujours accès à tous les projets, skills et connexions MCP, y compris futurs. Créez un autre agent pour un accès restreint."
    )
    Row(verticalAlignment = Alignment.CenterVertically) {
        Text("Tous, y compris futurs", Modifier.weight(1f))
        LockedChip()
    }
}

/** One overview row: title, effective summary with state chips, and what opens it. */
@Composable
internal fun OverviewRow(
    section: AgentSection,
    summary: String,
    locked: Boolean = false,
    error: Boolean = false,
    changed: Boolean = false,
    chips: @Composable () -> Unit = {},
    open: () -> Unit,
) {
    val description = buildString {
        append(section.title)
        if (summary.isNotBlank()) append(", $summary")
        if (locked) append(", fixé")
        if (error) append(", à corriger")
        if (changed) append(", modifié")
    }
    Row(
        Modifier.fillMaxWidth()
            .heightIn(min = 64.dp)
            .then(
                if (locked) Modifier
                else Modifier.clickable(onClickLabel = "Ouvrir ${section.title}", onClick = open)
            )
            .semantics(mergeDescendants = true) { contentDescription = description }
            .testTag("agent-section-${section.name}")
            .padding(horizontal = 16.dp, vertical = 10.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            Text(section.title, style = MaterialTheme.typography.bodyLarge)
            FlowRow(
                horizontalArrangement = Arrangement.spacedBy(6.dp),
                verticalArrangement = Arrangement.spacedBy(4.dp),
                itemVerticalAlignment = Alignment.CenterVertically,
            ) {
                chips()
                if (summary.isNotBlank())
                    Text(
                        summary,
                        style = MaterialTheme.typography.bodyMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
            }
        }
        if (error)
            Box(
                Modifier.padding(horizontal = 6.dp)
                    .size(8.dp)
                    .background(MaterialTheme.colorScheme.error, CircleShape)
            )
        else if (changed)
            Box(
                Modifier.padding(horizontal = 6.dp)
                    .size(8.dp)
                    .background(MaterialTheme.colorScheme.primary, CircleShape)
            )
        if (locked) LockedChip()
        else
            androidx.compose.material3.Icon(
                CairnIcons.Right,
                null,
                Modifier.size(20.dp),
                tint = MaterialTheme.colorScheme.onSurfaceVariant,
            )
    }
    HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant.copy(alpha = 0.6f))
}

@Composable
internal fun AgentOverview(
    draft: AgentDraft,
    resources: AgentResources,
    creating: Boolean,
    issues: List<AgentIssue>,
    errors: Set<AgentSection>,
    changed: Set<AgentSection>,
    /** Codex's catalog, to name the agent's speed as its model runs it. */
    codexModels: List<CodingAgentModel>,
    people: AgentPeople,
    grantsLoaded: Boolean,
    grantsError: String?,
    open: (AgentSection) -> Unit,
) {
    val agent = draft.agent
    val access = agent.access
    val main = agent.id == MAIN_AGENT_ID
    var showIssues by rememberSaveable { mutableStateOf(false) }
    if (main)
        InfoBox(
            "L’agent principal a toujours accès à tous les projets, skills et connexions MCP, y compris futurs. Créez un autre agent pour un accès restreint."
        )
    if (issues.isNotEmpty())
        Column(
            Modifier.fillMaxWidth()
                .clip(RoundedCornerShape(14.dp))
                .background(signal.warningSoft)
                .clickable { showIssues = !showIssues }
                .padding(14.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Text(
                "⚠ ${issuesLabel(issues.size)}",
                color = signal.warning,
                style = MaterialTheme.typography.titleSmall,
            )
            if (showIssues)
                issues.forEach { issue ->
                    Text(
                        "${issue.section.title} : ${issue.text}",
                        Modifier.fillMaxWidth()
                            .clickable(onClickLabel = "Ouvrir ${issue.section.title}") {
                                open(issue.section)
                            }
                            .padding(vertical = 4.dp),
                        style = MaterialTheme.typography.bodyMedium,
                    )
                }
        }
    if (creating)
        Note(
            "Choisissez ce que ce nouvel agent peut voir et faire, puis créez-le avec le bouton en bas de l’écran."
        )
    val disabledMcps =
        access.mcps.orEmpty().count { id -> resources.mcps.find { it.id == id }?.enabled == false }
    val plugins = pluginSummary(agent, resources.catalog)
    val exclusive = agent.effectiveExclusive(resources.nodes)
    Column {
        Eyebrow("Accès", Modifier.padding(start = 16.dp, top = 8.dp, bottom = 6.dp))
        OverviewRow(
            AgentSection.Projects,
            selectionSummary(access.projects, resources.projects.size),
            locked = main,
            error = AgentSection.Projects in errors,
            changed = AgentSection.Projects in changed,
        ) {
            open(AgentSection.Projects)
        }
        OverviewRow(
            AgentSection.Skills,
            if (main) "Toutes" else selectionSummary(access.skills, resources.skills.size, true),
            locked = main,
            error = AgentSection.Skills in errors,
            changed = AgentSection.Skills in changed,
        ) {
            open(AgentSection.Skills)
        }
        OverviewRow(
            AgentSection.Mcp,
            when {
                main || access.mcps == null -> "Toutes les connexions"
                access.mcps.isEmpty() -> "Aucune connexion"
                else -> "${access.mcps.size} connexion${if (access.mcps.size > 1) "s" else ""}"
            },
            locked = main,
            error = AgentSection.Mcp in errors,
            changed = AgentSection.Mcp in changed,
            chips = {
                if (!main && disabledMcps > 0)
                    ToneChip(
                        PluginBadge(
                            "$disabledMcps désactivée${if (disabledMcps > 1) "s" else ""}",
                            PluginTone.WARNING,
                        )
                    )
            },
        ) {
            open(AgentSection.Mcp)
        }
        OverviewRow(
            AgentSection.Plugins,
            when {
                grantsError != null -> "Indisponible"
                !grantsLoaded -> "Chargement…"
                plugins.isBlank() -> if (main) "· même pour Main" else ""
                else -> plugins
            },
            error = AgentSection.Plugins in errors || grantsError != null,
            changed = AgentSection.Plugins in changed,
            chips = {
                when {
                    grantsError != null -> ToneChip(PluginBadge("Indisponible", PluginTone.WARNING))
                    !grantsLoaded -> ToneChip(PluginBadge("Chargement…", PluginTone.MUTED))
                    plugins.isBlank() -> ToneChip(PluginBadge("Aucun accès", PluginTone.MUTED))
                }
            },
        ) {
            open(AgentSection.Plugins)
        }
        Eyebrow("Exécution", Modifier.padding(start = 16.dp, top = 16.dp, bottom = 6.dp))
        OverviewRow(
            AgentSection.Nodes,
            if (resources.nodesLoaded) nodesSummary(agent, resources.nodes) else NOT_LOADED,
            error = AgentSection.Nodes in errors,
            changed = AgentSection.Nodes in changed,
            chips = {
                if (access.backend == NATIVE_BACKEND) PowerChip(backendLabel(access.backend))
            },
        ) {
            open(AgentSection.Nodes)
        }
        OverviewRow(
            AgentSection.Sandbox,
            sandboxLabel(access.sandbox),
            changed = AgentSection.Sandbox in changed,
        ) {
            open(AgentSection.Sandbox)
        }
        Eyebrow("Droits élevés", Modifier.padding(start = 16.dp, top = 16.dp, bottom = 6.dp))
        OverviewRow(
            AgentSection.Powers,
            when {
                !resources.nodesLoaded -> NOT_LOADED
                exclusive.isEmpty() && !access.installation && access.delegation != true -> "Aucun"
                exclusive.isEmpty() -> ""
                access.installation || access.delegation == true -> ""
                else -> "· gestion désactivée"
            },
            changed = AgentSection.Powers in changed,
            chips = {
                if (exclusive.isNotEmpty()) PowerChip("Exclusivité")
                if (access.installation) PowerChip("Gestion de l’installation")
                if (access.delegation == true) PowerChip("Délégation de tâches")
            },
        ) {
            open(AgentSection.Powers)
        }
        Eyebrow("Profil", Modifier.padding(start = 16.dp, top = 16.dp, bottom = 6.dp))
        OverviewRow(
            AgentSection.Identity,
            agent.description.take(60),
            error = AgentSection.Identity in errors,
            changed = AgentSection.Identity in changed,
        ) {
            open(AgentSection.Identity)
        }
        OverviewRow(
            AgentSection.Engine,
            listOfNotNull(
                    providerLabel(agent.provider),
                    agent.model.ifBlank { "modèle par défaut" },
                    effortLabel(agent.reasoning).lowercase(),
                    speedNote(agent.speed, agent.model, codexModels),
                )
                .joinToString(" · "),
            error = AgentSection.Engine in errors,
            changed = AgentSection.Engine in changed,
        ) {
            open(AgentSection.Engine)
        }
        OverviewRow(
            AgentSection.Advanced,
            draft.timeout.trim().toIntOrNull()?.let(::timeoutSummary) ?: "",
            error = AgentSection.Advanced in errors,
            changed = AgentSection.Advanced in changed,
        ) {
            open(AgentSection.Advanced)
        }
        Eyebrow("Personnes", Modifier.padding(start = 16.dp, top = 16.dp, bottom = 6.dp))
        val allowed = people.rows.count { it.known && it.allowed }
        OverviewRow(
            AgentSection.People,
            when {
                creating -> agentAccessOverview(emptyList())
                people.outdated -> "Mettez l’installation à jour pour choisir qui peut l’utiliser"
                people.error != null -> NOT_LOADED
                !people.loaded -> "Vérification des membres qui peuvent l’utiliser…"
                else -> agentAccessOverview(people.rows)
            },
            chips = {
                when {
                    people.outdated -> ToneChip(PluginBadge("Tous les membres", PluginTone.OK))
                    people.error != null ->
                        ToneChip(PluginBadge("Indisponible", PluginTone.WARNING))
                    creating || (people.loaded && allowed == 0) ->
                        ToneChip(PluginBadge("Vous seul", PluginTone.MUTED))
                    people.loaded -> ToneChip(PluginBadge("Partagé", PluginTone.OK))
                }
            },
        ) {
            open(AgentSection.People)
        }
    }
}

@Composable
internal fun SaveBar(
    count: Int,
    creating: Boolean,
    saving: Boolean,
    errors: Map<AgentSection, String>,
    serverError: String?,
    errorSection: AgentSection?,
    conflict: Boolean,
    retryable: Boolean,
    discard: () -> Unit,
    save: () -> Unit,
    reload: () -> Unit,
    open: (AgentSection) -> Unit,
) {
    Surface(
        color = MaterialTheme.colorScheme.surface,
        shadowElevation = 8.dp,
        modifier = Modifier.fillMaxWidth().testTag("agent-save-bar"),
    ) {
        Column(
            Modifier.padding(horizontal = 16.dp, vertical = 10.dp),
            verticalArrangement = Arrangement.spacedBy(6.dp),
        ) {
            if (saving) LinearProgressIndicator(Modifier.fillMaxWidth())
            serverError?.let {
                Text(it, color = MaterialTheme.colorScheme.error)
                Row {
                    if (conflict)
                        TextButton(onClick = reload, enabled = !saving) { Text("Recharger") }
                    errorSection?.let { section ->
                        TextButton(onClick = { open(section) }) { Text("Voir ${section.title}") }
                    }
                }
            }
            if (errors.isNotEmpty()) {
                val first = errors.keys.first()
                Text(
                    "À corriger : ${errors.keys.joinToString(", ") { it.title }}",
                    Modifier.clickable(onClickLabel = "Ouvrir ${first.title}") { open(first) },
                    color = MaterialTheme.colorScheme.error,
                    style = MaterialTheme.typography.bodySmall,
                )
            }
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(
                    when {
                        creating -> "● Nouvel agent"
                        count == 0 && saving -> "Enregistrement…"
                        else -> "● ${changesLabel(count)}"
                    },
                    Modifier.weight(1f),
                    style = MaterialTheme.typography.bodyMedium,
                    fontWeight = FontWeight.SemiBold,
                )
                if (count > 0 || creating)
                    TextButton(onClick = discard, enabled = !saving) { Text("Annuler") }
                Button(
                    onClick = save,
                    enabled =
                        !saving &&
                            !conflict &&
                            errors.isEmpty() &&
                            (count > 0 || creating || retryable),
                ) {
                    Text(if (creating) "Créer l’agent" else "Enregistrer")
                }
            }
        }
    }
}

/** A plugin grant change: its instance, `before → after`, and details folded by default. */
@Composable
internal fun StructuredReviewLine(line: ReviewLine, index: Int) {
    var expanded by rememberSaveable(index, line.text) { mutableStateOf(false) }
    Text(
        line.title.orEmpty(),
        style = MaterialTheme.typography.titleSmall,
        fontWeight = FontWeight.SemiBold,
        color = MaterialTheme.colorScheme.onSurface,
    )
    line.change?.let {
        Text(
            it,
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurface,
        )
    }
    if (line.details.isNotEmpty()) {
        Box(
            Modifier.heightIn(min = 40.dp)
                .clickable(role = Role.Button) { expanded = !expanded }
                .semantics {
                    contentDescription =
                        (if (expanded) "Masquer les détails de " else "Détails de ") +
                            line.title.orEmpty()
                },
            contentAlignment = Alignment.CenterStart,
        ) {
            Text(
                if (expanded) "Masquer les détails" else "Détails",
                style = MaterialTheme.typography.labelLarge,
                color = MaterialTheme.colorScheme.primary,
            )
        }
        if (expanded)
            line.details.forEach { detail ->
                Row {
                    Text("•  ", style = MaterialTheme.typography.bodySmall)
                    Text(
                        detail,
                        Modifier.weight(1f),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
    }
}

@Composable
internal fun ReviewDialog(
    review: PendingReview,
    reapprove: Set<String>,
    saving: Boolean,
    toggle: (String, Boolean) -> Unit,
    dismiss: () -> Unit,
    confirm: () -> Unit,
) {
    AlertDialog(
        onDismissRequest = { if (!saving) dismiss() },
        modifier = Modifier.testTag("agent-review"),
        title = { Text("Vérifier avant d’enregistrer") },
        text = {
            val audience = review.lines.filter { it.kind == ReviewKind.Audience }
            val changes = review.lines.filter { it.kind != ReviewKind.Audience }
            Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
                Text(
                    "Toute personne pouvant utiliser cet agent peut exercer ses permissions à travers lui. Vérifiez ces modifications pour toutes ces personnes.",
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                // Who the new rights reach stays visible above the scrolling changes.
                audience.forEach { line ->
                    Column(
                        Modifier.fillMaxWidth()
                            .clip(RoundedCornerShape(12.dp))
                            .background(MaterialTheme.colorScheme.surface)
                            .border(
                                1.dp,
                                MaterialTheme.colorScheme.outlineVariant,
                                RoundedCornerShape(12.dp),
                            )
                            .padding(12.dp)
                            .testTag("agent-review-audience"),
                        verticalArrangement = Arrangement.spacedBy(6.dp),
                    ) {
                        ToneChip(PluginBadge(line.kind.label, PluginTone.ATTENTION))
                        Text(line.text, style = MaterialTheme.typography.bodyMedium)
                    }
                }
                Column(
                    Modifier.weight(1f, fill = false).verticalScroll(rememberScrollState()),
                    verticalArrangement = Arrangement.spacedBy(12.dp),
                ) {
                    changes.forEachIndexed { index, line ->
                        Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
                            when (line.kind) {
                                ReviewKind.Power -> PowerChip(line.kind.label)
                                ReviewKind.Access ->
                                    ToneChip(PluginBadge(line.kind.label, PluginTone.OK))
                                ReviewKind.Reduction ->
                                    ToneChip(PluginBadge(line.kind.label, PluginTone.WARNING))
                                ReviewKind.Audience ->
                                    ToneChip(PluginBadge(line.kind.label, PluginTone.ATTENTION))
                            }
                            if (line.title == null)
                                Text(line.text, style = MaterialTheme.typography.bodyMedium)
                            else StructuredReviewLine(line, index)
                        }
                    }
                    review.delegations.forEach { delegation ->
                        Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
                            ToneChip(PluginBadge("Effet externe", PluginTone.ATTENTION))
                            if (delegation.blocked != null) {
                                Text(
                                    "Impossible de réapprouver « ${delegation.name} » : ${delegation.blocked}",
                                    style = MaterialTheme.typography.bodyMedium,
                                )
                                Text(
                                    "Ce bot sera suspendu jusqu’à sa reconfirmation sur l’instance.",
                                    style = MaterialTheme.typography.bodySmall,
                                    color = MaterialTheme.colorScheme.error,
                                )
                            } else {
                                val checked = delegation.id in reapprove
                                Row(
                                    Modifier.fillMaxWidth().heightIn(min = 48.dp).toggleable(
                                        value = checked,
                                        role = Role.Checkbox,
                                        enabled = !saving,
                                    ) {
                                        toggle(delegation.id, it)
                                    },
                                    verticalAlignment = Alignment.CenterVertically,
                                ) {
                                    Checkbox(checked, null)
                                    Spacer(Modifier.width(8.dp))
                                    Text(
                                        "Réapprouver les nouveaux accès pour « ${delegation.name} »",
                                        style = MaterialTheme.typography.bodyMedium,
                                    )
                                }
                                if (delegation.alreadySuspended)
                                    Text(
                                        "Déjà suspendu par une modification précédente. Ne le réapprouvez qu’après avoir vérifié ses accès actuels.",
                                        style = MaterialTheme.typography.bodySmall,
                                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                                    )
                                if (!checked)
                                    Text(
                                        "Ce bot sera suspendu jusqu’à sa reconfirmation sur l’instance.",
                                        style = MaterialTheme.typography.bodySmall,
                                        color = MaterialTheme.colorScheme.error,
                                    )
                            }
                        }
                    }
                }
            }
        },
        confirmButton = {
            Button(onClick = confirm, enabled = !saving) { Text("Enregistrer") }
        },
        dismissButton = {
            TextButton(onClick = dismiss, enabled = !saving) { Text("Revenir") }
        },
    )
}

/** « Tous, y compris futurs » / « Choisir ». */

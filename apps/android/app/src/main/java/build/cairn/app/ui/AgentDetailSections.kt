@file:OptIn(ExperimentalLayoutApi::class, ExperimentalMaterial3Api::class)

package build.cairn.app.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.toggleable
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import build.cairn.app.data.*

@Composable
internal fun ScopeChoice(all: Boolean, feminine: Boolean, change: (Boolean) -> Unit) {
    SingleChoiceSegmentedButtonRow(Modifier.fillMaxWidth()) {
        SegmentedButton(
            selected = all,
            onClick = { change(true) },
            shape = SegmentedButtonDefaults.itemShape(0, 2),
        ) {
            Text(
                if (feminine) "Toutes, y compris futures" else "Tous, y compris futurs",
                maxLines = 2,
            )
        }
        SegmentedButton(
            selected = !all,
            onClick = { change(false) },
            shape = SegmentedButtonDefaults.itemShape(1, 2),
        ) {
            Text("Choisir")
        }
    }
}

/** One choosable item; [unavailable] says why it cannot be used now, [missing] that it is gone. */
internal data class ChoiceItem(
    val id: String,
    val label: String,
    val detail: String = "",
    val unavailable: String? = null,
    val missing: Boolean = false,
)

/**
 * The « Choisir » list: search from 8 items, « Sélectionnés » / « Indisponibles » filters, a count,
 * « Tout sélectionner (filtrés) » / « Effacer », and a « Retirer » action for missing targets.
 */
@Composable
internal fun ChooseList(
    items: List<ChoiceItem>,
    selected: List<String>,
    search: String,
    change: (List<String>) -> Unit,
    extra: @Composable (ChoiceItem, Boolean) -> Unit = { _, _ -> },
) {
    var query by rememberSaveable { mutableStateOf("") }
    var onlySelected by rememberSaveable { mutableStateOf(false) }
    var onlyUnavailable by rememberSaveable { mutableStateOf(false) }
    if (items.size >= 8) SearchField(search, query) { query = it }
    if (items.isNotEmpty())
        FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            FilterChip(
                onlySelected,
                { onlySelected = !onlySelected },
                label = { Text("Sélectionnés") },
            )
            if (items.any { it.unavailable != null || it.missing })
                FilterChip(
                    onlyUnavailable,
                    { onlyUnavailable = !onlyUnavailable },
                    label = { Text("Indisponibles") },
                )
        }
    val filtered = items.filter {
        (query.isBlank() || "${it.label} ${it.detail}".contains(query.trim(), true)) &&
            (!onlySelected || it.id in selected) &&
            (!onlyUnavailable || it.unavailable != null || it.missing)
    }
    // Wraps at narrow widths instead of squeezing the count.
    FlowRow(
        Modifier.fillMaxWidth(),
        verticalArrangement = Arrangement.Center,
        itemVerticalAlignment = Alignment.CenterVertically,
    ) {
        Text(
            "${selected.count { id -> items.any { it.id == id && !it.missing } }} sur ${items.count { !it.missing }}",
            Modifier.padding(end = 8.dp),
            style = MaterialTheme.typography.labelLarge,
        )
        Spacer(Modifier.weight(1f))
        TextButton(
            onClick = {
                change(
                    (selected +
                            filtered.filter { !it.missing && it.unavailable == null }.map { it.id })
                        .distinct()
                )
            },
            enabled = filtered.isNotEmpty(),
        ) {
            Text("Tout sélectionner (filtrés)")
        }
        TextButton(
            onClick = { change(selected - filtered.map { it.id }.toSet()) },
            enabled = filtered.any { it.id in selected },
        ) {
            Text("Effacer")
        }
    }
    if (items.isEmpty()) Note("Rien à choisir pour l’instant.")
    else if (filtered.isEmpty()) Note("Aucun résultat.")
    filtered.forEach { item ->
        val checked = item.id in selected
        Column {
            Row(
                Modifier.fillMaxWidth()
                    .heightIn(min = 48.dp)
                    .then(
                        if (item.missing) Modifier
                        else
                            Modifier.toggleable(value = checked, role = Role.Checkbox) { on ->
                                change(if (on) selected + item.id else selected - item.id)
                            }
                    )
                    .semantics(mergeDescendants = true) {},
                verticalAlignment = Alignment.CenterVertically,
            ) {
                if (!item.missing) Checkbox(checked, null)
                Column(Modifier.weight(1f).padding(start = 4.dp)) {
                    Text(item.label, style = MaterialTheme.typography.bodyLarge)
                    if (item.detail.isNotBlank()) Note(item.detail)
                    item.unavailable?.let { Note(it) }
                }
                when {
                    item.missing -> {
                        ToneChip(PluginBadge("Introuvable", PluginTone.ATTENTION))
                        TextButton(onClick = { change(selected - item.id) }) { Text("Retirer") }
                    }
                    item.unavailable != null ->
                        ToneChip(PluginBadge("Indisponible", PluginTone.WARNING))
                }
            }
            extra(item, checked)
        }
    }
}

@Composable
internal fun ProjectsSection(
    access: AccessPolicy,
    projects: List<Project>,
    chooseAll: (Boolean) -> Unit,
    change: ((AccessPolicy) -> AccessPolicy) -> Unit,
) {
    Note(
        "Les projets que cet agent peut ouvrir. Une réduction vaut pour les prochaines exécutions : le travail en file qui n’est plus couvert ne démarrera pas."
    )
    ScopeChoice(access.projects == null, false, chooseAll)
    if (access.projects == null) {
        Note("Les nouveaux projets seront accessibles dès leur ajout.")
        return
    }
    val selected = access.projects
    val items =
        projects.map { ChoiceItem(it.id, it.name, it.path) } +
            selected
                .filter { id -> projects.none { it.id == id } }
                .map { ChoiceItem(it, it, missing = true) }
    ChooseList(
        items,
        selected,
        "Rechercher un projet",
        { list ->
            change { it.copy(projects = list) }
        },
    )
}

@Composable
internal fun SkillsSection(
    access: AccessPolicy,
    skills: List<Skill>,
    projects: List<Project>,
    change: ((AccessPolicy) -> AccessPolicy) -> Unit,
) {
    Note(
        "Seules les skills globales et celles des projets autorisés peuvent être choisies. S’applique aux prochaines exécutions."
    )
    val choosable = choosableSkills(skills, access)
    ScopeChoice(access.skills == null, true) { all ->
        // Choosing starts with no skill selected, as on the web.
        change { it.copy(skills = if (all) null else it.skills ?: emptyList()) }
    }
    if (access.skills == null) {
        Note("${choosable.size} disponibles · limitées aux projets autorisés.")
        return
    }
    val selected = access.skills
    val items =
        choosable.map { ChoiceItem(skillKey(it), skillLabel(it, projects), it.description) } +
            selected
                .filter { key -> choosable.none { skillKey(it) == key } }
                .map { key ->
                    val exists = skills.any { skillKey(it) == key }
                    ChoiceItem(
                        key,
                        skillKeyLabel(key, projects),
                        unavailable = if (exists) "Son projet n’est pas autorisé." else null,
                        missing = !exists,
                    )
                }
    ChooseList(
        items,
        selected,
        "Rechercher une skill",
        { list ->
            change { it.copy(skills = list) }
        },
    )
}

@Composable
internal fun McpTools(
    mcp: Mcp,
    access: AccessPolicy,
    change: ((AccessPolicy) -> AccessPolicy) -> Unit,
) {
    var open by rememberSaveable(mcp.id) { mutableStateOf(false) }
    val enabled = mcp.tools.filter { mcp.enabledTools == null || it.name in mcp.enabledTools }
    val chosen = access.mcpTools[mcp.id]
    Column(Modifier.padding(start = 12.dp)) {
        TextButton(onClick = { open = !open }) {
            Text(
                "Outils : " +
                    if (chosen == null) "tous les outils activés"
                    else
                        "${enabled.size} activés sur la connexion · ${chosen.count { name -> enabled.any { it.name == name } }} choisis"
            )
        }
        if (open) {
            Toggle("Tous les outils activés de « ${mcp.name} »", chosen == null) { all ->
                change {
                    it.copy(
                        mcpTools =
                            build.cairn.contracts.McpTools(
                                if (all) it.mcpTools - mcp.id
                                // Choosing starts with no tool selected, as on the web.
                                else it.mcpTools + (mcp.id to emptyList())
                            )
                    )
                }
            }
            if (chosen != null)
                enabled.forEach { tool ->
                    Toggle(tool.title ?: tool.name, tool.name in chosen) { on ->
                        change {
                            val old = it.mcpTools[mcp.id].orEmpty()
                            it.copy(
                                mcpTools =
                                    build.cairn.contracts.McpTools(
                                        it.mcpTools +
                                            (mcp.id to if (on) old + tool.name else old - tool.name)
                                    )
                            )
                        }
                    }
                }
            if (enabled.isEmpty())
                Note("Testez la connexion dans Serveurs MCP pour découvrir ses outils.")
        }
    }
}

@Composable
internal fun McpSection(
    access: AccessPolicy,
    mcps: List<Mcp>,
    openMcps: () -> Unit,
    change: ((AccessPolicy) -> AccessPolicy) -> Unit,
) {
    Note(
        "L’accès réel combine les connexions accordées ici et les outils activés sur chaque connexion. S’applique aux prochaines exécutions."
    )
    ScopeChoice(access.mcps == null, true) { all ->
        change {
            it.copy(
                // Choosing starts with no connection selected, as on the web.
                mcps = if (all) null else it.mcps ?: emptyList(),
                mcpTools = it.mcpTools,
            )
        }
    }
    if (access.mcps == null) {
        if (mcps.isEmpty()) Note("Les nouvelles connexions MCP seront accessibles dès leur ajout.")
        mcps.forEach { mcp ->
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(mcp.name, Modifier.weight(1f), style = MaterialTheme.typography.bodyLarge)
                if (!mcp.enabled)
                    ToneChip(PluginBadge("Désactivée sur l’installation", PluginTone.WARNING))
            }
            McpTools(mcp, access, change)
        }
        return
    }
    val selected = access.mcps
    if (mcps.isEmpty()) {
        Empty("Aucune connexion MCP", "Ajoutez une connexion pour la proposer à vos agents.")
        TextButton(onClick = openMcps) { Text("Ouvrir les serveurs MCP ›") }
    }
    val items =
        mcps.map {
            ChoiceItem(
                it.id,
                it.name,
                it.url.ifBlank { it.command },
                unavailable = if (it.enabled) null else "Désactivée sur l’installation.",
            )
        } +
            selected
                .filter { id -> mcps.none { it.id == id } }
                .map { ChoiceItem(it, it, missing = true) }
    ChooseList(
        items,
        selected,
        "Rechercher une connexion",
        { list ->
            // Tool choices stay in the draft: granting the connection again restores them.
            change { it.copy(mcps = list) }
        },
    ) { item, checked ->
        mcps.find { it.id == item.id }?.let { if (checked) McpTools(it, access, change) }
    }
}

@Composable
internal fun RadioRow(
    label: String,
    detail: String,
    selected: Boolean,
    enabled: Boolean = true,
    select: () -> Unit,
) {
    Row(
        Modifier.fillMaxWidth()
            .heightIn(min = 56.dp)
            .selectable(selected, enabled = enabled, role = Role.RadioButton, onClick = select)
            .padding(vertical = 4.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        RadioButton(selected, null, enabled = enabled)
        Column(Modifier.weight(1f).padding(start = 8.dp)) {
            Text(
                label,
                style = MaterialTheme.typography.bodyLarge,
                color =
                    if (enabled) MaterialTheme.colorScheme.onSurface
                    else MaterialTheme.colorScheme.onSurfaceVariant,
            )
            if (detail.isNotBlank()) Note(detail)
        }
    }
}

@Composable
internal fun SwitchRow(
    label: String,
    detail: String,
    checked: Boolean,
    enabled: Boolean = true,
    power: Boolean = false,
    description: String = label,
    change: (Boolean) -> Unit,
) {
    val (_, powerContent) = powerColors()
    Row(
        Modifier.fillMaxWidth()
            .heightIn(min = 56.dp)
            .toggleable(
                value = checked,
                enabled = enabled,
                role = Role.Switch,
                onValueChange = change,
            )
            .semantics(mergeDescendants = true) { contentDescription = description }
            .padding(vertical = 4.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Column(Modifier.weight(1f)) {
            Text(
                label,
                style = MaterialTheme.typography.bodyLarge,
                color =
                    if (enabled) MaterialTheme.colorScheme.onSurface
                    else MaterialTheme.colorScheme.onSurfaceVariant,
            )
            if (detail.isNotBlank()) Note(detail)
        }
        Spacer(Modifier.width(12.dp))
        Switch(
            checked,
            null,
            enabled = enabled,
            colors =
                if (power)
                    SwitchDefaults.colors(
                        checkedTrackColor = powerContent,
                        checkedBorderColor = powerContent,
                    )
                else SwitchDefaults.colors(),
        )
    }
}

internal fun nodeStatus(node: ExecutionNode): PluginBadge =
    when {
        node.local -> PluginBadge("En ligne", PluginTone.OK)
        node.status == "offline" -> PluginBadge("Hors ligne", PluginTone.WARNING)
        node.maintenance != null || (node.status == "online" && !node.executionReady) ->
            PluginBadge("Runtime à mettre à jour", PluginTone.WARNING)
        !node.accepting -> PluginBadge("N’accepte pas de travaux", PluginTone.MUTED)
        else -> PluginBadge("En ligne", PluginTone.OK)
    }

@Composable
internal fun NodesSection(
    agent: Agent,
    nodes: List<ExecutionNode>,
    loaded: Boolean,
    failed: Boolean,
    retry: () -> Unit,
    change: ((Agent) -> Agent) -> Unit,
) {
    val access = agent.access
    // Without the machine list, any change could restrict the agent to the current runner.
    val editable = loaded && !failed
    if (!loaded && !failed) LinearProgressIndicator(Modifier.fillMaxWidth())
    if (failed)
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text(
                "Impossible de charger les nodes. Leurs accès ne peuvent pas être modifiés pour l’instant.",
                Modifier.weight(1f),
                color = MaterialTheme.colorScheme.error,
            )
            TextButton(onClick = retry) { Text("Réessayer") }
        }
    var explain by remember { mutableStateOf(false) }
    var query by rememberSaveable { mutableStateOf("") }
    val native = access.backend == NATIVE_BACKEND
    val automatic = access.backend == "auto"
    // Only machines of the agent's backend can run it; the current runner is a VM runner.
    val choices = agent.nodeChoices(nodes)
    SwitchRow(
        "Choix automatique · VM ou natif",
        "Les nouvelles conversations suivent les priorités de cet agent, puis les autres nodes autorisées disponibles. Les conversations existantes gardent leur environnement jusqu’à une migration explicite.",
        automatic,
        enabled = editable,
    ) { on ->
        change { it.withBackend(if (on) "auto" else VM_BACKEND) }
    }
    if (!automatic)
        SwitchRow(
            "Exécution native sur l’hôte",
            if (native)
                "Partage le GPU et les outils de l’hôte, avec une isolation plus faible qu’une VM. Chaque node doit aussi autoriser les personnes qui dirigent cet agent."
            else "Dans une VM Firecracker isolée.",
            native,
            enabled = editable,
            power = true,
        ) { on ->
            change { it.withBackend(on) }
        }
    Text("Priorités de cet agent", style = MaterialTheme.typography.titleSmall)
    Note(
        "Premier node autorisé disponible, puis les suivants. Une priorité ne donne aucun droit d’accès."
    )
    NodePriorityList(
        ids = agent.nodePriorities,
        name = { id ->
            choices.find { it.id == id }?.name ?: nodes.find { it.id == id }?.name ?: id
        },
        enabled = editable,
        reorder = { order -> change { it.copy(nodePriorities = order) } },
        remove = { id -> change { it.copy(nodePriorities = it.nodePriorities - id) } },
    )
    choices
        .filter { it.id !in agent.nodePriorities }
        .forEach { node ->
            TextButton(
                enabled = editable && access.runsOn(node.id),
                onClick = { change { it.copy(nodePriorities = it.nodePriorities + node.id) } },
            ) {
                Text("Prioriser ${node.name}")
            }
        }
    HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
    SwitchRow(
        if (automatic) "Toutes les nodes, y compris futures"
        else if (native) "Toutes les nodes natives, y compris futures"
        else "Toutes les nodes VM, y compris futures",
        if (native || automatic)
            "Chaque node doit aussi activer l’exécution native et autoriser toutes les personnes qui dirigent la conversation."
        else "Les nouvelles machines seront utilisables par cet agent dès leur inscription.",
        access.nodes == null,
        enabled = editable,
    ) { all ->
        change { it.withAllNodes(all, nodes) }
    }
    if (native && loaded && choices.isEmpty())
        Note("Aucune node native. Inscrivez une machine en exécution native depuis Nodes.")
    if (!loaded) {
        // The machines are unknown: what the agent holds is listed as saved, read-only.
        Note(
            when {
                access.nodes == null ->
                    if (native) "Exécution autorisée sur toutes les nodes natives de confiance."
                    else "Exécution autorisée sur toutes les nodes VM."
                access.nodes.isEmpty() -> "Exécution autorisée sur aucune node."
                else -> "Exécution autorisée sur : ${access.nodes.joinToString(", ")}"
            }
        )
        if (agent.exclusiveNodes.isNotEmpty())
            Note("Réservation exclusive sur : ${agent.exclusiveNodes.joinToString(", ")}")
    }
    if (loaded)
        agent.unusableNodes(nodes).forEach { held ->
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(
                    if (held.otherBackend) "« ${held.name} » · autre mode d’exécution"
                    else "Node supprimée ou révoquée",
                    Modifier.weight(1f),
                )
                ToneChip(
                    if (held.otherBackend) PluginBadge("Indisponible", PluginTone.ATTENTION)
                    else PluginBadge("Introuvable", PluginTone.ATTENTION)
                )
                TextButton(
                    onClick = { change { it.withNodeRun(held.id, false) } },
                    modifier =
                        Modifier.semantics {
                            contentDescription = "Retirer ${held.name ?: held.id}"
                        },
                ) {
                    Text("Retirer")
                }
            }
        }
    if (loaded && choices.size >= 8)
        SearchField("Rechercher une node ou un tag", query) { query = it }
    (if (loaded) choices else emptyList())
        .filter {
            query.isBlank() ||
                "${it.name} ${it.tags.joinToString(" ")} ${it.capabilities.os}"
                    .contains(query.trim(), true)
        }
        .forEach { node ->
            val runs = access.runsOn(node.id)
            val exclusive = node.id in agent.exclusiveNodes
            Column(
                Modifier.fillMaxWidth().testTag("agent-node-${node.id}"),
                verticalArrangement = Arrangement.spacedBy(2.dp),
            ) {
                Row(
                    Modifier.padding(top = 10.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Text(
                        node.name,
                        style = MaterialTheme.typography.titleSmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                    Spacer(Modifier.width(8.dp))
                    ToneChip(nodeStatus(node))
                }
                val facts =
                    (node.tags +
                            listOfNotNull(
                                node.capabilities.os.ifBlank { null },
                                if (node.local) null else "${node.capabilities.cpu} CPU",
                            ))
                        .joinToString(" · ")
                if (facts.isNotBlank()) Note(facts)
                val administrator = node.nativeModeRequest?.mode == "administrator"
                if (administrator)
                    Note(
                        "Natif administrateur : accorder l’accès donne sudo sans mot de passe sur l’hôte réel. Root peut lire les fichiers et identifiants locaux, modifier les services et contourner les limites coopératives. Le retour en protégé ne restaure pas la machine."
                    )
                SwitchRow(
                    "Exécuter ses conversations",
                    if (administrator && agent.id !in node.nativeReviewedAgents)
                        "Accès administrateur à examiner par le propriétaire"
                    else if (access.nodes == null) "Inclus dans « Toutes les nodes »" else "",
                    runs,
                    enabled = editable && access.nodes != null,
                    power = administrator,
                    description = "Exécuter ses conversations sur ${node.name}",
                ) { on ->
                    change { it.withNodeRun(node.id, on) }
                }
                HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
                val supported = node.exclusiveReservations
                SwitchRow(
                    "Réserver en exclusivité",
                    when {
                        !supported -> "Indisponible sur ce runtime"
                        !runs -> "Nécessite le droit d’exécuter"
                        else -> "Suspend les autres conversations"
                    },
                    exclusive,
                    enabled = editable && runs && supported,
                    power = true,
                    description = "Réserver ${node.name} en exclusivité",
                ) { on ->
                    change { it.withExclusive(node.id, on) }
                }
                if (exclusive && (!runs || !supported))
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Note(
                            if (!runs) "Inactif : ne peut pas s’exécuter ici."
                            else "Inactif tant que le runtime n’est pas mis à jour.",
                            Modifier.weight(1f),
                        )
                        TextButton(onClick = { change { it.withExclusive(node.id, false) } }) {
                            Text("Retirer")
                        }
                    }
                HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
            }
        }
    if (nodes.isNotEmpty())
        agent.exclusiveNodes
            .filter { id -> id != LOCAL_NODE_ID && choices.none { it.id == id } }
            .forEach { id ->
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text("Node supprimée ou révoquée", Modifier.weight(1f))
                    ToneChip(PluginBadge("Introuvable", PluginTone.ATTENTION))
                    TextButton(onClick = { change { it.withExclusive(id, false) } }) {
                        Text("Retirer")
                    }
                }
            }
    Text(
        "ⓘ Exécuter ou suspendre les autres : quelle différence ?",
        Modifier.fillMaxWidth()
            .clip(RoundedCornerShape(14.dp))
            .background(MaterialTheme.colorScheme.secondaryContainer)
            .clickable { explain = true }
            .padding(14.dp),
        color = MaterialTheme.colorScheme.onSecondaryContainer,
    )
    if (explain)
        DetailSheet("Exécuter ou suspendre les autres", { explain = false }) {
            Text(
                "Exécuter : ses conversations peuvent démarrer ou être déplacées sur cette machine et partagent ses slots et budgets avec les autres agents."
            )
            Text(
                "Réserver en exclusivité : pendant un travail intensif, l’agent peut suspendre toutes les autres conversations de la machine, pour 15 minutes par demande et 1 heure au total avec les renouvellements. Elles reprennent automatiquement. Ce droit exige le droit d’exécuter."
            )
            Text(
                "Ces droits s’appliquent immédiatement, y compris dans les conversations en cours, et ne bloquent jamais le travail en file."
            )
        }
}

@Composable
internal fun SandboxSection(value: String, backend: String, change: (String) -> Unit) {
    Note("Un mode plus strict bloque les exécutions en file qui demandaient plus.")
    SANDBOX_CHOICES.forEach { (key, label, effect) ->
        val description =
            when {
                key != "yolo" || backend == VM_BACKEND -> effect
                backend == "auto" ->
                    "Les droits suivent le node choisi : VM privée ou hôte natif de confiance."
                else ->
                    "Processus natif. Les droits de l’hôte suivent son mode protégé ou administrateur confirmé."
            }
        RadioRow(label, description, value == key) { change(key) }
    }
}

@Composable
internal fun PowersSection(
    agent: Agent,
    nodes: List<ExecutionNode>,
    nodesLoaded: Boolean,
    open: (AgentSection) -> Unit,
    change: ((AccessPolicy) -> AccessPolicy) -> Unit,
) {
    val access = agent.access
    val (container, content) = powerColors()
    Column(
        Modifier.fillMaxWidth()
            .clip(RoundedCornerShape(16.dp))
            .background(container.copy(alpha = 0.5f))
            .padding(14.dp),
        verticalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        Text("Droits élevés", color = content, style = MaterialTheme.typography.titleSmall)
        SwitchRow(
            "Gérer l’installation Cairn",
            "Créer et modifier les agents (y compris leurs permissions), projets, skills, connexions MCP, missions et exécutions de toute l’installation, au nom du propriétaire. S’applique même si les accès ci-dessus sont restreints.",
            access.installation,
            power = true,
        ) { on ->
            change { it.copy(installation = on) }
        }
        SwitchRow(
            "Déléguer des tâches",
            "Confier des tâches d’une conversation à de nouvelles conversations qui lui rendent compte, sur tout agent de code et modèle. Chacune s’exécute en tant que cet agent, ou qu’un agent dont il couvre les accès, dans son propre espace de travail. Une conversation déléguée ne délègue pas à son tour, et 5 au plus travaillent en même temps.",
            access.delegation == true,
            power = true,
        ) { on ->
            change { it.copy(delegation = on) }
        }
    }
    val exclusive = agent.effectiveExclusive(nodes)
    Panel {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text(
                "Réservation exclusive",
                Modifier.weight(1f),
                style = MaterialTheme.typography.titleMedium,
            )
            if (exclusive.isNotEmpty())
                PowerChip(if (exclusive.size == 1) "1 node" else "${exclusive.size} nodes")
        }
        Text(
            when {
                !nodesLoaded -> NOT_LOADED
                exclusive.isEmpty() -> "Aucune node"
                else -> "Accordée sur ${exclusive.joinToString(", ") { it.name }}"
            }
        )
        TextButton(onClick = { open(AgentSection.Nodes) }) { Text("Régler par node ›") }
    }
    Note(
        "La réservation exclusive et les accès aux plugins restent réservés au propriétaire : un agent qui gère l’installation ne peut pas les accorder."
    )
    Panel {
        Text("Toujours disponible", style = MaterialTheme.typography.titleMedium)
        Text(
            "Publier des livrables (y compris en lien public), attendre un délai ou une condition, voir ses nodes et y déplacer la conversation, rendre compte du résultat."
        )
    }
}

@Composable
internal fun IdentitySection(
    vm: CairnViewModel,
    state: Workspace,
    saved: Agent,
    agent: Agent,
    creating: Boolean,
    update: ((Agent) -> Agent) -> Unit,
) {
    Field("Nom", agent.name, { value -> update { it.copy(name = value) } })
    Field("Description", agent.description, { value -> update { it.copy(description = value) } }, 3)
    Field(
        "Instructions",
        agent.instructions,
        { value -> update { it.copy(instructions = value) } },
        6,
    )
    if (!creating) AgentPortraitEditor(vm, state, saved)
    else Note("Le portrait pourra être ajouté après la création.")
}

@Composable
internal fun EngineSection(
    vm: CairnViewModel,
    state: Workspace,
    agent: Agent,
    update: ((Agent) -> Agent) -> Unit,
    configurationEnabled: Boolean,
    saved: Agent?,
    openConnections: () -> Unit,
    configuration: () -> Unit,
) {
    ModelPicker(
        state.modelCatalog(agent.provider),
        agent.model,
        agent.reasoning,
        defaultModel = agent.codingAgentDefaults?.get(agent.provider)?.model.orEmpty(),
        defaultReasoning = agent.codingAgentDefaults?.get(agent.provider)?.reasoning.orEmpty(),
        defaultSpeed = agent.codingAgentDefaults?.get(agent.provider)?.speed.orEmpty(),
        inherit = agent.revision.isNotEmpty(),
        enabled = !state.busy,
        refresh = { vm.refreshModels(agent.provider) },
        provider = agent.provider,
        changeProvider = { provider ->
            val previous = saved?.takeIf { it.provider == provider }
            update {
                it.copy(
                    provider = provider,
                    model = previous?.model.orEmpty(),
                    reasoning = previous?.reasoning.orEmpty(),
                    speed = previous?.speed.orEmpty(),
                )
            }
        },
        field = true,
        speed = agent.speed,
        changeSpeed = { speed -> update { it.copy(speed = speed) } },
        catalogs = state.modelCatalogs,
        savedProvider = saved?.provider,
        requireConnection = agent.provider != saved?.provider,
        refreshCatalog = { vm.refreshModels(it, availabilityOnly = true) },
        // Connections is pushed on the navigation stack, so Back restores the saved draft.
        openConnections = openConnections,
    ) { model, reasoning ->
        update { it.copy(model = model, reasoning = reasoning) }
    }
    InfoBox(
        "Utilise le pool partagé de comptes ${providerLabel(agent.provider)} de l’installation. Ce n’est pas une permission."
    )
    OutlinedButton(onClick = configuration, enabled = configurationEnabled) {
        Text("Configuration native")
    }
    Note(
        if (configurationEnabled) "Contexte, fonctionnalités natives et options avancées."
        else "Enregistrez l’agent avant d’ouvrir sa configuration native."
    )
}

@Composable
internal fun AdvancedSection(timeout: String, change: (String) -> Unit) {
    val minutes = timeout.trim().toIntOrNull()
    Toggle("Sans limite de temps", minutes == 0) { change(if (it) "0" else "120") }
    if (minutes != 0)
        Field(
            "Limite en minutes (1–720)",
            timeout,
            change,
            keyboardOptions = InputKeyboards.Number,
        )
    Note("Une exécution qui dépasse cette limite est arrêtée.")
}

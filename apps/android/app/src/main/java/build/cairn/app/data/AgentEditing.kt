package build.cairn.app.data

import kotlinx.serialization.Serializable
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.encodeToJsonElement
import kotlinx.serialization.json.jsonObject

/*
 * The agent page: one draft for everything an agent can reach or do, its summaries, what needs
 * checking and what saving it changes. Pure functions so they can be tested without Compose.
 */

typealias AgentPreviewRuns = build.cairn.contracts.AgentPreviewRuns

typealias AgentDelegationEffect = build.cairn.contracts.AgentDelegationEffect

typealias AgentPreview = build.cairn.contracts.AgentPreview

/** The sections of the agent page, in the order of its overview. */
enum class AgentSection(val title: String) {
    Projects("Projets"),
    Skills("Skills"),
    Mcp("MCP & outils"),
    Plugins("Secrets & plugins"),
    Nodes("Nodes"),
    Sandbox("Mode d’exécution"),
    Powers("Pouvoirs spéciaux"),
    Identity("Identité & instructions"),
    Engine("Moteur & modèle"),
    Advanced("Avancé"),
}

/** Sections the server fixes for the main agent: always everything, including future ones. */
val MAIN_LOCKED_SECTIONS = setOf(AgentSection.Projects, AgentSection.Skills, AgentSection.Mcp)

/**
 * The page's single draft. [timeout] keeps the field text so it can be validated. External accounts
 * such as GitHub are plugin instances granted in [Agent.plugins], like any other plugin.
 */
@Serializable
data class AgentDraft(val agent: Agent, val timeout: String = agent.timeoutMinutes.toString())

/** « Tous » / « Choisir » for projects. */
fun AgentDraft.withAllProjects(all: Boolean): AgentDraft =
    copy(agent = agent.copy(access = agent.access.withAllProjects(all)))

/**
 * The draft of a new agent: it starts with no access and gains only what the owner grants. The
 * server's defaults for existing agents are unchanged.
 */
fun newAgent() =
    Agent(
        id = "00000000-0000-4000-8000-000000000000",
        name = "",
        revision = "",
        createdAt = 0,
        access =
            AccessPolicy(
                nodes = listOf(LOCAL_NODE_ID),
                projects = emptyList(),
                skills = emptyList(),
                mcps = emptyList(),
                mcpTools = build.cairn.contracts.McpTools(emptyMap()),
                installation = false,
                sandbox = "yolo",
                backend = VM_BACKEND,
            ),
        exclusiveNodes = emptyList(),
        plugins = emptyList(),
    )

/** Everything the page shows the agent's access against. */
data class AgentResources(
    val projects: List<Project> = emptyList(),
    val skills: List<Skill> = emptyList(),
    val mcps: List<Mcp> = emptyList(),
    val nodes: List<ExecutionNode> = emptyList(),
    val catalog: PluginCatalog? = null,
    /** False while the machine list is loading or failed to load. */
    val nodesLoaded: Boolean = true,
    /** Whether the installation's skills have loaded, so a missing one can be reported. */
    val skillsLoaded: Boolean = true,
)

/** Shown instead of a summary computed from something that has not loaded. */
const val NOT_LOADED = "Non chargé"

/**
 * MCP tool choices only for connections the agent is granted: the server rejects the others. All
 * are kept when every connection is granted.
 */
fun AccessPolicy.withGrantedTools(existing: Set<String>? = null): AccessPolicy =
    copy(
        mcpTools =
            build.cairn.contracts.McpTools(
                mcpTools.filterKeys {
                    (mcps == null || it in mcps) && (existing == null || it in existing)
                }
            )
    )

/** The agent as it will be sent: the parsed time limit applied. */
fun AgentDraft.resolved(): Agent =
    agent.copy(timeoutMinutes = timeout.trim().toIntOrNull() ?: agent.timeoutMinutes)

/**
 * Whether [edited] differs from the saved [base] in anything the agent save sends. Identity,
 * revision and portrait come from the server.
 */
fun profileChanged(base: Agent, edited: Agent): Boolean =
    edited.copy(
        id = base.id,
        revision = base.revision,
        createdAt = base.createdAt,
        avatar = base.avatar,
    ) != base

/** The section of every unsaved change, once per changed setting. */
fun agentChanges(original: Agent, draft: AgentDraft): List<AgentSection> {
    val edited = draft.agent
    val before = original.access
    val after = edited.access
    return buildList {
        if (edited.name != original.name) add(AgentSection.Identity)
        if (edited.description != original.description) add(AgentSection.Identity)
        if (edited.instructions != original.instructions) add(AgentSection.Identity)
        if (
            edited.provider != original.provider ||
                edited.model != original.model ||
                edited.reasoning != original.reasoning ||
                edited.speed != original.speed
        )
            add(AgentSection.Engine)
        if (draft.timeout.trim() != original.timeoutMinutes.toString()) add(AgentSection.Advanced)
        if (after.projects != before.projects) add(AgentSection.Projects)
        if (after.skills != before.skills) add(AgentSection.Skills)
        // Tool choices compare as sets: their order is not a change.
        // Only choices of granted connections are saved; the others wait in the draft.
        val toolsChanged =
            after.withGrantedTools().mcpTools.mapValues { it.value.toSet() } !=
                before.withGrantedTools().mcpTools.mapValues { it.value.toSet() }
        if (after.mcps != before.mcps || toolsChanged) add(AgentSection.Mcp)
        if (edited.plugins.toSet() != original.plugins.toSet()) add(AgentSection.Plugins)
        if (after.backend != before.backend) add(AgentSection.Nodes)
        if (after.nodes?.toSet() != before.nodes?.toSet()) add(AgentSection.Nodes)
        if (edited.exclusiveNodes.toSet() != original.exclusiveNodes.toSet())
            add(AgentSection.Nodes)
        if (after.sandbox != before.sandbox) add(AgentSection.Sandbox)
        if (after.installation != before.installation) add(AgentSection.Powers)
        if ((after.delegation == true) != (before.delegation == true)) add(AgentSection.Powers)
    }
}

fun changesLabel(count: Int) =
    if (count == 1) "1 modification non enregistrée" else "$count modifications non enregistrées"

/** Problems that prevent saving, shown in their section. */
fun agentErrors(draft: AgentDraft): Map<AgentSection, String> = buildMap {
    val agent = draft.agent
    val identity =
        when {
            agent.name.isBlank() -> "Donnez un nom à cet agent."
            agent.name.length > 100 -> "Le nom doit faire 100 caractères au maximum."
            agent.description.length > 500 -> "La description doit faire 500 caractères au maximum."
            agent.instructions.length > 20000 ->
                "Les instructions doivent faire 20 000 caractères au maximum."
            else -> null
        }
    identity?.let { put(AgentSection.Identity, it) }
    if (
        agent.model.length > 100 ||
            agent.reasoning.length > 40 ||
            !Regex("(?:[a-z][a-z0-9_-]*)?").matches(agent.reasoning) ||
            agent.speed.length > 40 ||
            !Regex("(?:[a-z][a-z0-9_-]*)?").matches(agent.speed)
    )
        put(AgentSection.Engine, "Choisissez un modèle et un niveau de réflexion valides.")
    if (draft.timeout.trim().toIntOrNull() !in 0..720)
        put(AgentSection.Advanced, "Saisissez une limite entre 1 et 720 minutes.")
}

/** Which section a server error belongs to, when its message names one. */
fun sectionOfError(message: String): AgentSection? {
    val text = message.lowercase()
    return when {
        "exclusive" in text || "node" in text -> AgentSection.Nodes
        "plugin" in text || "instance" in text || "delegation" in text -> AgentSection.Plugins
        "mcp" in text || "tool" in text -> AgentSection.Mcp
        "skill" in text -> AgentSection.Skills
        "project" in text -> AgentSection.Projects
        "sandbox" in text -> AgentSection.Sandbox
        "installation" in text -> AgentSection.Powers
        "model" in text || "reasoning" in text || "provider" in text || "speed" in text ->
            AgentSection.Engine
        "timeout" in text -> AgentSection.Advanced
        "name" in text || "instructions" in text || "description" in text -> AgentSection.Identity
        else -> null
    }
}

/** "Tous, y compris futurs" | "3 sur 12" | "Aucun". */
fun selectionSummary(selected: List<String>?, total: Int, feminine: Boolean = false): String =
    when {
        selected == null -> if (feminine) "Toutes, y compris futures" else "Tous, y compris futurs"
        selected.isEmpty() -> if (feminine) "Aucune" else "Aucun"
        else -> "${selected.size} sur $total"
    }

/**
 * The node list of the agent page: the current runner first, then enrolled machines. With a
 * [backend], only machines of that execution backend (the server refuses the others); the current
 * runner is a VM runner.
 */
fun agentNodeChoices(nodes: List<ExecutionNode>, backend: String? = null): List<ExecutionNode> {
    val local =
        nodes.find { it.local || it.id == LOCAL_NODE_ID }
            ?: ExecutionNode(
                LOCAL_NODE_ID,
                "Runner actuel",
                local = true,
                accepting = true,
                status = "online",
            )
    val runner =
        listOf(local.copy(id = LOCAL_NODE_ID, name = "Runner actuel")).filter {
            backend == null || backend != NATIVE_BACKEND
        }
    return runner +
        nodes.filter {
            !it.local &&
                it.id != LOCAL_NODE_ID &&
                !it.revoked &&
                (backend == null ||
                    (it.capabilities.backend == NATIVE_BACKEND) == (backend == NATIVE_BACKEND))
        }
}

/** A run permission the agent holds on a node it cannot use. */
data class HeldNode(val id: String, val name: String?, val otherBackend: Boolean)

/**
 * Run permissions on nodes that are gone, revoked or of the other execution backend: kept by the
 * server until removed, they never let the agent run there.
 */
fun Agent.unusableNodes(nodes: List<ExecutionNode>): List<HeldNode> {
    val held = access.nodes ?: return emptyList()
    val usable = nodeChoices(nodes).map { it.id }.toSet()
    val known = agentNodeChoices(nodes)
    return held
        .filter { it !in usable }
        .map { id ->
            val node = known.find { it.id == id }
            HeldNode(id, node?.name, otherBackend = node != null)
        }
}

/** The node list for this agent's execution backend. */
fun Agent.nodeChoices(nodes: List<ExecutionNode>) = agentNodeChoices(nodes, access.backend)

/**
 * Switches between a VM and native execution on the host. Nodes of the other backend no longer
 * apply, so node execution and exclusive reservations start again from none.
 */
fun Agent.withBackend(native: Boolean): Agent {
    val backend = if (native) NATIVE_BACKEND else VM_BACKEND
    if (backend == access.backend) return this
    return copy(
        access = access.copy(backend = backend, nodes = emptyList()),
        exclusiveNodes = emptyList(),
    )
}

/** "VM" or "Native", as the overview shows the execution backend. */
fun backendLabel(backend: String) = if (backend == NATIVE_BACKEND) "Exécution native" else "VM"

fun AccessPolicy.runsOn(node: String) = nodes == null || node in nodes

/** Exclusive grants that take effect: the agent may also run there and the runtime supports it. */
fun Agent.effectiveExclusive(nodes: List<ExecutionNode>): List<ExecutionNode> =
    nodeChoices(nodes).filter {
        it.id in exclusiveNodes && access.runsOn(it.id) && it.exclusiveReservations
    }

/** Allows or stops running on one node. Stopping also drops the exclusive grant there. */
fun Agent.withNodeRun(node: String, run: Boolean): Agent {
    val current = access.nodes ?: return this
    return copy(
        access = access.copy(nodes = if (run) (current + node).distinct() else current - node),
        exclusiveNodes = if (run) exclusiveNodes else exclusiveNodes - node,
    )
}

/**
 * "All nodes, including future": turning it off keeps every machine known today, so nothing is
 * removed except future machines. Without the machine list (it failed to load), nothing changes:
 * restricting then would keep the current runner only.
 */
fun AccessPolicy.withAllNodes(
    all: Boolean,
    nodes: List<ExecutionNode>,
    agentId: String = "",
): AccessPolicy =
    when {
        all -> copy(nodes = null)
        this.nodes != null -> this
        nodes.isEmpty() -> this
        else ->
            copy(
                nodes =
                    agentNodeChoices(nodes, backend)
                        .filter {
                            it.nativeModeRequest?.mode != "administrator" ||
                                agentId in it.nativeReviewedAgents
                        }
                        .map { it.id }
            )
    }

fun Agent.withExclusive(node: String, granted: Boolean): Agent =
    copy(
        exclusiveNodes = if (granted) (exclusiveNodes + node).distinct() else exclusiveNodes - node
    )

/** Choosing projects starts with none selected, as on the web. */
fun AccessPolicy.withAllProjects(all: Boolean): AccessPolicy =
    if (all) copy(projects = null) else copy(projects = this.projects ?: emptyList())

fun skillKey(skill: Skill) = "${skill.scope}/${skill.name}"

/** "review · Global", "lint · cairn": the skill's name and where it lives. */
fun skillLabel(skill: Skill, projects: List<Project>): String =
    "${skill.name} · ${if (skill.scope == "global") "Global" else projects.find { it.id == skill.scope }?.name ?: skill.scope}"

/** The label of a selected skill key, including keys whose skill no longer exists. */
fun skillKeyLabel(key: String, projects: List<Project>): String {
    val scope = key.substringBefore('/')
    val name = key.substringAfter('/')
    return skillLabel(Skill(name = name, scope = scope), projects)
}

/** Only global skills and skills of projects the agent may open can be chosen. */
fun choosableSkills(skills: List<Skill>, access: AccessPolicy): List<Skill> = skills.filter {
    it.valid && (it.scope == "global" || access.projects == null || it.scope in access.projects)
}

/**
 * Whether the package's instances can be granted to agents: an approved version runs and its
 * manifest gives agents commands, tools or instructions.
 */
fun PluginEntry.agentGrantable() =
    manifest?.agentAccess != null && (status == "active" || status == "update")

/** Plugin instances whose package gives agents access, grouped by package. */
fun PluginCatalog.agentGrantable(): List<Pair<PluginEntry, List<PluginConnection>>> =
    packages
        .filter { it.agentGrantable() }
        .map { entry -> entry to connections.filter { it.pluginId == entry.id } }
        .filter { it.second.isNotEmpty() }

/**
 * Instances the agent holds whose package no longer gives agents access (not approved, invalid or
 * without agent access): the grant is kept but has no effect, so it is shown to be removed.
 */
fun PluginCatalog.unavailableGrants(plugins: List<String>): List<PluginConnection> =
    connections.filter { connection ->
        connection.id in plugins &&
            packages.find { it.id == connection.pluginId }?.agentGrantable() != true
    }

private fun plural(count: Int, singular: String, plural: String = "${singular}s") =
    "$count ${if (count == 1) singular else plural}"

/** "1Password : 1 compte · Discord", or empty when the agent has no plugin grant. */
fun pluginSummary(agent: Agent, catalog: PluginCatalog?): String {
    if (agent.plugins.isEmpty()) return ""
    if (catalog == null) return plural(agent.plugins.size, "instance")
    val groups =
        catalog.connections
            .filter { it.id in agent.plugins }
            .groupBy { it.pluginId }
            .map { (pluginId, granted) ->
                val name = catalog.packages.find { it.id == pluginId }?.name ?: pluginId
                if (pluginId == "onepassword") "$name : ${plural(granted.size, "compte")}" else name
            }
    return groups.joinToString(" · ").ifBlank { plural(agent.plugins.size, "instance") }
}

fun sandboxLabel(value: String) =
    when (value) {
        "workspace-write" -> "Écriture limitée"
        "read-only" -> "Lecture seule"
        else -> "YOLO"
    }

val SANDBOX_CHOICES =
    listOf(
        Triple("yolo", "YOLO", "VM privée avec sudo et Docker."),
        Triple(
            "workspace-write",
            "Écriture limitée",
            "Écritures limitées aux espaces de travail des tâches, sans sudo.",
        ),
        Triple("read-only", "Lecture seule", "Lit les fichiers sans rien modifier."),
    )

/** "2 sur 4 · exclusivité sur 1", "Runner actuel seulement", "Toutes, y compris futures". */
fun nodesSummary(agent: Agent, nodes: List<ExecutionNode>): String {
    val access = agent.access
    val choices = agent.nodeChoices(nodes)
    val run =
        when {
            access.nodes == null -> "Toutes, y compris futures"
            access.nodes.isEmpty() -> "Aucune"
            access.nodes == listOf(LOCAL_NODE_ID) -> "Runner actuel seulement"
            else ->
                "${access.nodes.count { id -> choices.any { it.id == id } }} sur ${choices.size}"
        }
    val exclusive = agent.effectiveExclusive(nodes).size
    val summary = if (exclusive > 0) "$run · exclusivité sur $exclusive" else run
    return if (access.backend == NATIVE_BACKEND) "${backendLabel(access.backend)} · $summary"
    else summary
}

fun timeoutSummary(minutes: Int) =
    if (minutes == 0) "Sans limite de temps" else "Limite : $minutes min"

/** The short access summary on an agent card: "3 projets · 2 nodes · 1 plugin". */
fun agentAccessSummary(
    agent: Agent,
    catalog: PluginCatalog? = null,
    /** Known machines, to show exclusivity only where it takes effect; null (unknown) omits it. */
    nodes: List<ExecutionNode>? = null,
): String {
    val access = agent.access
    return buildList {
            add(access.projects?.let { plural(it.size, "projet") } ?: "Tous les projets")
            add(access.nodes?.let { plural(it.size, "node") } ?: "Toutes les nodes")
            if (access.backend == NATIVE_BACKEND) add(backendLabel(access.backend))
            if (agent.plugins.isNotEmpty())
                if (catalog == null) add(plural(agent.plugins.size, "plugin"))
                else
                    addAll(
                        agent.plugins
                            .map { id ->
                                val connection = catalog.connections.find { it.id == id }
                                catalog.packages.find { it.id == connection?.pluginId }?.name
                                    ?: "Plugin"
                            }
                            .distinct()
                    )
            if (
                // Only where it takes effect, so never while the machines are unknown.
                nodes != null && agent.effectiveExclusive(nodes).isNotEmpty()
            )
                add("Exclusivité")
            if (access.installation) add("Gestion de l’installation")
            if (access.delegation == true) add("Délégation de tâches")
        }
        .joinToString(" · ")
}

/** Something on the page that needs the owner's attention, and where to fix it. */
data class AgentIssue(val section: AgentSection, val text: String)

fun agentIssues(agent: Agent, resources: AgentResources): List<AgentIssue> = buildList {
    val access = agent.access
    if (agent.id != MAIN_AGENT_ID) {
        access.projects.orEmpty().forEach { id ->
            if (resources.projects.none { it.id == id })
                add(AgentIssue(AgentSection.Projects, "Un projet sélectionné n’existe plus."))
        }
        if (resources.skillsLoaded)
            access.skills.orEmpty().forEach { key ->
                if (resources.skills.none { skillKey(it) == key })
                    add(
                        AgentIssue(
                            AgentSection.Skills,
                            "La skill « ${key.substringAfter('/')} » n’existe plus.",
                        )
                    )
            }
        // Granted one by one or through « Toutes, y compris futures ».
        resources.mcps
            .filter { !it.enabled && (access.mcps == null || it.id in access.mcps) }
            .forEach { mcp ->
                add(
                    AgentIssue(
                        AgentSection.Mcp,
                        "La connexion MCP « ${mcp.name} » est désactivée sur l’installation. L’agent la garde, mais ne peut pas l’utiliser.",
                    )
                )
            }
    }
    if (resources.nodes.isNotEmpty()) {
        agent.unusableNodes(resources.nodes).forEach { held ->
            add(
                AgentIssue(
                    AgentSection.Nodes,
                    if (held.otherBackend)
                        "« ${held.name} » utilise un autre mode d’exécution : l’agent ne peut pas s’y exécuter."
                    else "Une node autorisée n’existe plus ou a été révoquée.",
                )
            )
        }
        val choices = agent.nodeChoices(resources.nodes)
        agent.exclusiveNodes.forEach { id ->
            val node = choices.find { it.id == id }
            when {
                // The current runner is always shown, even when the list omits it.
                node == null && id == LOCAL_NODE_ID -> Unit
                node == null ->
                    add(
                        AgentIssue(
                            AgentSection.Nodes,
                            "Une node réservable en exclusivité n’existe plus.",
                        )
                    )
                !access.runsOn(id) ->
                    add(
                        AgentIssue(
                            AgentSection.Nodes,
                            "Exclusivité inactive sur « ${node.name} » : l’agent ne peut pas s’y exécuter.",
                        )
                    )
                !node.exclusiveReservations ->
                    add(
                        AgentIssue(
                            AgentSection.Nodes,
                            "« ${node.name} » doit mettre à jour son runtime pour les réservations exclusives.",
                        )
                    )
            }
        }
    }
    resources.catalog?.let { catalog ->
        val unavailable = catalog.unavailableGrants(agent.plugins)
        agent.plugins.forEach { id ->
            val connection = catalog.connections.find { it.id == id }
            when {
                connection == null ->
                    add(
                        AgentIssue(
                            AgentSection.Plugins,
                            "Une instance de plugin accordée n’existe plus.",
                        )
                    )
                connection in unavailable ->
                    add(
                        AgentIssue(
                            AgentSection.Plugins,
                            "« ${connection.name} » n’est plus disponible pour les agents : l’accès est conservé, mais ne donne rien.",
                        )
                    )
                !connection.enabled ->
                    add(
                        AgentIssue(
                            AgentSection.Plugins,
                            "« ${connection.name} » est désactivé : l’agent garde l’accès, mais ne peut rien en utiliser.",
                        )
                    )
            }
        }
        catalog.connections
            .filter { agent.id.isNotEmpty() && it.agentId == agent.id }
            .filter { it.enabled && it.status.state == "needs_review" }
            .forEach {
                add(
                    AgentIssue(
                        AgentSection.Plugins,
                        "« ${it.name} » est suspendu jusqu’à sa reconfirmation.",
                    )
                )
            }
    }
}

fun issuesLabel(count: Int) = if (count == 1) "1 point à vérifier" else "$count points à vérifier"

enum class ReviewKind(val label: String) {
    Power("Pouvoir"),
    Access("Accès"),
    Reduction("Réduction"),
}

data class ReviewLine(val kind: ReviewKind, val text: String)

private val sandboxStrictness = mapOf("yolo" to 0, "workspace-write" to 1, "read-only" to 2)

private fun quoted(names: List<String>) = names.joinToString(", ") { "« $it »" }

/** What the draft removes from the saved access, in words. */
fun agentReductions(
    original: Agent,
    edited: Agent,
    resources: AgentResources,
): List<String> = buildList {
    val before = original.access
    val after = edited.access
    fun removed(
        old: List<String>?,
        new: List<String>?,
        noun: String,
        plural: String,
        name: (String) -> String,
        feminine: Boolean = false,
    ) {
        if (new == null) return
        if (old == null) {
            add(
                if (new.isEmpty()) "plus ${if (feminine) "aucune" else "aucun"} $noun"
                else "accès limité à ${plural(new.size, noun, plural)}"
            )
            return
        }
        val gone = old.filter { it !in new }
        if (gone.isNotEmpty())
            add("${if (gone.size == 1) noun else plural} ${quoted(gone.map(name))}")
    }
    removed(
        before.projects,
        after.projects,
        "projet",
        "projets",
        { id ->
            resources.projects.find { it.id == id }?.name ?: id
        },
    )
    removed(
        before.skills,
        after.skills,
        "skill",
        "skills",
        { key ->
            skillKeyLabel(key, resources.projects)
        },
        feminine = true,
    )
    removed(
        before.mcps,
        after.mcps,
        "connexion MCP",
        "connexions MCP",
        { id ->
            resources.mcps.find { it.id == id }?.name ?: id
        },
        feminine = true,
    )
    if (
        after.mcpTools.any { (id, tools) ->
            // A connection granted just now, or removed, is reported as such.
            val kept =
                (before.mcps == null || id in before.mcps) &&
                    (after.mcps == null || id in after.mcps)
            // A narrower choice, or a choice where every enabled tool was allowed.
            kept && before.mcpTools[id]?.let { previous -> !tools.containsAll(previous) } ?: true
        }
    )
        add("outils MCP restreints")
    val nodes = agentNodeChoices(resources.nodes)
    removed(
        before.nodes,
        after.nodes,
        "node",
        "nodes",
        { id ->
            nodes.find { it.id == id }?.name ?: id
        },
    )
    original.exclusiveNodes
        .filter { it !in edited.exclusiveNodes }
        .forEach { id ->
            add("réservation exclusive sur « ${nodes.find { it.id == id }?.name ?: id} »")
        }
    original.plugins
        .filter { it !in edited.plugins }
        .forEach { id -> add(pluginGrantLabel(id, resources.catalog)) }
    if (before.installation && !after.installation) add("gestion de l’installation")
    if (before.delegation == true && after.delegation != true) add("délégation de tâches")
    if (before.backend == NATIVE_BACKEND && after.backend != NATIVE_BACKEND) add("exécution native")
    if ((sandboxStrictness[after.sandbox] ?: 0) > (sandboxStrictness[before.sandbox] ?: 0))
        add("mode d’exécution « ${sandboxLabel(after.sandbox)} »")
}

/** "1Password « Production »": a granted plugin instance and its package. */
private fun pluginGrantLabel(id: String, catalog: PluginCatalog?): String {
    val connection = catalog?.connections?.find { it.id == id }
    val plugin =
        catalog?.packages?.find { it.id == connection?.pluginId }?.name
            ?: connection?.pluginId
            ?: "Plugin"
    return "$plugin « ${connection?.name ?: "instance supprimée"} »"
}

private fun runsSentence(runs: AgentPreviewRuns): String {
    val parts = buildList {
        if (runs.queued > 0)
            add(plural(runs.queued.toInt(), "exécution en file", "exécutions en file"))
        if (runs.running > 0)
            add(plural(runs.running.toInt(), "conversation en cours", "conversations en cours"))
    }
    val subject = parts.joinToString(" et ")
    val total = runs.queued + runs.running
    return "$subject ne ${if (total == 1L) "pourra" else "pourront"} pas continuer avec les anciens accès."
}

/** The review shown before saving: powers and secrets added, and work a reduction stops. */
fun Agent.nativeReviewNodes(resources: AgentResources): List<String> =
    if (access.backend != NATIVE_BACKEND || access.nodes == null) emptyList()
    else
        resources.nodes
            .filter {
                it.nativeModeRequest?.mode == "administrator" &&
                    it.id in access.nodes &&
                    id !in it.nativeReviewedAgents
            }
            .map { it.id }

fun agentReview(
    original: Agent,
    edited: Agent,
    resources: AgentResources,
    preview: AgentPreview?,
): List<ReviewLine> = buildList {
    val before = original.access
    val after = edited.access
    val nodes = agentNodeChoices(resources.nodes)
    edited.nativeReviewNodes(resources).forEach { id ->
        val name = resources.nodes.find { it.id == id }?.name ?: id
        add(
            ReviewLine(
                ReviewKind.Power,
                "Ajout : accès administrateur à $name. Toutes ses conversations admises auront sudo sans mot de passe sur l’hôte réel. Root peut lire les fichiers et identifiants locaux, modifier les services et contourner les limites coopératives. Le retour en protégé ne nettoie ni ne restaure la machine.",
            )
        )
    }
    if (after.backend == NATIVE_BACKEND && before.backend != NATIVE_BACKEND)
        add(
            ReviewLine(
                ReviewKind.Power,
                "Ajout : exécution native sur l’hôte. Cet agent partagera le GPU et les outils de l’hôte, avec une isolation plus faible qu’une VM.",
            )
        )
    edited.exclusiveNodes
        .filter { it !in original.exclusiveNodes }
        .forEach { id ->
            val name = nodes.find { it.id == id }?.name ?: id
            add(
                ReviewLine(
                    ReviewKind.Power,
                    "Ajout : réservation exclusive sur $name. Cet agent pourra suspendre les conversations des autres agents sur cette machine.",
                )
            )
        }
    if (after.installation && !before.installation)
        add(
            ReviewLine(
                ReviewKind.Power,
                "Ajout : gestion de l’installation. Cet agent pourra créer et modifier les agents (y compris leurs permissions), projets, skills, connexions MCP, missions et exécutions de toute l’installation.",
            )
        )
    if (after.delegation == true && before.delegation != true)
        add(
            ReviewLine(
                ReviewKind.Power,
                "Ajout : délégation de tâches. Cet agent pourra lancer des conversations qui lui rendent compte, sur tout agent de code et modèle, en tant que lui-même ou qu’un agent dont il couvre les accès.",
            )
        )
    edited.plugins
        .filter { it !in original.plugins }
        .forEach { id ->
            add(
                ReviewLine(
                    ReviewKind.Access,
                    "Ajout : ${pluginGrantLabel(id, resources.catalog)}, effectif immédiatement, y compris dans les conversations en cours.",
                )
            )
        }
    val reductions = agentReductions(original, edited, resources)
    val runs = preview?.runs ?: AgentPreviewRuns(queued = 0, running = 0)
    val affected = runs.queued + runs.running > 0
    if (reductions.isNotEmpty()) {
        val text = "Retrait : ${reductions.joinToString(", ")}."
        add(ReviewLine(ReviewKind.Reduction, if (affected) "$text ${runsSentence(runs)}" else text))
    } else if (affected)
        // The server reports work the change stops even when nothing listed here is removed.
        add(ReviewLine(ReviewKind.Reduction, "Accès modifié. ${runsSentence(runs)}"))
}

/** Delegations the save would suspend: these get a reapproval checkbox. */
fun AgentPreview.suspendedDelegations() = delegations.filter { it.suspended }

/**
 * Reapprovals checked when the review opens: only delegations this save suspends, that can be
 * reapproved and were not already suspended by an earlier, unrelated change.
 */
fun AgentPreview.preselectedReapprovals(): Set<String> =
    suspendedDelegations()
        .filter { it.blocked == null && !it.alreadySuspended }
        .map { it.id }
        .toSet()

/**
 * The review is needed for a power or a secret added, a reduction that stops work, or a delegation
 * this save would suspend. A delegation already suspended by an earlier change does not open it on
 * its own. Otherwise the page saves directly.
 */
fun needsReview(lines: List<ReviewLine>, preview: AgentPreview?): Boolean =
    lines.any { it.kind != ReviewKind.Reduction } ||
        (preview != null &&
            (preview.runs.queued + preview.runs.running > 0 ||
                preview.suspendedDelegations().any { !it.alreadySuspended }))

/**
 * Applies the owner's edits to the newest saved agent after someone else saved it: every setting
 * the owner did not change takes the newer value.
 */
fun rebaseAgent(
    base: Agent,
    mine: Agent,
    theirs: Agent,
    /** Connections that still exist: tool choices of the others are dropped. Null keeps all. */
    mcpIds: Set<String>? = null,
): Agent {
    fun <T> pick(old: T, edited: T, newer: T) = if (edited == old) newer else edited
    val a = base.access
    val m = mine.access
    val t = theirs.access
    val tools =
        mergeToolChoices(a.mcpTools, m.mcpTools, t.mcpTools).filterKeys {
            mcpIds == null || it in mcpIds
        }
    // Node lists only make sense for one backend: a side that switched backend wins them whole,
    // the owner first, so entries of the other backend are never merged in.
    val (nodes, exclusive) =
        when {
            m.backend != a.backend -> m.nodes to mine.exclusiveNodes
            t.backend != a.backend -> t.nodes to theirs.exclusiveNodes
            else ->
                mergeList(a.nodes, m.nodes, t.nodes) to
                    mergeList(base.exclusiveNodes, mine.exclusiveNodes, theirs.exclusiveNodes)
                        .orEmpty()
        }
    return theirs.copy(
        name = pick(base.name, mine.name, theirs.name),
        description = pick(base.description, mine.description, theirs.description),
        model = pick(base.model, mine.model, theirs.model),
        reasoning = pick(base.reasoning, mine.reasoning, theirs.reasoning),
        speed = pick(base.speed, mine.speed, theirs.speed),
        provider = pick(base.provider, mine.provider, theirs.provider),
        instructions = pick(base.instructions, mine.instructions, theirs.instructions),
        timeoutMinutes = pick(base.timeoutMinutes, mine.timeoutMinutes, theirs.timeoutMinutes),
        exclusiveNodes = exclusive,
        plugins = mergeList(base.plugins, mine.plugins, theirs.plugins).orEmpty(),
        access =
            AccessPolicy(
                nodes = nodes,
                projects = mergeList(a.projects, m.projects, t.projects),
                skills = mergeList(a.skills, m.skills, t.skills),
                mcps = mergeList(a.mcps, m.mcps, t.mcps),
                mcpTools = build.cairn.contracts.McpTools(tools),
                installation = pick(a.installation, m.installation, t.installation),
                delegation = pick(a.delegation, m.delegation, t.delegation),
                sandbox = pick(a.sandbox, m.sandbox, t.sandbox),
                backend = pick(a.backend, m.backend, t.backend),
            ),
    )
}

/**
 * A three-way merge of a selection (null means "all, including future"): untouched, it takes the
 * newer value; switching between all and a choice wins on whichever side made it, mine first;
 * otherwise the newer entries, without those I removed, plus those I added.
 */
fun mergeList(base: List<String>?, mine: List<String>?, theirs: List<String>?): List<String>? {
    if (mine == base) return theirs
    if (base == null || mine == null) return mine
    if (theirs == null) return null
    val removed = base.filter { it !in mine }
    val added = mine.filter { it !in base }
    return (theirs.filter { it !in removed } + added).distinct()
}

/** [mergeList] per MCP connection; an absent connection means every enabled tool. */
fun mergeToolChoices(
    base: Map<String, List<String>>,
    mine: Map<String, List<String>>,
    theirs: Map<String, List<String>>,
): Map<String, List<String>> =
    (base.keys + mine.keys + theirs.keys)
        .mapNotNull { id -> mergeList(base[id], mine[id], theirs[id])?.let { id to it } }
        .toMap()

/**
 * The draft rebased on the newest saved agent ([rebaseAgent]), including the time limit text: an
 * untouched limit takes the newer value, an edited one is kept.
 */
fun AgentDraft.rebased(base: Agent, theirs: Agent, mcpIds: Set<String>? = null): AgentDraft {
    val limitEdited = timeout.trim() != base.timeoutMinutes.toString()
    return copy(
        agent = rebaseAgent(base, agent, theirs, mcpIds),
        timeout = if (limitEdited) timeout else theirs.timeoutMinutes.toString(),
    )
}

/**
 * The body of `POST /agents`, `PUT /agents/{id}` and its preview. "All" selections are sent as
 * explicit nulls: an omitted key keeps the saved value on the server. The loaded revision is sent
 * back so a concurrent save is refused instead of overwritten.
 */
fun agentSaveBody(
    agent: Agent,
    reapprove: List<String> = emptyList(),
    /** MCP connections that exist; tool choices of deleted ones are not sent. Null keeps all. */
    mcpIds: Set<String>? = null,
    nativeNodeReviews: List<String> = emptyList(),
): JsonObject {
    val settings = agentSettings(agent, mcpIds)
    return build.cairn.contracts.contractJson
        .encodeToJsonElement(
            build.cairn.contracts.ReplaceAgentInput(
                agentId = agent.id,
                revision = agent.revision,
                settings = settings,
                review =
                    build.cairn.contracts.AgentReview(
                        nativeNodeReviews = nativeNodeReviews,
                        reapproveDelegations = reapprove,
                    ),
            )
        )
        .jsonObject
}

fun agentSettings(agent: Agent, mcpIds: Set<String>? = null): build.cairn.contracts.AgentSettings =
    build.cairn.contracts.AgentSettings(
        name = agent.name,
        provider = agent.provider,
        description = agent.description,
        model = agent.model,
        reasoning = agent.reasoning,
        instructions = agent.instructions,
        timeoutMinutes = agent.timeoutMinutes,
        access = agent.access.withGrantedTools(mcpIds),
        exclusiveNodes = agent.exclusiveNodes,
        plugins = agent.plugins,
    )

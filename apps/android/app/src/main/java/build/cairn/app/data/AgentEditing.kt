package build.cairn.app.data

import kotlinx.serialization.Serializable
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.encodeToJsonElement
import kotlinx.serialization.json.jsonObject

/*
 * The agent page: one draft for everything an agent can reach or do, its summaries, what needs
 * checking and what saving it changes. Pure functions so they can be tested without Compose.
 */

/** What saving an edited agent would interrupt (`POST /agents/{id}/preview`). */
@Serializable data class AgentPreviewRuns(val queued: Int = 0, val running: Int = 0)

/** A plugin delegation that answers as the agent, and how the new access affects it. */
@Serializable
data class AgentDelegationEffect(
    val id: String = "",
    val pluginId: String = "",
    val name: String = "",
    val enabled: Boolean = true,
    /** Saving without reapproval stops the delegation until it is reapproved. */
    val suspended: Boolean = false,
    /** Why the new access cannot be reapproved, or null. */
    val blocked: String? = null,
)

@Serializable
data class AgentPreview(
    val runs: AgentPreviewRuns = AgentPreviewRuns(),
    val delegations: List<AgentDelegationEffect> = emptyList(),
)

/** The sections of the agent page, in the order of its overview. */
enum class AgentSection(val title: String) {
    Projects("Projets"),
    Skills("Skills"),
    Mcp("MCP & outils"),
    Github("GitHub"),
    Plugins("Secrets & plugins"),
    Nodes("Nodes"),
    Sandbox("Mode d’exécution"),
    Powers("Pouvoirs spéciaux"),
    Identity("Identité & instructions"),
    Engine("Moteur & modèle"),
    Advanced("Avancé"),
}

/** Sections the server fixes for the main agent: always everything, including future ones. */
val MAIN_LOCKED_SECTIONS =
    setOf(AgentSection.Projects, AgentSection.Skills, AgentSection.Mcp, AgentSection.Github)

/** How the agent reaches GitHub. */
enum class GithubMode(val value: String, val label: String) {
    Shared("shared", "Connexion partagée"),
    Dedicated("dedicated", "Jeton dédié"),
    None("none", "Pas de GitHub"),
}

/**
 * The page's single draft. [timeout] keeps the field text so it can be validated; [github] is null
 * until the owner picks a GitHub option. The dedicated token itself is never part of the draft, so
 * it never reaches saved instance state.
 */
@Serializable
data class AgentDraft(
    val agent: Agent,
    val timeout: String = agent.timeoutMinutes.toString(),
    val github: String? = null,
)

/**
 * The draft of a new agent: it starts with no access and gains only what the owner grants. The
 * server's defaults for existing agents are unchanged.
 */
fun newAgent() =
    Agent(
        access =
            AccessPolicy(
                nodes = listOf(LOCAL_NODE_ID),
                projects = emptyList(),
                skills = emptyList(),
                mcps = emptyList(),
                mcpTools = emptyMap(),
                installation = false,
                github = false,
                sandbox = "yolo",
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
    /** Whether the agent has a dedicated GitHub token saved. */
    val tokenSaved: Boolean = false,
)

fun AgentDraft.githubMode(tokenSaved: Boolean): GithubMode =
    GithubMode.entries.find { it.value == github }
        ?: when {
            agent.access.github -> GithubMode.Shared
            tokenSaved -> GithubMode.Dedicated
            else -> GithubMode.None
        }

/** The agent as it will be sent: parsed time limit and the GitHub choice applied. */
fun AgentDraft.resolved(tokenSaved: Boolean): Agent {
    // Shared GitHub requires every project; restricting projects already turned it off.
    val shared =
        githubMode(tokenSaved) == GithubMode.Shared &&
            (agent.access.projects == null || agent.access.github)
    return agent.copy(
        timeoutMinutes = timeout.trim().toIntOrNull() ?: agent.timeoutMinutes,
        access = agent.access.copy(github = shared),
    )
}

/** What happens to the dedicated token when the page is saved: a new value, removal or nothing. */
fun AgentDraft.tokenChange(tokenSaved: Boolean, newToken: String): String? =
    when (githubMode(tokenSaved)) {
        GithubMode.Dedicated -> newToken.trim().takeIf { it.isNotEmpty() }
        GithubMode.None -> "".takeIf { tokenSaved }
        GithubMode.Shared -> null
    }

/**
 * Whether [edited] differs from the saved [base] in anything the agent save sends. Identity,
 * revision and portrait come from the server.
 */
fun profileChanged(base: Agent, edited: Agent): Boolean =
    edited.copy(id = base.id, revision = base.revision, avatar = base.avatar) != base

/**
 * The draft once its profile is saved: the saved agent, keeping the GitHub choice so a token that
 * failed to save stays pending and the next save only retries it, from the new revision.
 */
fun AgentDraft.afterProfileSaved(saved: Agent) = AgentDraft(saved, github = github)

/** The section of every unsaved change, once per changed setting. */
fun agentChanges(
    original: Agent,
    draft: AgentDraft,
    tokenSaved: Boolean,
    newToken: String,
): List<AgentSection> {
    val edited = draft.resolved(tokenSaved)
    val before = original.access
    val after = edited.access
    return buildList {
        if (edited.name != original.name) add(AgentSection.Identity)
        if (edited.description != original.description) add(AgentSection.Identity)
        if (edited.instructions != original.instructions) add(AgentSection.Identity)
        if (
            edited.provider != original.provider ||
                edited.model != original.model ||
                edited.reasoning != original.reasoning
        )
            add(AgentSection.Engine)
        if (draft.timeout.trim() != original.timeoutMinutes.toString()) add(AgentSection.Advanced)
        if (after.projects != before.projects) add(AgentSection.Projects)
        if (after.skills != before.skills) add(AgentSection.Skills)
        if (after.mcps != before.mcps || after.mcpTools != before.mcpTools) add(AgentSection.Mcp)
        if (after.github != before.github || draft.tokenChange(tokenSaved, newToken) != null)
            add(AgentSection.Github)
        if (edited.plugins.toSet() != original.plugins.toSet()) add(AgentSection.Plugins)
        if (after.nodes?.toSet() != before.nodes?.toSet()) add(AgentSection.Nodes)
        if (edited.exclusiveNodes.toSet() != original.exclusiveNodes.toSet())
            add(AgentSection.Nodes)
        if (after.sandbox != before.sandbox) add(AgentSection.Sandbox)
        if (after.installation != before.installation) add(AgentSection.Powers)
    }
}

fun changesLabel(count: Int) =
    if (count == 1) "1 modification non enregistrée" else "$count modifications non enregistrées"

/** Problems that prevent saving, shown in their section. */
fun agentErrors(
    draft: AgentDraft,
    tokenSaved: Boolean,
    newToken: String,
): Map<AgentSection, String> = buildMap {
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
            !Regex("(?:[a-z][a-z0-9_-]*)?").matches(agent.reasoning)
    )
        put(AgentSection.Engine, "Choisissez un modèle et un niveau de réflexion valides.")
    if (draft.timeout.trim().toIntOrNull() !in 0..720)
        put(AgentSection.Advanced, "Saisissez une limite entre 1 et 720 minutes.")
    if (draft.githubMode(tokenSaved) == GithubMode.Dedicated && !tokenSaved && newToken.isBlank())
        put(
            AgentSection.Github,
            "Saisissez le jeton dédié, ou choisissez une autre option GitHub.",
        )
}

/** Which section a server error belongs to, when its message names one. */
fun sectionOfError(message: String): AgentSection? {
    val text = message.lowercase()
    return when {
        "exclusive" in text || "node" in text -> AgentSection.Nodes
        "plugin" in text || "instance" in text || "delegation" in text -> AgentSection.Plugins
        "github" in text -> AgentSection.Github
        "mcp" in text || "tool" in text -> AgentSection.Mcp
        "skill" in text -> AgentSection.Skills
        "project" in text -> AgentSection.Projects
        "sandbox" in text -> AgentSection.Sandbox
        "installation" in text -> AgentSection.Powers
        "model" in text || "reasoning" in text || "provider" in text -> AgentSection.Engine
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

/** The node list of the agent page: the current runner first, then enrolled machines. */
fun agentNodeChoices(nodes: List<ExecutionNode>): List<ExecutionNode> {
    val local =
        nodes.find { it.local || it.id == LOCAL_NODE_ID }
            ?: ExecutionNode(
                LOCAL_NODE_ID,
                "Runner actuel",
                local = true,
                accepting = true,
                status = "online",
            )
    return listOf(local.copy(id = LOCAL_NODE_ID, name = "Runner actuel")) +
        nodes.filter { !it.local && it.id != LOCAL_NODE_ID && !it.revoked }
}

fun AccessPolicy.runsOn(node: String) = nodes == null || node in nodes

/** Exclusive grants that take effect: the agent may also run there and the runtime supports it. */
fun Agent.effectiveExclusive(nodes: List<ExecutionNode>): List<ExecutionNode> =
    agentNodeChoices(nodes).filter {
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
 * removed except future machines.
 */
fun AccessPolicy.withAllNodes(all: Boolean, nodes: List<ExecutionNode>): AccessPolicy =
    if (all) copy(nodes = null)
    else copy(nodes = this.nodes ?: agentNodeChoices(nodes).map { it.id })

fun Agent.withExclusive(node: String, granted: Boolean): Agent =
    copy(
        exclusiveNodes = if (granted) (exclusiveNodes + node).distinct() else exclusiveNodes - node
    )

/** Turning "all projects" off also turns the shared GitHub connection off. */
fun AccessPolicy.withAllProjects(all: Boolean, projects: List<Project>): AccessPolicy =
    if (all) copy(projects = null)
    else copy(projects = this.projects ?: projects.map { it.id }, github = false)

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

fun githubSummary(mode: GithubMode) = mode.label

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
    val choices = agentNodeChoices(nodes)
    val run =
        when {
            access.nodes == null -> "Toutes, y compris futures"
            access.nodes.isEmpty() -> "Aucune"
            access.nodes == listOf(LOCAL_NODE_ID) -> "Runner actuel seulement"
            else ->
                "${access.nodes.count { id -> choices.any { it.id == id } }} sur ${choices.size}"
        }
    val exclusive = agent.effectiveExclusive(nodes).size
    return if (exclusive > 0) "$run · exclusivité sur $exclusive" else run
}

fun timeoutSummary(minutes: Int) =
    if (minutes == 0) "Sans limite de temps" else "Limite : $minutes min"

/** The short access summary on an agent card: "3 projets · 2 nodes · 1 plugin". */
fun agentAccessSummary(agent: Agent): String {
    val access = agent.access
    return buildList {
            add(access.projects?.let { plural(it.size, "projet") } ?: "Tous les projets")
            add(access.nodes?.let { plural(it.size, "node") } ?: "Toutes les nodes")
            if (agent.plugins.isNotEmpty()) add(plural(agent.plugins.size, "plugin"))
            if (agent.exclusiveNodes.isNotEmpty()) add("Exclusivité")
            if (access.installation) add("Gestion de l’installation")
        }
        .joinToString(" · ")
}

/** Something on the page that needs the owner's attention, and where to fix it. */
data class AgentIssue(val section: AgentSection, val text: String)

fun agentIssues(agent: Agent, resources: AgentResources): List<AgentIssue> = buildList {
    val access = agent.access
    if (agent.id != MAIN_AGENT_ID) {
        access.mcps.orEmpty().forEach { id ->
            val mcp = resources.mcps.find { it.id == id }
            if (mcp != null && !mcp.enabled)
                add(
                    AgentIssue(
                        AgentSection.Mcp,
                        "La connexion MCP « ${mcp.name} » est désactivée sur l’installation. L’agent la garde, mais ne peut pas l’utiliser.",
                    )
                )
        }
        if (resources.skills.isNotEmpty())
            access.skills.orEmpty().forEach { key ->
                if (resources.skills.none { skillKey(it) == key })
                    add(
                        AgentIssue(
                            AgentSection.Skills,
                            "La skill « ${key.substringAfter('/')} » n’existe plus.",
                        )
                    )
            }
    }
    if (resources.nodes.isNotEmpty()) {
        val choices = agentNodeChoices(resources.nodes)
        agent.exclusiveNodes.forEach { id ->
            val node = choices.find { it.id == id }
            when {
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
fun agentReductions(original: Agent, edited: Agent, resources: AgentResources): List<String> =
    buildList {
        val before = original.access
        val after = edited.access
        fun removed(
            old: List<String>?,
            new: List<String>?,
            noun: String,
            plural: String,
            name: (String) -> String,
        ) {
            if (new == null) return
            if (old == null) {
                add("accès limité à ${plural(new.size, noun, plural)}")
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
        )
        removed(
            before.mcps,
            after.mcps,
            "connexion MCP",
            "connexions MCP",
            { id ->
                resources.mcps.find { it.id == id }?.name ?: id
            },
        )
        if (
            after.mcpTools.any { (id, tools) ->
                before.mcpTools[id]?.let { !it.containsAll(tools) } ?: true
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
        if (before.github && !after.github) add("connexion GitHub partagée")
        if ((sandboxStrictness[after.sandbox] ?: 0) > (sandboxStrictness[before.sandbox] ?: 0))
            add("mode d’exécution « ${sandboxLabel(after.sandbox)} »")
    }

private fun runsSentence(runs: AgentPreviewRuns): String {
    val parts = buildList {
        if (runs.queued > 0) add(plural(runs.queued, "exécution en file", "exécutions en file"))
        if (runs.running > 0)
            add(plural(runs.running, "conversation en cours", "conversations en cours"))
    }
    val subject = parts.joinToString(" et ")
    val total = runs.queued + runs.running
    return "$subject ne ${if (total == 1) "pourra" else "pourront"} pas continuer avec les anciens accès."
}

/** The review shown before saving: powers and secrets added, and work a reduction stops. */
fun agentReview(
    original: Agent,
    edited: Agent,
    resources: AgentResources,
    preview: AgentPreview?,
): List<ReviewLine> = buildList {
    val before = original.access
    val after = edited.access
    val nodes = agentNodeChoices(resources.nodes)
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
    edited.plugins
        .filter { it !in original.plugins }
        .forEach { id ->
            val connection = resources.catalog?.connections?.find { it.id == id }
            val plugin =
                resources.catalog?.packages?.find { it.id == connection?.pluginId }?.name
                    ?: connection?.pluginId
                    ?: "Plugin"
            add(
                ReviewLine(
                    ReviewKind.Access,
                    "Ajout : $plugin « ${connection?.name ?: id} », effectif immédiatement, y compris dans les conversations en cours.",
                )
            )
        }
    val reductions = agentReductions(original, edited, resources)
    val runs = preview?.runs ?: AgentPreviewRuns()
    if (reductions.isNotEmpty()) {
        val text = "Retrait : ${reductions.joinToString(", ")}."
        add(
            ReviewLine(
                ReviewKind.Reduction,
                if (runs.queued + runs.running > 0) "$text ${runsSentence(runs)}" else text,
            )
        )
    }
    if (before.github && !after.github && after.projects != null && before.projects == null)
        add(
            ReviewLine(
                ReviewKind.Reduction,
                "Connexion GitHub partagée désactivée : les projets choisis exigent un jeton dédié.",
            )
        )
}

/** Delegations the save would suspend: these get a reapproval checkbox. */
fun AgentPreview.suspendedDelegations() = delegations.filter { it.suspended }

/**
 * The review is needed for a power or a secret added, a reduction that stops work, or a delegation
 * it would suspend. Otherwise the page saves directly.
 */
fun needsReview(lines: List<ReviewLine>, preview: AgentPreview?): Boolean =
    lines.any { it.kind != ReviewKind.Reduction } ||
        (preview != null &&
            (preview.runs.queued + preview.runs.running > 0 ||
                preview.suspendedDelegations().isNotEmpty()))

/**
 * Applies the owner's edits to the newest saved agent after someone else saved it: every setting
 * the owner did not change takes the newer value.
 */
fun rebaseAgent(base: Agent, mine: Agent, theirs: Agent): Agent {
    fun <T> pick(old: T, edited: T, newer: T) = if (edited == old) newer else edited
    val a = base.access
    val m = mine.access
    val t = theirs.access
    return theirs.copy(
        name = pick(base.name, mine.name, theirs.name),
        description = pick(base.description, mine.description, theirs.description),
        model = pick(base.model, mine.model, theirs.model),
        reasoning = pick(base.reasoning, mine.reasoning, theirs.reasoning),
        provider = pick(base.provider, mine.provider, theirs.provider),
        instructions = pick(base.instructions, mine.instructions, theirs.instructions),
        timeoutMinutes = pick(base.timeoutMinutes, mine.timeoutMinutes, theirs.timeoutMinutes),
        exclusiveNodes = pick(base.exclusiveNodes, mine.exclusiveNodes, theirs.exclusiveNodes),
        plugins = pick(base.plugins, mine.plugins, theirs.plugins),
        access =
            AccessPolicy(
                nodes = pick(a.nodes, m.nodes, t.nodes),
                projects = pick(a.projects, m.projects, t.projects),
                skills = pick(a.skills, m.skills, t.skills),
                mcps = pick(a.mcps, m.mcps, t.mcps),
                mcpTools = pick(a.mcpTools, m.mcpTools, t.mcpTools),
                installation = pick(a.installation, m.installation, t.installation),
                github = pick(a.github, m.github, t.github),
                sandbox = pick(a.sandbox, m.sandbox, t.sandbox),
            ),
    )
}

/**
 * The draft rebased on the newest saved agent ([rebaseAgent]), including the time limit text: an
 * untouched limit takes the newer value, an edited one is kept. The GitHub choice is kept.
 */
fun AgentDraft.rebased(base: Agent, theirs: Agent): AgentDraft {
    val limitEdited = timeout.trim() != base.timeoutMinutes.toString()
    return copy(
        agent = rebaseAgent(base, agent, theirs),
        timeout = if (limitEdited) timeout else theirs.timeoutMinutes.toString(),
    )
}

/**
 * The body of `POST /agents`, `PUT /agents/{id}` and its preview. "All" selections are sent as
 * explicit nulls: an omitted key keeps the saved value on the server. The loaded revision is sent
 * back so a concurrent save is refused instead of overwritten.
 */
fun agentSaveBody(agent: Agent, reapprove: List<String> = emptyList()): JsonObject {
    val fields = wireJson.encodeToJsonElement(agent).jsonObject.toMutableMap()
    fields.remove("avatar")
    fields.remove("revision")
    if (agent.revision.isNotBlank()) fields["revision"] = JsonPrimitive(agent.revision)
    fun list(value: List<String>?): JsonElement =
        value?.let { JsonArray(it.map(::JsonPrimitive)) } ?: JsonNull
    val access = fields["access"]!!.jsonObject.toMutableMap()
    access["nodes"] = list(agent.access.nodes)
    access["projects"] = list(agent.access.projects)
    access["skills"] = list(agent.access.skills)
    access["mcps"] = list(agent.access.mcps)
    fields["access"] = JsonObject(access)
    if (reapprove.isNotEmpty()) fields["reapproveDelegations"] = list(reapprove)
    return JsonObject(fields)
}
